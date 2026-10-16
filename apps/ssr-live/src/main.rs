#![forbid(unsafe_code)]

//! ssr-live: development tool for the SpatialAudioLab test PC (ADR 0018).
//!
//! * `record` writes the SAL ring's record stream to a dump file, the input
//!   the MAT decoder and the black-box goldens are built from (ADR 0005).
//! * `play` runs SAL ring → engine → WASAPI with a BRIR profile, printing
//!   latency, crossing-ring level, underruns and clock correction every
//!   second; `--record` keeps a dump of the same stream. `--from-dump` plays
//!   a dump instead of the ring (`--loop` repeats it) and `--report` writes
//!   the run's figures as JSON, which is how CI soaks the live path.
//! * `devices` lists render endpoints.
//! * `host` drives the supervisor's Windows host adapter
//!   (`ssr_sys::WindowsHost`) and prints one JSON document per call (shapes
//!   in `host_json`): `show` the platform, driver, endpoints and default
//!   roles, `set-default` one role, `setup` one elevated `ssr-setup` task.
//!   CI checks the adapter with it (`tools/ci/host_check.py`,
//!   `tools/ci/host_role_check.py`).
//!
//! The SAL device opens only from an administrator prompt.

#[cfg_attr(not(windows), allow(dead_code))]
mod host_json;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{CommandFactory, FromArgMatches, Parser, Subcommand, ValueEnum};
use ssr_types::app::Language;
use ssr_types::{Role, SetupTask};

#[derive(Parser)]
#[command(
    version,
    about = "Spatial SoftReceiver live tool for the SAL development PC"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum Clock {
    Auto,
    Asrc,
    Trim,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum CaptureKind {
    Driver,
    Sal,
}

/// What only the command line knows for `--report` (source kind, requested
/// clock and output mode, direct layouts).
#[cfg_attr(not(any(windows, test)), allow(dead_code))]
fn request_fields(
    source: CaptureKind,
    from_dump: bool,
    clock: Clock,
    shared: bool,
    direct_layouts: Vec<supervisor::DirectLayoutField>,
) -> supervisor::RequestFields {
    supervisor::RequestFields {
        source: if from_dump {
            "dump"
        } else {
            match source {
                CaptureKind::Driver => "driver",
                CaptureKind::Sal => "sal",
            }
        }
        .to_owned(),
        clock_requested: match clock {
            Clock::Auto => "auto",
            Clock::Asrc => "asrc",
            Clock::Trim => "trim",
        }
        .to_owned(),
        output_mode_requested: if shared { "shared" } else { "exclusive" }.to_owned(),
        direct_layouts,
    }
}

#[cfg_attr(not(any(windows, test)), allow(dead_code))]
fn parse_endpoints(value: Option<&str>) -> Result<engine::EndpointFilter, String> {
    let Some(value) = value else {
        return Ok(engine::EndpointFilter::All);
    };
    let mut mask = 0_u32;
    let mut count = 0;
    for part in value.split(',') {
        let part = part.trim();
        let endpoint = part.parse::<u16>().map_err(|_| {
            ssr_i18n::live::invalid_endpoint(ssr_i18n::cli_language(), format_args!("{part:?}"))
        })?;
        if endpoint as usize >= ssr_abi::MAX_ENDPOINTS {
            return Err(ssr_i18n::live::endpoint_out_of_range(
                ssr_i18n::cli_language(),
                endpoint,
                ssr_abi::MAX_ENDPOINTS - 1,
            ));
        }
        let bit = 1_u32 << endpoint;
        if mask & bit != 0 {
            return Err(ssr_i18n::live::endpoint_duplicate(
                ssr_i18n::cli_language(),
                endpoint,
            ));
        }
        mask |= bit;
        count += 1;
    }
    if count == 0 {
        return Err(ssr_i18n::live::endpoints_empty(ssr_i18n::cli_language()).to_owned());
    }
    Ok(engine::EndpointFilter::Mask(mask))
}

#[cfg(test)]
mod endpoint_tests {
    use super::*;

    #[test]
    fn english_help_is_the_derived_help_and_korean_help_is_complete() {
        ssr_i18n::clap_help::verify(Cli::command(), localized_command);
    }

    #[test]
    fn report_request_fields_cover_sources_clocks_and_output_modes() {
        let report = supervisor::SessionReport {
            seconds: 1.0,
            engine: engine::StopReport {
                final_status: ssr_types::EngineStatus {
                    output_rate: 44_100,
                    trim_available: true,
                    ..Default::default()
                },
                ..Default::default()
            },
            figures: supervisor::RunFigures::default(),
            output_kind: engine::OutputKind::Speakers,
            output_channels: 2,
            output_channel_mask: 3,
            recorder_lost_bytes: 0,
            driver: None,
            driver_stats_error: None,
        };
        for (source, from_dump, expected) in [
            (CaptureKind::Driver, false, "driver"),
            (CaptureKind::Sal, false, "sal"),
            (CaptureKind::Driver, true, "dump"),
        ] {
            for (clock, clock_name) in [
                (Clock::Auto, "auto"),
                (Clock::Asrc, "asrc"),
                (Clock::Trim, "trim"),
            ] {
                for (shared, mode) in [(false, "exclusive"), (true, "shared")] {
                    let fields = report.to_json(&request_fields(
                        source,
                        from_dump,
                        clock,
                        shared,
                        Vec::new(),
                    ));
                    assert_eq!(fields["source"], expected);
                    assert_eq!(fields["clock_requested"], clock_name);
                    assert_eq!(fields["output_mode_requested"], mode);
                    assert_eq!(fields["output_sample_rate"], 44_100);
                    assert_eq!(fields["trim_available"], true);
                    assert_eq!(fields["direct_layouts"], serde_json::json!([]));
                }
            }
        }
    }

    #[test]
    fn endpoint_parser_accepts_arbitrary_subsets_and_rejects_bad_masks() {
        let subset = parse_endpoints(Some("0,2")).unwrap();
        assert!(subset.includes(0));
        assert!(!subset.includes(1));
        assert!(subset.includes(2));
        assert!(!subset.includes(3));
        assert!(parse_endpoints(Some("4")).is_err());
        assert!(parse_endpoints(Some("2,2")).is_err());
        assert!(parse_endpoints(Some("")).is_err());
    }

    #[test]
    fn play_direct_layouts_are_repeatable_and_record_rejects_them() {
        let cli = Cli::try_parse_from([
            "ssr-live",
            "play",
            "--profile",
            "profile.toml",
            "--direct-layout",
            "2=L,R",
            "--direct-layout",
            "12=7.1.4",
        ])
        .unwrap();
        let Command::Play { direct_layouts, .. } = cli.command else {
            panic!("expected play command");
        };
        assert_eq!(direct_layouts.len(), 2);
        assert_eq!(direct_layouts[0].channels, 2);
        assert_eq!(direct_layouts[1].channels, 12);

        assert!(
            Cli::try_parse_from([
                "ssr-live",
                "record",
                "--out",
                "capture.ssrdump",
                "--direct-layout",
                "2=stereo",
            ])
            .is_err()
        );
        assert!(
            Cli::try_parse_from([
                "ssr-live",
                "play",
                "--profile",
                "profile.toml",
                "--direct-layout",
                "2=L,L",
            ])
            .is_err()
        );
    }
}

/// `ssr-setup.exe`, beside `ssr-live.exe` or in the package directory.
const SETUP_EXE: &str = "ssr-setup.exe";

/// The state directory under the temporary directory when `--state-dir` is
/// not given.
const HOST_STATE_DIR: &str = "ssr-live-host";

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum RoleArg {
    Console,
    Multimedia,
    Communications,
}

impl From<RoleArg> for Role {
    fn from(role: RoleArg) -> Self {
        match role {
            RoleArg::Console => Role::Console,
            RoleArg::Multimedia => Role::Multimedia,
            RoleArg::Communications => Role::Communications,
        }
    }
}

/// The `ssr-setup` tasks, in the order `host setup --help` lists them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum SetupKind {
    /// Trust the package's test certificate, create the root device, install
    /// the driver and apply the endpoints' exclusive-mode policy.
    Install,
    /// Remove the current and legacy devices, driver packages, orphan services
    /// and test certificates.
    Uninstall,
    /// Reinstall the driver on the device and apply the exclusive-mode policy,
    /// or install when there is no device.
    Repair,
    /// Disable and enable the device.
    RestartDevice,
    /// Set the driver's endpoint mask (--endpoint-mask), restart the device
    /// and apply the exclusive-mode policy to the endpoints it enables.
    SetFormats,
}

#[derive(Subcommand, Debug)]
enum HostCommand {
    /// Print the platform, the driver state, every render endpoint (ours with
    /// its endpoint number) and the default device of each role.
    Show {
        /// Directory for ssr-setup's result file and the restore point
        /// [default: %TEMP%\ssr-live-host].
        #[arg(long)]
        state_dir: Option<PathBuf>,
    },
    /// Make a render endpoint the default device of one role.
    SetDefault {
        /// The role whose default device changes.
        #[arg(long, value_enum)]
        role: RoleArg,
        /// The endpoint id, as `host show` prints it.
        #[arg(long)]
        id: String,
        /// Directory for ssr-setup's result file and the restore point
        /// [default: %TEMP%\ssr-live-host].
        #[arg(long)]
        state_dir: Option<PathBuf>,
    },
    /// Run one administrator task through an elevated ssr-setup (one UAC
    /// prompt).
    ///
    /// ssr-setup.exe runs from --package-dir when that directory holds one, and
    /// otherwise from beside ssr-live.exe; install and repair take
    /// spatialsoftreceiver.inf and spatialsoftreceiver-test.cer from the
    /// directory of the ssr-setup.exe that runs. ssr-live copies nothing: put
    /// ssr-setup.exe into the package directory first (the CI step copies the
    /// binaries into the kit directory before it calls this).
    Setup {
        /// Administrator task to run.
        #[arg(value_enum)]
        task: SetupKind,
        /// With set-formats: the driver's EndpointMask, bit n enables endpoint n
        /// (1-15).
        #[arg(long, value_parser = clap::value_parser!(u8).range(1..16))]
        endpoint_mask: Option<u8>,
        /// The driver package directory (spatialsoftreceiver.inf, its catalog
        /// and driver, the test certificate, and ssr-setup.exe).
        #[arg(long)]
        package_dir: PathBuf,
        /// Directory for ssr-setup's result file and the restore point
        /// [default: %TEMP%\ssr-live-host].
        #[arg(long)]
        state_dir: Option<PathBuf>,
    },
}

/// The task `host setup` asks for; `--endpoint-mask` belongs to set-formats
/// and set-formats needs it.
fn setup_task(kind: SetupKind, endpoint_mask: Option<u8>) -> Result<SetupTask, String> {
    match (kind, endpoint_mask) {
        (SetupKind::SetFormats, Some(endpoint_mask)) => Ok(SetupTask::SetFormats { endpoint_mask }),
        (SetupKind::SetFormats, None) => {
            Err(ssr_i18n::live::endpoint_mask_required(ssr_i18n::cli_language()).to_owned())
        }
        (_, Some(_)) => {
            Err(ssr_i18n::live::endpoint_mask_only(ssr_i18n::cli_language()).to_owned())
        }
        (SetupKind::Install, None) => Ok(SetupTask::Install),
        (SetupKind::Uninstall, None) => Ok(SetupTask::Uninstall),
        (SetupKind::Repair, None) => Ok(SetupTask::Repair),
        (SetupKind::RestartDevice, None) => Ok(SetupTask::RestartDevice),
    }
}

/// `--state-dir`, or `ssr-live-host` under the temporary directory.
#[cfg_attr(not(any(windows, test)), allow(dead_code))]
fn host_state_dir(state_dir: Option<PathBuf>, temp_dir: &Path) -> PathBuf {
    state_dir.unwrap_or_else(|| temp_dir.join(HOST_STATE_DIR))
}

/// The `ssr-setup.exe` that `host setup` runs: the package directory's when it
/// holds one, else `sibling` (the one beside ssr-live.exe). The adapter takes
/// the package files from the chosen executable's directory.
#[cfg_attr(not(any(windows, test)), allow(dead_code))]
fn package_setup_exe(
    package_dir: &Path,
    sibling: PathBuf,
    is_file: impl Fn(&Path) -> bool,
) -> PathBuf {
    let packaged = package_dir.join(SETUP_EXE);
    if is_file(&packaged) {
        packaged
    } else {
        sibling
    }
}

#[cfg(test)]
mod host_tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<HostCommand, clap::Error> {
        let mut line = vec!["ssr-live", "host"];
        line.extend_from_slice(args);
        let cli = Cli::try_parse_from(line)?;
        let Command::Host { command } = cli.command else {
            panic!("expected the host command");
        };
        Ok(command)
    }

    #[test]
    fn host_subcommands_parse_their_documented_lines() {
        let HostCommand::Show { state_dir } = parse(&["show"]).unwrap() else {
            panic!("expected show");
        };
        assert_eq!(state_dir, None);
        let HostCommand::Show { state_dir } = parse(&["show", "--state-dir", "s"]).unwrap() else {
            panic!("expected show");
        };
        assert_eq!(state_dir, Some(PathBuf::from("s")));

        for (name, role) in [
            ("console", Role::Console),
            ("multimedia", Role::Multimedia),
            ("communications", Role::Communications),
        ] {
            let HostCommand::SetDefault {
                role: parsed, id, ..
            } = parse(&[
                "set-default",
                "--role",
                name,
                "--id",
                "{0.0.0.00000000}.{a}",
            ])
            .unwrap()
            else {
                panic!("expected set-default");
            };
            assert_eq!(Role::from(parsed), role);
            assert_eq!(id, "{0.0.0.00000000}.{a}");
        }
        assert!(parse(&["set-default", "--role", "default", "--id", "x"]).is_err());
        assert!(parse(&["set-default", "--role", "console"]).is_err());

        for (name, kind) in [
            ("install", SetupKind::Install),
            ("uninstall", SetupKind::Uninstall),
            ("repair", SetupKind::Repair),
            ("restart-device", SetupKind::RestartDevice),
            ("set-formats", SetupKind::SetFormats),
        ] {
            let HostCommand::Setup {
                task,
                endpoint_mask,
                package_dir,
                state_dir,
            } = parse(&["setup", name, "--package-dir", "kit"]).unwrap()
            else {
                panic!("expected setup");
            };
            assert_eq!((task, endpoint_mask), (kind, None));
            assert_eq!(package_dir, PathBuf::from("kit"));
            assert_eq!(state_dir, None);
        }
        assert!(parse(&["setup", "install"]).is_err());
        assert!(parse(&["setup", "reinstall", "--package-dir", "kit"]).is_err());
        for mask in ["0", "16", "x"] {
            assert!(
                parse(&[
                    "setup",
                    "set-formats",
                    "--endpoint-mask",
                    mask,
                    "--package-dir",
                    "kit"
                ])
                .is_err()
            );
        }
        let HostCommand::Setup { endpoint_mask, .. } = parse(&[
            "setup",
            "set-formats",
            "--endpoint-mask",
            "7",
            "--package-dir",
            "kit",
        ])
        .unwrap() else {
            panic!("expected setup");
        };
        assert_eq!(endpoint_mask, Some(7));
    }

    #[test]
    fn setup_task_needs_the_mask_for_set_formats_only() {
        assert_eq!(
            setup_task(SetupKind::SetFormats, Some(5)),
            Ok(SetupTask::SetFormats { endpoint_mask: 5 })
        );
        assert!(setup_task(SetupKind::SetFormats, None).is_err());
        for (kind, task) in [
            (SetupKind::Install, SetupTask::Install),
            (SetupKind::Uninstall, SetupTask::Uninstall),
            (SetupKind::Repair, SetupTask::Repair),
            (SetupKind::RestartDevice, SetupTask::RestartDevice),
        ] {
            assert_eq!(setup_task(kind, None), Ok(task));
            assert!(setup_task(kind, Some(7)).is_err());
        }
    }

    #[test]
    fn state_dir_defaults_under_the_temporary_directory() {
        let temp = Path::new("tmp");
        assert_eq!(host_state_dir(None, temp), temp.join("ssr-live-host"));
        assert_eq!(
            host_state_dir(Some(PathBuf::from("mine")), temp),
            PathBuf::from("mine")
        );
    }

    #[test]
    fn setup_exe_comes_from_the_package_dir_when_it_holds_one() {
        let kit = Path::new("kit");
        let sibling = PathBuf::from("bin").join(SETUP_EXE);
        assert_eq!(
            package_setup_exe(kit, sibling.clone(), |path| path == kit.join(SETUP_EXE)),
            kit.join(SETUP_EXE)
        );
        assert_eq!(package_setup_exe(kit, sibling.clone(), |_| false), sibling);
    }
}

#[derive(Subcommand)]
#[allow(clippy::large_enum_variant)]
enum Command {
    /// Record a capture ring (the Spatial SoftReceiver driver or SAL) to a dump file (run as administrator).
    Record {
        /// Capture source; the Spatial SoftReceiver driver is the normal source.
        #[arg(long, value_enum, default_value_t = CaptureKind::Driver)]
        source: CaptureKind,
        /// Output file (`.ssrdump`).
        #[arg(long)]
        out: PathBuf,
        /// Seconds to record.
        #[arg(long, default_value_t = 60.0)]
        seconds: f64,
    },
    /// Play a capture ring through a BRIR profile (run as administrator).
    Play {
        /// Capture source; ignored when --from-dump supplies the source.
        #[arg(long, value_enum, default_value_t = CaptureKind::Driver)]
        source: CaptureKind,
        /// Endpoints to consume: all exposed endpoints, or a comma-separated subset.
        #[arg(long)]
        endpoints: Option<String>,
        /// Profile TOML (see crates/brir).
        #[arg(long)]
        profile: PathBuf,
        /// Part of the output device's name; the default device when omitted.
        #[arg(long)]
        output: Option<String>,
        /// Use WASAPI shared mode instead of exclusive.
        #[arg(long)]
        shared: bool,
        /// Permit an exclusive request to fall back to WASAPI shared mode.
        #[arg(long, conflicts_with = "shared")]
        allow_shared_fallback: bool,
        /// Device sample rate.
        #[arg(long, default_value_t = 48_000)]
        rate: u32,
        /// Device period in milliseconds; the device's minimum when omitted.
        #[arg(long)]
        period_ms: Option<f64>,
        /// Crossing-ring level to hold, in milliseconds (10 for games, 40 for music).
        #[arg(long, default_value_t = 10.0)]
        target_ms: f32,
        /// Clock-control mode.
        #[arg(long, value_enum, default_value_t = Clock::Auto)]
        clock: Clock,
        /// Also write the captured record stream to this dump file.
        #[arg(long)]
        record: Option<PathBuf>,
        /// Stop after this many seconds (otherwise press Enter to stop).
        #[arg(long)]
        seconds: Option<f64>,
        /// Play a dump file at its recorded pace instead of the SAL ring, to run
        /// the live path on a PC without SAL. Stops when the dump ends.
        #[arg(long)]
        from_dump: Option<PathBuf>,
        /// With --from-dump: repeat the dump's data records, with continuous
        /// positions and times, until --seconds or Enter stops the run.
        #[arg(long = "loop", requires = "from_dump")]
        repeat: bool,
        /// Run everything but send silence to the device.
        #[arg(long)]
        mute: bool,
        /// Write the run's figures (underruns, restarts, faults, latency,
        /// ring trough, clock) as JSON to this file when it stops.
        #[arg(long)]
        report: Option<PathBuf>,
        /// Override a zero-mask PCM order, as CHANNELS=PRESET or CHANNELS=SPEAKER,SPEAKER,...
        #[arg(long = "direct-layout")]
        direct_layouts: Vec<engine::DirectLayoutOverride>,
    },
    /// List render endpoints (name, default flag, id).
    Devices {
        /// Include shared mix format and standard-layout exclusive support.
        #[arg(long)]
        formats: bool,
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Query and drive the Windows host adapter; prints one JSON document.
    Host {
        #[command(subcommand)]
        command: HostCommand,
    },
}

fn localized_command(language: Language) -> clap::Command {
    use ssr_i18n::live;

    let mut command = Cli::command().about(live::about(language));
    for (name, about) in [
        ("record", live::record_about(language)),
        ("play", live::play_about(language)),
        ("devices", live::devices_about(language)),
        ("host", live::host_about(language)),
    ] {
        command = command.mut_subcommand(name, |subcommand| subcommand.about(about));
    }
    command = command
        .mut_subcommand("record", |subcommand| {
            subcommand
                .mut_arg("source", |arg| arg.help(live::record_source_help(language)))
                .mut_arg("out", |arg| arg.help(live::record_out_help(language)))
                .mut_arg("seconds", |arg| {
                    arg.help(live::record_seconds_help(language))
                })
        })
        .mut_subcommand("play", |subcommand| {
            subcommand
                .mut_arg("source", |arg| arg.help(live::play_source_help(language)))
                .mut_arg("endpoints", |arg| {
                    arg.help(live::play_endpoints_help(language))
                })
                .mut_arg("profile", |arg| arg.help(live::play_profile_help(language)))
                .mut_arg("output", |arg| arg.help(live::play_output_help(language)))
                .mut_arg("shared", |arg| arg.help(live::play_shared_help(language)))
                .mut_arg("allow_shared_fallback", |arg| {
                    arg.help(live::play_allow_shared_fallback_help(language))
                })
                .mut_arg("rate", |arg| arg.help(live::play_rate_help(language)))
                .mut_arg("period_ms", |arg| {
                    arg.help(live::play_period_ms_help(language))
                })
                .mut_arg("target_ms", |arg| {
                    arg.help(live::play_target_ms_help(language))
                })
                .mut_arg("clock", |arg| arg.help(live::play_clock_help(language)))
                .mut_arg("record", |arg| arg.help(live::play_record_help(language)))
                .mut_arg("seconds", |arg| arg.help(live::play_seconds_help(language)))
                .mut_arg("from_dump", |arg| {
                    arg.help(live::play_from_dump_help(language))
                })
                .mut_arg("repeat", |arg| arg.help(live::play_loop_help(language)))
                .mut_arg("mute", |arg| arg.help(live::play_mute_help(language)))
                .mut_arg("report", |arg| arg.help(live::play_report_help(language)))
                .mut_arg("direct_layouts", |arg| {
                    arg.help(live::play_direct_layout_help(language))
                })
        })
        .mut_subcommand("devices", |subcommand| {
            subcommand
                .mut_arg("formats", |arg| {
                    arg.help(live::devices_formats_help(language))
                })
                .mut_arg("json", |arg| arg.help(live::devices_json_help(language)))
        })
        .mut_subcommand("host", |mut subcommand| {
            for (name, about) in [
                ("show", live::host_show_about(language)),
                ("set-default", live::host_set_default_about(language)),
                ("setup", live::host_setup_about(language)),
            ] {
                subcommand = subcommand.mut_subcommand(name, |nested| nested.about(about));
            }
            subcommand
                .mut_subcommand("show", |nested| {
                    nested.mut_arg("state_dir", |arg| {
                        arg.help(live::host_state_dir_help(language))
                    })
                })
                .mut_subcommand("set-default", |nested| {
                    nested
                        .mut_arg("role", |arg| arg.help(live::host_role_help(language)))
                        .mut_arg("id", |arg| arg.help(live::host_id_help(language)))
                        .mut_arg("state_dir", |arg| {
                            arg.help(live::host_state_dir_help(language))
                        })
                })
                .mut_subcommand("setup", |nested| {
                    nested
                        .long_about(live::host_setup_long_about(language))
                        .mut_arg("task", |arg| {
                            arg.help(live::host_setup_task_help(language)).value_parser(
                                ssr_i18n::clap_help::Values::new(|task: &SetupKind| match task {
                                    SetupKind::Install => live::host_setup_task_install(language),
                                    SetupKind::Uninstall => {
                                        live::host_setup_task_uninstall(language)
                                    }
                                    SetupKind::Repair => ssr_i18n::setup::repair_about(language),
                                    SetupKind::RestartDevice => {
                                        ssr_i18n::setup::restart_device_about(language)
                                    }
                                    SetupKind::SetFormats => {
                                        live::host_setup_task_set_formats(language)
                                    }
                                }),
                            )
                        })
                        .mut_arg("endpoint_mask", |arg| {
                            arg.help(live::host_endpoint_mask_help(language))
                        })
                        .mut_arg("package_dir", |arg| {
                            arg.help(live::host_package_dir_help(language))
                        })
                        .mut_arg("state_dir", |arg| {
                            arg.help(live::host_state_dir_help(language))
                        })
                })
        });
    ssr_i18n::clap_help::localize(command, language)
}

fn parse_cli(language: Language) -> Result<Cli, clap::Error> {
    let matches = localized_command(language).try_get_matches()?;
    Cli::from_arg_matches(&matches)
}

fn parse_error_exit(error: clap::Error, language: Language) -> ExitCode {
    if matches!(
        error.kind(),
        clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion
    ) {
        let _ = error.print();
        ExitCode::SUCCESS
    } else {
        eprintln!("{}", ssr_i18n::cli::parse_error(language));
        let _ = error.print();
        ExitCode::from(2)
    }
}

fn main() -> ExitCode {
    let language = ssr_i18n::cli_language();
    let cli = match parse_cli(language) {
        Ok(cli) => cli,
        Err(error) => return parse_error_exit(error, language),
    };
    let result = match cli.command {
        Command::Record {
            source,
            out,
            seconds,
        } => platform::record(source, &out, seconds),
        Command::Play {
            source,
            endpoints,
            profile,
            output,
            shared,
            allow_shared_fallback,
            rate,
            period_ms,
            target_ms,
            clock,
            record,
            seconds,
            from_dump,
            repeat,
            mute,
            report,
            direct_layouts,
        } => platform::play(platform::PlayOptions {
            source,
            endpoints,
            profile,
            output,
            shared,
            allow_shared_fallback,
            rate,
            period_ms,
            target_ms,
            clock,
            record,
            seconds,
            from_dump,
            repeat,
            mute,
            report,
            direct_layouts,
        }),
        Command::Devices { formats, json } => platform::devices(formats, json),
        Command::Host { command } => match command {
            HostCommand::Show { state_dir } => platform::host_show(state_dir),
            HostCommand::SetDefault {
                role,
                id,
                state_dir,
            } => platform::host_set_default(role.into(), id, state_dir),
            HostCommand::Setup {
                task,
                endpoint_mask,
                package_dir,
                state_dir,
            } => setup_task(task, endpoint_mask)
                .and_then(|task| platform::host_setup(&task, &package_dir, state_dir)),
        },
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("{}", ssr_i18n::live::error_prefix(language, message));
            ExitCode::FAILURE
        }
    }
}

#[cfg(windows)]
mod platform {
    use std::collections::BTreeMap;
    use std::io::BufRead;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread;
    use std::time::{Duration, Instant};

    use engine::adapters::DumpCapture;
    use engine::{ClockPreference, EngineConfig};
    #[cfg(test)]
    use ssr_abi::dump_header_bytes;
    use ssr_abi::{Record, records};
    use ssr_sys::driver_ring::DriverRing;
    use ssr_sys::mmcss;
    use ssr_sys::output::WasapiOpener;
    use ssr_sys::{WindowsHost, devices, qpc, sal::SalRing};
    use ssr_types::{
        CaptureError, CaptureSource, DeviceId, Host, Role, SetupTask, TrimUnsupported,
    };

    use super::{
        CaptureKind, Clock, SETUP_EXE, host_json, host_state_dir, package_setup_exe,
        parse_endpoints,
    };

    pub struct PlayOptions {
        pub source: CaptureKind,
        pub endpoints: Option<String>,
        pub profile: PathBuf,
        pub output: Option<String>,
        pub shared: bool,
        pub allow_shared_fallback: bool,
        pub rate: u32,
        pub period_ms: Option<f64>,
        pub target_ms: f32,
        pub clock: Clock,
        pub record: Option<PathBuf>,
        pub seconds: Option<f64>,
        pub from_dump: Option<PathBuf>,
        pub repeat: bool,
        pub mute: bool,
        pub report: Option<PathBuf>,
        pub direct_layouts: Vec<engine::DirectLayoutOverride>,
    }

    fn open_sal() -> Result<SalRing, String> {
        SalRing::open().map_err(|e| capture_message("SAL", e))
    }

    fn open_driver() -> Result<DriverRing, String> {
        DriverRing::open().map_err(|e| {
            capture_message(ssr_i18n::live::source_driver(ssr_i18n::cli_language()), e)
        })
    }

    fn capture_message(name: &str, error: CaptureError) -> String {
        let language = ssr_i18n::cli_language();
        match error {
            CaptureError::NotPresent => ssr_i18n::live::capture_not_present(language, name),
            CaptureError::AccessDenied => ssr_i18n::live::capture_access_denied(language, name),
            other => other.to_string(),
        }
    }

    pub fn record(source: CaptureKind, out: &Path, seconds: f64) -> Result<(), String> {
        let (ring, qpc_frequency, source_name) = match source {
            CaptureKind::Driver => {
                let ring = open_driver()?;
                let frequency = ring.info().qpc_frequency;
                (
                    Source::Driver(ring),
                    frequency,
                    ssr_i18n::live::source_driver(ssr_i18n::cli_language()),
                )
            }
            CaptureKind::Sal => (Source::Sal(open_sal()?), qpc::frequency(), "SAL"),
        };
        let tee = supervisor::TeeCapture::new(ring, qpc::now);
        let (mut ring, recorder) = supervisor::Recorder::start(
            supervisor::RecorderConfig {
                path: out.to_owned(),
                now: qpc::now,
            },
            qpc_frequency,
            tee,
        )?;
        let mut buf = vec![0_u8; 1 << 20];
        let mut bytes = 0_u64;
        let mut endpoints = BTreeMap::<u16, (u64, u64, u64)>::new();
        let started = Instant::now();
        let mut next_report = Duration::from_secs(1);
        let language = ssr_i18n::cli_language();
        eprintln!(
            "{}",
            ssr_i18n::live::recording(language, source_name, out.display(), seconds)
        );
        while started.elapsed().as_secs_f64() < seconds {
            let n = ring
                .read(&mut buf, Duration::from_millis(100))
                .map_err(|e| e.to_string())?;
            if n > 0 {
                bytes += n as u64;
                for record in records(&buf[..n]).flatten() {
                    let endpoint = record.endpoint();
                    let counts = endpoints.entry(endpoint).or_default();
                    match record {
                        Record::Data { bytes: payload, .. } => counts.0 += payload.len() as u64,
                        Record::FormatChange { epoch, format, .. } => {
                            counts.1 += 1;
                            eprintln!(
                                "{}",
                                ssr_i18n::live::format_changed(
                                    language,
                                    endpoint,
                                    epoch,
                                    format_args!("{format:?}"),
                                )
                            );
                        }
                        Record::Overrun { lost_bytes, .. } => counts.2 += lost_bytes,
                        _ => {}
                    }
                }
            }
            if started.elapsed() >= next_report {
                for (endpoint, (data, formats, lost)) in &endpoints {
                    eprintln!(
                        "{}",
                        ssr_i18n::live::record_progress(
                            language,
                            format_args!("{:>4}", next_report.as_secs()),
                            endpoint,
                            data,
                            formats,
                            lost,
                        )
                    );
                }
                next_report += Duration::from_secs(1);
            }
        }
        drop(ring);
        let recorder_lost = recorder.dropped_bytes();
        recorder.stop()?;
        let data = endpoints.values().map(|counts| counts.0).sum::<u64>();
        eprintln!(
            "{}",
            ssr_i18n::live::record_finished(language, bytes, data, recorder_lost, out.display(),)
        );
        if data == 0 {
            return Err(ssr_i18n::live::no_data(language).to_owned());
        }
        Ok(())
    }

    enum Source {
        Driver(DriverRing),
        Sal(SalRing),
        Dump(DumpCapture),
    }

    impl CaptureSource for Source {
        fn read(&mut self, buf: &mut [u8], wait: Duration) -> Result<usize, CaptureError> {
            match self {
                Self::Driver(ring) => ring.read(buf, wait),
                Self::Sal(ring) => ring.read(buf, wait),
                Self::Dump(dump) => dump.read(buf, wait),
            }
        }

        fn now_qpc(&self) -> Option<u64> {
            match self {
                Self::Driver(ring) => ring.now_qpc(),
                Self::Sal(ring) => ring.now_qpc(),
                Self::Dump(dump) => dump.now_qpc(),
            }
        }

        fn set_rate_trim(&mut self, ppm: f64) -> Result<(), TrimUnsupported> {
            match self {
                Self::Driver(ring) => ring.set_rate_trim(ppm),
                Self::Sal(ring) => ring.set_rate_trim(ppm),
                Self::Dump(dump) => dump.set_rate_trim(ppm),
            }
        }

        fn reconnect(&mut self) -> Result<(), CaptureError> {
            match self {
                Self::Driver(ring) => ring.reconnect(),
                Self::Sal(ring) => ring.reconnect(),
                Self::Dump(dump) => dump.reconnect(),
            }
        }
    }

    type OpenedSource = (Source, u64, Option<Box<dyn supervisor::DriverStatsSource>>);

    fn open_source(options: &PlayOptions, ended: Arc<AtomicBool>) -> Result<OpenedSource, String> {
        if let Some(path) = &options.from_dump {
            let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
            return Ok((
                Source::Dump(DumpCapture::from_dump(
                    &bytes,
                    qpc::frequency(),
                    qpc::now,
                    Some(ended),
                    options.repeat,
                )?),
                qpc::frequency(),
                None,
            ));
        }
        match options.source {
            CaptureKind::Driver => {
                let ring = open_driver()?;
                let frequency = ring.info().qpc_frequency;
                let observer = ring.observer();
                Ok((Source::Driver(ring), frequency, Some(Box::new(observer))))
            }
            CaptureKind::Sal => Ok((Source::Sal(open_sal()?), qpc::frequency(), None)),
        }
    }

    fn engine_config(options: &PlayOptions) -> Result<EngineConfig, String> {
        let endpoints = parse_endpoints(options.endpoints.as_deref())?;
        let mut direct = engine::DirectLayouts::default();
        for value in &options.direct_layouts {
            direct
                .set(value.channels, value.layout)
                .map_err(str::to_owned)?;
        }
        Ok(EngineConfig {
            chain: engine::ChainConfig {
                direct,
                endpoints,
                ..engine::ChainConfig::default()
            },
            target_ms: options.target_ms,
            clock: match options.clock {
                Clock::Auto => ClockPreference::Auto,
                Clock::Asrc => ClockPreference::Asrc,
                Clock::Trim => ClockPreference::Trim,
            },
            thread_hook: Some(Arc::new(mmcss::promote_engine_thread)),
            ..EngineConfig::default()
        })
    }

    fn report_request(options: &PlayOptions) -> supervisor::RequestFields {
        super::request_fields(
            options.source,
            options.from_dump.is_some(),
            options.clock,
            options.shared,
            options
                .direct_layouts
                .iter()
                .map(|value| supervisor::DirectLayoutField {
                    channels: value.channels,
                    speakers: value
                        .layout
                        .speakers()
                        .iter()
                        .copied()
                        .map(engine::speaker_name)
                        .map(str::to_owned)
                        .collect(),
                })
                .collect(),
        )
    }

    pub fn play(options: PlayOptions) -> Result<(), String> {
        let profile = engine::load_output_profile(&options.profile).map_err(|e| e.to_string())?;
        let endpoint = devices::find(options.output.as_deref()).map_err(|e| e.to_string())?;
        let language = ssr_i18n::cli_language();
        eprintln!("{}", ssr_i18n::live::output(language, &endpoint.name));
        let ended = Arc::new(AtomicBool::new(false));
        let (source, source_qpc, driver_stats) = open_source(&options, ended.clone())?;
        let config = engine_config(&options)?;
        let output = WasapiOpener {
            endpoint: Some(endpoint.id.clone()),
            rate: options.rate,
            exclusive: !options.shared,
            shared_fallback: options.allow_shared_fallback,
            period: options
                .period_ms
                .filter(|ms| ms.is_finite() && *ms > 0.0)
                .map(|ms| Duration::from_secs_f64(ms / 1000.0)),
        };
        let recorder = options
            .record
            .as_ref()
            .map(|path| supervisor::RecorderConfig {
                path: path.clone(),
                now: qpc::now,
            });
        let mut session = supervisor::Session::start(
            supervisor::SessionConfig {
                engine: config,
                profile,
                qpc_frequency: source_qpc,
                mute: options.mute,
                recorder,
            },
            source,
            output,
            driver_stats,
        )
        .map_err(|error| error.to_string())?;

        let stop = Arc::new(AtomicBool::new(false));
        if options.seconds.is_none() {
            let stop = stop.clone();
            thread::spawn(move || {
                let mut line = String::new();
                if std::io::stdin()
                    .lock()
                    .read_line(&mut line)
                    .is_ok_and(|n| n > 0)
                {
                    stop.store(true, Ordering::Release);
                }
            });
            eprintln!("{}", ssr_i18n::live::playing(language));
        }

        let started = Instant::now();
        let mut next_report = Duration::from_secs(1);
        let mut ended_at: Option<Instant> = None;
        let mut reported_underruns = 0;
        while !stop.load(Ordering::Acquire)
            && options
                .seconds
                .is_none_or(|s| started.elapsed().as_secs_f64() < s)
            && ended_at.is_none_or(|t| t.elapsed() < Duration::from_millis(500))
        {
            if ended_at.is_none() && ended.load(Ordering::Acquire) {
                eprintln!("{}", ssr_i18n::live::dump_ended(language));
                ended_at = Some(Instant::now());
                session.source_ended();
            }
            for notice in session.poll().map_err(|error| error.to_string())? {
                print_notice(notice, language);
            }
            if started.elapsed() >= next_report {
                let status = session.status();
                session.observe(next_report.as_secs());
                print_status(
                    next_report.as_secs(),
                    &status,
                    session.recorder_dropped_bytes(),
                    &mut reported_underruns,
                    language,
                );
                next_report += Duration::from_secs(1);
            }
            thread::sleep(Duration::from_millis(33));
        }

        let report = session.stop().map_err(|error| error.to_string())?;
        eprintln!(
            "{}",
            ssr_i18n::live::stopped(
                language,
                report.engine.blocks,
                report.engine.underruns,
                report.engine.final_status.output_restarts,
                report.engine.lost_bytes,
                report.recorder_lost_bytes,
                format_args!("{:.1}", report.engine.final_status.latency.p50_ms),
                format_args!("{:.1}", report.engine.final_status.latency.p99_ms),
            )
        );
        if let Some(path) = &options.report {
            let text = serde_json::to_string_pretty(&report.to_json(&report_request(&options)))
                .map_err(|error| error.to_string())?;
            std::fs::write(path, text).map_err(|error| format!("{}: {error}", path.display()))?;
        }
        Ok(())
    }

    fn print_notice(notice: supervisor::SessionNotice, language: ssr_types::app::Language) {
        use ssr_i18n::live;

        match notice {
            supervisor::SessionNotice::FormatChanged {
                endpoint,
                epoch,
                format,
                unsupported,
            } => match unsupported {
                Some(reason) => eprintln!(
                    "{}",
                    live::format_unsupported(
                        language,
                        endpoint,
                        epoch,
                        format_args!("{format:?}"),
                        reason
                    )
                ),
                None => eprintln!(
                    "{}",
                    live::format_changed(language, endpoint, epoch, format_args!("{format:?}"))
                ),
            },
            supervisor::SessionNotice::NeedsPrepared => {}
            supervisor::SessionNotice::Prepared {
                kind,
                channels,
                millis,
            } => eprintln!(
                "{}",
                live::prepared(
                    language,
                    kind.as_str(),
                    channels,
                    engine::MIX_RATE,
                    format_args!("{millis:.0}"),
                )
            ),
            supervisor::SessionNotice::PrepareRetry => {
                eprintln!("{}", live::prepare_retry(language))
            }
            supervisor::SessionNotice::Overrun { lost_bytes } => {
                eprintln!("{}", live::overrun(language, lost_bytes))
            }
            supervisor::SessionNotice::OutputOverflow { frames } => {
                eprintln!("{}", live::output_overflow(language, frames))
            }
            supervisor::SessionNotice::OutputLayoutChanged {
                channels,
                channel_mask,
            } => eprintln!(
                "{}",
                live::output_reopened(language, channels, format_args!("{channel_mask:#x}"))
            ),
            supervisor::SessionNotice::OutputReconnected => {
                eprintln!("{}", live::output_reconnected(language))
            }
            supervisor::SessionNotice::FaultCleared => {
                eprintln!("{}", live::fault_cleared(language))
            }
            supervisor::SessionNotice::OutputCadenceFault { ratio } => eprintln!(
                "{}",
                live::output_cadence_fault(language, format_args!("{ratio:.3}"))
            ),
            supervisor::SessionNotice::OutputCadenceReopen { period_ms } => eprintln!(
                "{}",
                live::output_cadence_reopen(language, format_args!("{period_ms:.1}"))
            ),
            supervisor::SessionNotice::Fault(fault) => {
                eprintln!("{}", live::fault(language, format_args!("{fault:?}")))
            }
        }
    }

    fn print_status(
        second: u64,
        status: &ssr_types::EngineStatus,
        recorder_dropped: u64,
        reported_underruns: &mut u64,
        language: ssr_types::app::Language,
    ) {
        let chain_errors = status.unattributed_chain_errors.total()
            + status
                .endpoints
                .iter()
                .map(|endpoint| endpoint.chain_errors.total())
                .sum::<u64>();
        let suffix = if chain_errors != 0 {
            ssr_i18n::live::chain_errors(language, chain_errors)
        } else {
            String::new()
        };
        let fault = status
            .fault
            .as_ref()
            .map(|fault| ssr_i18n::live::fault_suffix(language, format_args!("{fault:?}")))
            .unwrap_or_default();
        eprintln!(
            "{}",
            ssr_i18n::live::status(
                language,
                format_args!("{second:>4}"),
                format_args!("{:.1}", status.latency.p50_ms),
                format_args!("{:.1}", status.latency.p99_ms),
                format_args!("{:.1}", status.ring_level_ms),
                status.underruns,
                status.output_restarts,
                format_args!("{:+.0}", status.clock_ppm),
                status.lost_bytes,
                recorder_dropped,
                suffix,
                fault,
            )
        );
        if status.underruns > *reported_underruns
            && let Some(value) = status.last_underrun
        {
            eprintln!(
                "{}",
                ssr_i18n::live::underrun(
                    language,
                    value.ring_frames,
                    value.marker_frames,
                    value.layout_frames,
                    value.needed_frames,
                    value.produced_frames,
                    format_args!("{:.1}", value.since_block_ms),
                )
            );
        }
        *reported_underruns = status.underruns;
    }

    pub fn devices(formats: bool, json: bool) -> Result<(), String> {
        if json {
            let endpoints = if formats {
                devices::render_endpoint_formats()
                    .map_err(|e| e.to_string())?
                    .into_iter()
                    .map(|formats| {
                        serde_json::json!({
                            "id": formats.endpoint.id,
                            "name": formats.endpoint.name,
                            "default": formats.endpoint.is_default,
                            "mix": {
                                "rate": formats.mix.rate,
                                "channels": formats.mix.channels,
                                "channel_mask": formats.mix.channel_mask,
                            },
                            "layouts": formats.layouts.into_iter().map(|layout| serde_json::json!({
                                "channels": layout.channels,
                                "channel_mask": layout.channel_mask,
                                "exclusive": layout.exclusive,
                            })).collect::<Vec<_>>(),
                        })
                    })
                    .collect::<Vec<_>>()
            } else {
                devices::render_endpoints()
                    .map_err(|e| e.to_string())?
                    .into_iter()
                    .map(|endpoint| {
                        serde_json::json!({
                            "id": endpoint.id,
                            "name": endpoint.name,
                            "default": endpoint.is_default,
                        })
                    })
                    .collect::<Vec<_>>()
            };
            println!(
                "{}",
                serde_json::to_string_pretty(&endpoints).map_err(|e| e.to_string())?
            );
            return Ok(());
        }
        match DriverRing::open() {
            Ok(ring) => {
                let info = ring.info();
                let language = ssr_i18n::cli_language();
                println!(
                    "{}",
                    ssr_i18n::live::driver_info(
                        language,
                        format_args!("{:X}", info.endpoint_mask),
                        info.qpc_frequency,
                    )
                );
                for endpoint in 0..ssr_abi::MAX_ENDPOINTS {
                    if info.endpoint_mask & (1_u32 << endpoint) != 0 {
                        let formats = info.endpoints[endpoint];
                        let codecs = [
                            (ssr_abi::FORMAT_AC3, "AC-3"),
                            (ssr_abi::FORMAT_EAC3, "E-AC-3"),
                            (ssr_abi::FORMAT_EAC3_JOC, "E-AC-3 JOC"),
                            (ssr_abi::FORMAT_TRUEHD, "TrueHD"),
                            (ssr_abi::FORMAT_MAT20, "MAT 2.0"),
                            (ssr_abi::FORMAT_MAT21, "MAT 2.1"),
                            (ssr_abi::FORMAT_DTS, "DTS"),
                            (ssr_abi::FORMAT_DTS_HD, "DTS-HD"),
                        ]
                        .into_iter()
                        .filter_map(|(bit, name)| (formats.codecs & bit != 0).then_some(name))
                        .collect::<Vec<_>>();
                        let rates = ssr_abi::PCM_RATES
                            .iter()
                            .enumerate()
                            .filter(|(i, _)| formats.pcm.rates & (1 << i) != 0)
                            .map(|(_, rate)| rate.to_string())
                            .collect::<Vec<_>>();
                        let shapes = ["int16", "int24 packed", "int24-in-32", "int32", "float32"]
                            .into_iter()
                            .enumerate()
                            .filter_map(|(i, shape)| {
                                (formats.pcm.shapes & (1 << i) != 0).then_some(shape)
                            })
                            .collect::<Vec<_>>();
                        let mut direct = Vec::new();
                        let mut channel = 1_u32;
                        while channel <= 32 {
                            if formats.pcm.direct_channels & (1 << (channel - 1)) == 0 {
                                channel += 1;
                                continue;
                            }
                            let first = channel;
                            while channel < 32 && formats.pcm.direct_channels & (1 << channel) != 0
                            {
                                channel += 1;
                            }
                            direct.push(if first == channel {
                                first.to_string()
                            } else {
                                format!("{first}-{channel}")
                            });
                            channel += 1;
                        }
                        println!(
                            "{}",
                            ssr_i18n::live::driver_endpoint(language, endpoint, codecs.join(", "),)
                        );
                        println!(
                            "{}",
                            ssr_i18n::live::driver_pcm(
                                language,
                                rates.join(", "),
                                shapes.join(", "),
                                formats.pcm.max_masked_channels,
                                direct.join(", "),
                            )
                        );
                    }
                }
            }
            Err(CaptureError::NotPresent) => println!(
                "{}",
                ssr_i18n::live::driver_missing(ssr_i18n::cli_language())
            ),
            Err(error) => println!(
                "{}",
                ssr_i18n::live::driver_error(ssr_i18n::cli_language(), error)
            ),
        }
        if formats {
            for formats in devices::render_endpoint_formats().map_err(|e| e.to_string())? {
                let mark = if formats.endpoint.is_default {
                    "*"
                } else {
                    " "
                };
                println!(
                    "{mark} {}  [{}]",
                    formats.endpoint.name, formats.endpoint.id
                );
                let language = ssr_i18n::cli_language();
                println!(
                    "{}",
                    ssr_i18n::live::device_mix(
                        language,
                        formats.mix.rate,
                        formats.mix.channels,
                        format_args!("{:#x}", formats.mix.channel_mask),
                    )
                );
                for layout in formats.layouts {
                    println!(
                        "{}",
                        ssr_i18n::live::device_layout(
                            language,
                            format_args!("{:>2}", layout.channels),
                            format_args!("{:#07x}", layout.channel_mask),
                            if layout.exclusive {
                                ssr_i18n::live::yes(language)
                            } else {
                                ssr_i18n::live::no(language)
                            },
                        )
                    );
                }
            }
        } else {
            for endpoint in devices::render_endpoints().map_err(|e| e.to_string())? {
                let mark = if endpoint.is_default { "*" } else { " " };
                println!("{mark} {}  [{}]", endpoint.name, endpoint.id);
            }
        }
        Ok(())
    }

    /// `ssr-setup.exe` beside the running ssr-live.exe.
    fn sibling_setup_exe() -> Result<PathBuf, String> {
        let exe = std::env::current_exe()
            .map_err(|error| ssr_i18n::live::locate_exe_failed(ssr_i18n::cli_language(), error))?;
        Ok(exe.with_file_name(SETUP_EXE))
    }

    /// The adapter `host` drives. Both paths are made absolute: the elevated
    /// `ssr-setup` does not start in this process's directory, and it is given
    /// the package files and its result file by path.
    fn windows_host(setup_exe: &Path, state_dir: Option<PathBuf>) -> Result<WindowsHost, String> {
        let absolute = |path: &Path| {
            std::path::absolute(path).map_err(|error| format!("{}: {error}", path.display()))
        };
        let state_dir = host_state_dir(state_dir, &std::env::temp_dir());
        Ok(WindowsHost::new(
            absolute(setup_exe)?,
            absolute(&state_dir)?,
        ))
    }

    /// Prints the answer's document, if any, then fails with its error.
    fn print_answer(answer: host_json::Answer) -> Result<(), String> {
        if let Some(document) = &answer.stdout {
            println!("{}", host_json::to_ascii_json(document));
        }
        answer.error.map_or(Ok(()), Err)
    }

    pub fn host_show(state_dir: Option<PathBuf>) -> Result<(), String> {
        let mut host = windows_host(&sibling_setup_exe()?, state_dir)?;
        let platform = host.platform();
        let driver = host.driver();
        let outputs = host.outputs();
        let roles = host.default_roles();
        let document = host_json::show_json(&platform, &driver, &outputs, &roles);
        println!("{}", host_json::to_ascii_json(&document));
        Ok(())
    }

    pub fn host_set_default(
        role: Role,
        id: String,
        state_dir: Option<PathBuf>,
    ) -> Result<(), String> {
        let mut host = windows_host(&sibling_setup_exe()?, state_dir)?;
        let id = DeviceId(id);
        let result = host.set_default_role(role, &id);
        print_answer(host_json::set_default_answer(role, &id, result))
    }

    pub fn host_setup(
        task: &SetupTask,
        package_dir: &Path,
        state_dir: Option<PathBuf>,
    ) -> Result<(), String> {
        let package_dir = std::path::absolute(package_dir)
            .map_err(|error| format!("{}: {error}", package_dir.display()))?;
        let setup_exe = package_setup_exe(&package_dir, sibling_setup_exe()?, Path::is_file);
        if setup_exe != package_dir.join(SETUP_EXE) {
            eprintln!(
                "{}",
                ssr_i18n::live::package_setup_fallback(
                    ssr_i18n::cli_language(),
                    package_dir.display(),
                    setup_exe.display(),
                )
            );
        }
        let mut host = windows_host(&setup_exe, state_dir)?;
        print_answer(host_json::setup_answer(host.run_setup(task)))
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use ssr_abi::RecordWriter;

        fn record_qpc(bytes: &[u8]) -> u64 {
            ssr_abi::raw_records(bytes)
                .next()
                .unwrap()
                .unwrap()
                .header
                .qpc
        }

        #[test]
        fn paced_dump_releases_records_at_their_pace_and_restamps_them() {
            // 10 MHz dump clock: a stream start at 0, data at 10 and 20 ms.
            let mut dump = dump_header_bytes(10_000_000);
            let mut buf = vec![0_u8; 4096];
            let mut writer = RecordWriter::new(&mut buf);
            assert!(writer.stream_start(1, 0));
            assert!(writer.data(1, 0, 100_000, &[1; 16]));
            assert!(writer.data(1, 16, 200_000, &[2; 16]));
            let len = writer.len();
            dump.extend_from_slice(&buf[..len]);
            let ended = Arc::new(AtomicBool::new(false));
            let mut paced = DumpCapture::from_dump(
                &dump,
                qpc::frequency(),
                qpc::now,
                Some(ended.clone()),
                false,
            )
            .unwrap();
            let frequency = qpc::frequency();

            let mut out = vec![0_u8; 4096];
            let before = qpc::now();
            let n = paced.read(&mut out, Duration::ZERO).unwrap();
            assert_eq!(n, ssr_abi::record_size(0), "only the stream start is due");
            assert!(record_qpc(&out) >= before);

            // A record is stamped when it is released, never before it is due
            // (counted from the first read, which is after `before`). A late
            // release delays only its own stamp, so each one is checked
            // against its due time rather than against the one before it.
            for due_ms in [10, 20] {
                let n = paced.read(&mut out, Duration::from_millis(500)).unwrap();
                assert_eq!(n, ssr_abi::record_size(16));
                assert!(
                    record_qpc(&out) - before >= due(frequency, due_ms),
                    "released before {due_ms} ms"
                );
            }
            assert_eq!(
                ssr_abi::raw_records(&out).next().unwrap().unwrap().payload[0],
                2
            );

            assert!(!ended.load(Ordering::Acquire));
            assert_eq!(
                paced.read(&mut out, Duration::ZERO),
                Err(CaptureError::Ended)
            );
            assert!(ended.load(Ordering::Acquire));
        }

        /// QPC ticks from the origin to a record due `ms` later, rounded down
        /// as the pacing does.
        fn due(frequency: u64, ms: u64) -> u64 {
            frequency * ms / 1000
        }

        fn record_pos(bytes: &[u8]) -> u64 {
            ssr_abi::raw_records(bytes)
                .next()
                .unwrap()
                .unwrap()
                .header
                .byte_pos
        }

        #[test]
        fn paced_dump_loops_with_continuous_positions_and_times() {
            // Data at 10 and 20 ms, 16 bytes each: one pass spans 20 ms and
            // 32 bytes, so the repeat is due at 30 and 40 ms at 32 and 48.
            let mut dump = dump_header_bytes(10_000_000);
            let mut buf = vec![0_u8; 4096];
            let mut writer = RecordWriter::new(&mut buf);
            assert!(writer.stream_start(1, 0));
            assert!(writer.data(1, 0, 100_000, &[1; 16]));
            assert!(writer.data(1, 16, 200_000, &[2; 16]));
            assert!(writer.stream_stop(1, 250_000));
            let len = writer.len();
            dump.extend_from_slice(&buf[..len]);
            let ended = Arc::new(AtomicBool::new(false));
            let mut paced = DumpCapture::from_dump(
                &dump,
                qpc::frequency(),
                qpc::now,
                Some(ended.clone()),
                true,
            )
            .unwrap();
            let frequency = qpc::frequency();

            let mut out = vec![0_u8; 4096];
            let before = qpc::now();
            assert_eq!(
                paced.read(&mut out, Duration::ZERO).unwrap(),
                ssr_abi::record_size(0)
            );
            for (pass, payload, pos, due_ms) in [
                (0, 1, 0, 10),
                (0, 2, 16, 20),
                (1, 1, 32, 30),
                (1, 2, 48, 40),
                (2, 1, 64, 50),
            ] {
                let n = paced.read(&mut out, Duration::from_millis(500)).unwrap();
                assert_eq!(n, ssr_abi::record_size(16), "pass {pass}");
                assert_eq!(
                    ssr_abi::raw_records(&out).next().unwrap().unwrap().payload[0],
                    payload,
                    "pass {pass}"
                );
                assert_eq!(record_pos(&out), pos, "pass {pass}");
                assert!(
                    record_qpc(&out) - before >= due(frequency, due_ms),
                    "released before {due_ms} ms in pass {pass}"
                );
            }
            // The stop record after the span never plays while looping.
            assert!(!ended.load(Ordering::Acquire));
        }

        #[test]
        fn format_two_loop_span_uses_whole_recorded_reads() {
            let mut dump = dump_header_bytes(1);
            let mut buf = vec![0_u8; 512];
            let mut writer = RecordWriter::new(&mut buf);
            assert!(writer.stream_start(1, 0));
            assert!(writer.read_end(0));
            assert!(writer.format_change(1, 0, &[]));
            assert!(writer.data(1, 0, 0, &[1]));
            assert!(writer.read_end(0));
            assert!(writer.data(1, 1, 0, &[2]));
            assert!(writer.stream_stop(1, 0));
            assert!(writer.read_end(0));
            assert!(writer.stream_start(2, 0));
            assert!(writer.read_end(0));
            let len = writer.len();
            dump.extend_from_slice(&buf[..len]);

            let mut capture = DumpCapture::from_dump(&dump, 1, || 0, None, true).unwrap();
            let mut out = vec![0_u8; 512];
            let mut next_kinds = || {
                let len = capture.read(&mut out, Duration::ZERO).unwrap();
                ssr_abi::raw_records(&out[..len])
                    .map(|record| record.unwrap().header.kind)
                    .collect::<Vec<_>>()
            };
            assert_eq!(next_kinds(), [ssr_abi::RECORD_KIND_STREAM_START]);
            assert_eq!(
                next_kinds(),
                [
                    ssr_abi::RECORD_KIND_FORMAT_CHANGE,
                    ssr_abi::RECORD_KIND_DATA
                ]
            );
            assert_eq!(
                next_kinds(),
                [ssr_abi::RECORD_KIND_DATA, ssr_abi::RECORD_KIND_STREAM_STOP]
            );
            assert_eq!(
                next_kinds(),
                [
                    ssr_abi::RECORD_KIND_FORMAT_CHANGE,
                    ssr_abi::RECORD_KIND_DATA
                ]
            );
        }

        #[test]
        fn a_dump_that_ends_on_data_keeps_looping() {
            // The fixtures end on their last data record, with no stop.
            let mut dump = dump_header_bytes(10_000_000);
            let mut buf = vec![0_u8; 4096];
            let mut writer = RecordWriter::new(&mut buf);
            assert!(writer.data(1, 0, 100_000, &[1; 16]));
            assert!(writer.data(1, 16, 200_000, &[2; 16]));
            let len = writer.len();
            dump.extend_from_slice(&buf[..len]);
            let ended = Arc::new(AtomicBool::new(false));
            let mut paced = DumpCapture::from_dump(
                &dump,
                qpc::frequency(),
                qpc::now,
                Some(ended.clone()),
                true,
            )
            .unwrap();
            let mut out = vec![0_u8; 4096];
            for pos in [0, 16, 32, 48, 64] {
                let n = paced.read(&mut out, Duration::from_millis(500)).unwrap();
                assert_eq!(n, ssr_abi::record_size(16));
                assert_eq!(record_pos(&out), pos);
            }
            assert!(!ended.load(Ordering::Acquire));
        }

        #[test]
        fn looping_needs_two_data_records() {
            let mut dump = dump_header_bytes(10_000_000);
            let mut buf = vec![0_u8; 256];
            let mut writer = RecordWriter::new(&mut buf);
            assert!(writer.data(1, 0, 100_000, &[1; 16]));
            let len = writer.len();
            dump.extend_from_slice(&buf[..len]);
            let ended = Arc::new(AtomicBool::new(false));
            assert!(
                DumpCapture::from_dump(&dump, qpc::frequency(), qpc::now, Some(ended), true)
                    .is_err()
            );
        }
    }
}

#[cfg(not(windows))]
mod platform {
    use std::path::{Path, PathBuf};

    use super::{CaptureKind, Clock};

    #[allow(dead_code)]
    pub struct PlayOptions {
        pub source: CaptureKind,
        pub endpoints: Option<String>,
        pub profile: PathBuf,
        pub output: Option<String>,
        pub shared: bool,
        pub allow_shared_fallback: bool,
        pub rate: u32,
        pub period_ms: Option<f64>,
        pub target_ms: f32,
        pub clock: Clock,
        pub record: Option<PathBuf>,
        pub seconds: Option<f64>,
        pub from_dump: Option<PathBuf>,
        pub repeat: bool,
        pub mute: bool,
        pub report: Option<PathBuf>,
        pub direct_layouts: Vec<engine::DirectLayoutOverride>,
    }

    pub fn record(_source: CaptureKind, _out: &Path, _seconds: f64) -> Result<(), String> {
        Err(ssr_i18n::live::windows_only(ssr_i18n::cli_language()).to_owned())
    }

    pub fn play(_options: PlayOptions) -> Result<(), String> {
        Err(ssr_i18n::live::windows_only(ssr_i18n::cli_language()).to_owned())
    }

    pub fn devices(_formats: bool, _json: bool) -> Result<(), String> {
        Err(ssr_i18n::live::windows_only(ssr_i18n::cli_language()).to_owned())
    }

    pub fn host_show(_state_dir: Option<PathBuf>) -> Result<(), String> {
        Err(ssr_i18n::live::windows_only(ssr_i18n::cli_language()).to_owned())
    }

    pub fn host_set_default(
        _role: ssr_types::Role,
        _id: String,
        _state_dir: Option<PathBuf>,
    ) -> Result<(), String> {
        Err(ssr_i18n::live::windows_only(ssr_i18n::cli_language()).to_owned())
    }

    pub fn host_setup(
        _task: &ssr_types::SetupTask,
        _package_dir: &Path,
        _state_dir: Option<PathBuf>,
    ) -> Result<(), String> {
        Err(ssr_i18n::live::windows_only(ssr_i18n::cli_language()).to_owned())
    }
}

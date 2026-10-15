#![forbid(unsafe_code)]

//! `ssr-setup`: the administrator one-shot helper (ADR 0013, ADR 0032).
//!
//! The app runs it elevated (one UAC prompt) for one task: `install`,
//! `uninstall`, `set-formats`, `restart-device` or `repair`. It answers with
//! one JSON line (`ssr_abi::setup_contract::TaskResult`) on stdout and, since
//! an elevated process's stdout cannot reach its parent, in the `--result`
//! file too. It exits 0 when the task succeeded and 1 when it failed. The
//! tasks live in `ssr-setup-sys` and run on Windows only; elsewhere it answers
//! that and exits 2.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use ssr_abi::setup_contract::{TaskLine, TaskName};

#[derive(Debug, Parser)]
#[command(
    name = "ssr-setup",
    version,
    about = "Spatial SoftReceiver administrator tasks (run elevated by the app)"
)]
struct Cli {
    #[command(subcommand)]
    task: Task,
}

#[derive(Debug, Subcommand)]
enum Task {
    /// Trust the package's test-signing certificate, create the root device,
    /// install the driver and apply the endpoints' exclusive-mode policy.
    Install {
        /// The driver package INF.
        #[arg(long)]
        inf: PathBuf,
        /// The test-signing certificate; a missing file means a release package.
        #[arg(long)]
        cert: Option<PathBuf>,
        /// Where to write the result line.
        #[arg(long)]
        result: PathBuf,
    },
    /// Remove the current and legacy devices, driver packages, orphan services
    /// and test-signing certificates, and report what stayed.
    Uninstall {
        #[arg(long)]
        result: PathBuf,
    },
    /// Set the driver's endpoint mask, restart the device and apply the
    /// exclusive-mode policy to the endpoints the mask enables.
    SetFormats {
        /// Bit n enables endpoint n.
        #[arg(long, value_parser = clap::value_parser!(u8).range(1..16))]
        endpoint_mask: u8,
        #[arg(long)]
        result: PathBuf,
    },
    /// Disable and enable the device.
    RestartDevice {
        #[arg(long)]
        result: PathBuf,
    },
    /// Reinstall the driver on the device and apply the exclusive-mode policy,
    /// or install when there is no device.
    Repair {
        #[arg(long)]
        inf: PathBuf,
        #[arg(long)]
        cert: Option<PathBuf>,
        #[arg(long)]
        result: PathBuf,
    },
}

impl Task {
    fn into_line(self) -> TaskLine {
        let line = |task, inf, cert, endpoint_mask, result| TaskLine {
            task,
            inf,
            cert,
            endpoint_mask,
            result,
        };
        match self {
            Task::Install { inf, cert, result } => {
                line(TaskName::Install, Some(inf), cert, None, result)
            }
            Task::Uninstall { result } => line(TaskName::Uninstall, None, None, None, result),
            Task::SetFormats {
                endpoint_mask,
                result,
            } => line(
                TaskName::SetFormats,
                None,
                None,
                Some(endpoint_mask),
                result,
            ),
            Task::RestartDevice { result } => {
                line(TaskName::RestartDevice, None, None, None, result)
            }
            Task::Repair { inf, cert, result } => {
                line(TaskName::Repair, Some(inf), cert, None, result)
            }
        }
    }
}

fn main() -> ExitCode {
    execute(&Cli::parse().task.into_line())
}

#[cfg(windows)]
fn execute(line: &TaskLine) -> ExitCode {
    let result = ssr_setup_sys::tasks::run(line);
    let json = result.to_json();
    println!("{json}");
    if let Err(error) = write_result(&line.result, &json) {
        eprintln!("ssr-setup: {}: {error}", line.result.display());
    }
    ExitCode::from(if result.ok { 0 } else { 1 })
}

/// The same line, for the parent that cannot read an elevated stdout.
#[cfg(windows)]
fn write_result(path: &std::path::Path, json: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent().filter(|dir| !dir.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, format!("{json}\n"))
}

#[cfg(not(windows))]
const NOT_ON_WINDOWS: &str = r#"{"ok":false,"error":"ssr-setup runs on Windows","code":2}"#;

#[cfg(not(windows))]
fn execute(_line: &TaskLine) -> ExitCode {
    println!("{NOT_ON_WINDOWS}");
    ExitCode::from(2)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ssr_types::SetupTask;
    use std::path::Path;

    fn parse(args: &[&str]) -> Result<TaskLine, clap::Error> {
        Cli::try_parse_from(std::iter::once("ssr-setup").chain(args.iter().copied()))
            .map(|cli| cli.task.into_line())
    }

    #[test]
    fn every_subcommand_parses_to_its_task_line() {
        let result = PathBuf::from("r.json");
        assert_eq!(
            parse(&[
                "install", "--inf", "p.inf", "--cert", "p.cer", "--result", "r.json"
            ])
            .unwrap(),
            TaskLine {
                task: TaskName::Install,
                inf: Some("p.inf".into()),
                cert: Some("p.cer".into()),
                endpoint_mask: None,
                result: result.clone(),
            }
        );
        assert_eq!(
            parse(&["install", "--inf", "p.inf", "--result", "r.json"])
                .unwrap()
                .cert,
            None
        );
        assert_eq!(
            parse(&["uninstall", "--result", "r.json"]).unwrap(),
            TaskLine {
                task: TaskName::Uninstall,
                inf: None,
                cert: None,
                endpoint_mask: None,
                result: result.clone(),
            }
        );
        assert_eq!(
            parse(&["set-formats", "--endpoint-mask", "5", "--result", "r.json"]).unwrap(),
            TaskLine {
                task: TaskName::SetFormats,
                inf: None,
                cert: None,
                endpoint_mask: Some(5),
                result: result.clone(),
            }
        );
        assert_eq!(
            parse(&["restart-device", "--result", "r.json"]).unwrap(),
            TaskLine {
                task: TaskName::RestartDevice,
                inf: None,
                cert: None,
                endpoint_mask: None,
                result: result.clone(),
            }
        );
        assert_eq!(
            parse(&[
                "repair", "--inf", "p.inf", "--cert", "p.cer", "--result", "r.json"
            ])
            .unwrap(),
            TaskLine {
                task: TaskName::Repair,
                inf: Some("p.inf".into()),
                cert: Some("p.cer".into()),
                endpoint_mask: None,
                result,
            }
        );
    }

    #[test]
    fn the_apps_task_lines_parse_back_unchanged() {
        let package = Path::new("C:/Program Files/Spatial SoftReceiver");
        let result =
            PathBuf::from("C:/Users/a b/AppData/Local/Spatial SoftReceiver/setup-result.json");
        for task in [
            SetupTask::Install,
            SetupTask::Uninstall,
            SetupTask::SetFormats { endpoint_mask: 7 },
            SetupTask::RestartDevice,
            SetupTask::Repair,
        ] {
            let line = TaskLine::from_setup_task(&task, package, result.clone());
            let args = std::iter::once("ssr-setup".into()).chain(line.to_args());
            let parsed = Cli::try_parse_from(args).unwrap().task.into_line();
            assert_eq!(parsed, line, "{task:?}");
        }
    }

    #[test]
    fn options_are_required_and_checked() {
        for args in [
            &["install", "--result", "r.json"][..],
            &["install", "--inf", "p.inf"],
            &["uninstall"],
            &["set-formats", "--result", "r.json"],
            &["set-formats", "--endpoint-mask", "0", "--result", "r.json"],
            &["set-formats", "--endpoint-mask", "16", "--result", "r.json"],
            &["repair", "--result", "r.json"],
            &["reboot", "--result", "r.json"],
            &[],
        ] {
            assert!(parse(args).is_err(), "{args:?}");
        }
    }

    #[cfg(not(windows))]
    #[test]
    fn other_systems_answer_with_a_failed_result() {
        use ssr_abi::setup_contract::TaskResult;
        assert_eq!(
            TaskResult::from_json(NOT_ON_WINDOWS).unwrap(),
            TaskResult::failure("ssr-setup runs on Windows", Some(2))
        );
    }

    #[test]
    fn subcommands_are_the_contracts_task_names() {
        let names: Vec<String> = <Cli as clap::CommandFactory>::command()
            .get_subcommands()
            .map(|command| command.get_name().to_owned())
            .collect();
        let expected: Vec<String> = TaskName::ALL.iter().map(ToString::to_string).collect();
        assert_eq!(names, expected);
    }
}

//! The supervisor state machine (ADR 0013 and its 2026-10-15 addendum).
//!
//! One place holds the setup steps, the chosen output, the engine's lifetime
//! and the default roles. The screen sends [`Intent`]s to [`Supervisor::handle`]
//! and draws [`Supervisor::snapshot`]; the operating system is reached only
//! through [`Host`], and the engine only through [`Launcher`] and
//! [`EngineHandle`], so every transition is tested on Linux with fakes.
//!
//! Every intent either succeeds, fails with a [`UserError`] that the snapshot
//! keeps until the next success, or is refused (not possible in this phase, or
//! not wired yet): a refusal returns its error but changes nothing, so the
//! snapshot keeps the step's explanation. Wizard steps that need the user to
//! act outside the app (a newer Windows, test signing, a reboot) are entered
//! with an error that says what to do.
//!
//! The shell drives the supervisor from a thread of its own, never the UI
//! thread: the Windows host joins the multithreaded apartment for its COM
//! calls, which a UI thread's single-threaded apartment cannot.

use std::cell::RefCell;
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use engine::{LiveChange, OutputProfile, Prepared};
use speakers::{SpeakerConfig, SpeakerProfile, SpeakerRange};
use ssr_types::app::{
    Intent, NextAction, OutputChoice, Phase, ProfileKind, ProfileView, SetupStep, Snapshot,
    UserError,
};
use ssr_types::ports::PolicyState;
use ssr_types::{
    Arch, DeviceId, DeviceState, DirectionId, DirectionView, DriverState, EngineFault,
    EngineStatus, Host, HostError, Layout, OutputDevice, Platform, Role, SecureBoot, SetupTask,
};

use crate::restore_point::{self, RestorePoint};
use crate::session::{Session, SessionError, SessionNotice};

/// A running engine as the state machine sees it.
pub trait EngineHandle {
    /// The engine's notices since the last poll. A `NeedsPrepared` is
    /// passed on, not answered: the supervisor prepares the profile it is
    /// playing and hands it over with [`EngineHandle::apply`].
    fn poll(&mut self) -> Result<Vec<SessionNotice>, SessionError>;
    fn status(&self) -> EngineStatus;
    /// Hands a change to the running engine, all or nothing: an `Err`
    /// ([`SessionError::Rejected`] when the engine's queue refuses it) leaves
    /// the output as it was. A `Prepared` was prepared by the caller, off the
    /// engine's real-time threads.
    fn apply(&mut self, change: LiveChange) -> Result<(), SessionError>;
    /// As [`EngineHandle::apply`], but returns a rejected change so a prepared
    /// output can be retried without doing the slow preparation again.
    fn apply_retaining(&mut self, change: LiveChange) -> Result<(), (SessionError, LiveChange)>;
    fn stop(self: Box<Self>) -> Result<(), SessionError>;
}

impl EngineHandle for Session {
    /// Leaves the re-prepare to the supervisor ([`Session::poll_events`]):
    /// the session knows only the profile it started with.
    fn poll(&mut self) -> Result<Vec<SessionNotice>, SessionError> {
        Ok(Session::poll_events(self))
    }

    fn status(&self) -> EngineStatus {
        Session::status(self)
    }

    fn apply(&mut self, change: LiveChange) -> Result<(), SessionError> {
        Session::apply(self, change)
    }

    fn apply_retaining(&mut self, change: LiveChange) -> Result<(), (SessionError, LiveChange)> {
        Session::apply_retaining(self, change)
    }

    /// Discards the report: the shell has no use for it yet.
    fn stop(self: Box<Self>) -> Result<(), SessionError> {
        Session::stop(*self).map(drop)
    }
}

/// Starts the engine for the state machine.
pub trait Launcher {
    /// Starts the engine on `output` (an active device that is not ours) with
    /// `profile`, which the supervisor resolved ([`Supervisor::output_profile`])
    /// and keeps: it is what a later re-prepare builds from. The launcher
    /// reads no profile of its own.
    fn launch(
        &mut self,
        output: &OutputDevice,
        profile: &OutputProfile,
    ) -> Result<Box<dyn EngineHandle>, SessionError>;
}

/// The running engine and the profile it plays.
struct Running {
    handle: Box<dyn EngineHandle>,
    /// The profile launched, then each one a profile intent handed over and
    /// each gain the engine took since: what a re-prepare builds from.
    profile: OutputProfile,
    /// The engine asked for its output again (`NeedsPrepared`) and has not
    /// taken one since.
    needs_prepared: bool,
    /// A prepared output the engine's full change queue has not taken yet.
    /// Keeping it makes retries cheap and leaves the guard's stall clock alone.
    pending: Option<Box<Prepared>>,
}

/// What the profile intents remember (ADR 0013, 2026-10-15 addendum).
///
/// Every field has a default, so a settings file written before profiles
/// existed, or one missing a field, still loads.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct ProfileSettings {
    /// The imported BRIR set: a folder of direction WAVs, `hesuvi.wav`, or
    /// `hrir.wav`. `Some` makes the output headphones.
    pub brir: Option<PathBuf>,
    /// The imported headphone FIR, folded into the BRIR set.
    pub headphone_fir: Option<PathBuf>,
    /// The gain [`Intent::SetGain`] saved, in dB (-20 to +10). `None` (never
    /// set, or a settings file from before profiles) keeps the gain of the
    /// state directory's `profile.toml` and is 0 dB for an imported set or
    /// the default stereo profile; `Some` overrides all of them.
    pub gain_db: Option<f32>,
}

/// Settings the shell persists; the machine reads and updates them.
///
/// The shell persists [`Supervisor::settings`] after [`Supervisor::new`] and
/// after every [`Supervisor::handle`] that returns `Ok`, because both can
/// rewrite the stored id (an output found again by its name). A `handle` that
/// fails after such a rewrite leaves the old id persisted, which the name finds
/// again next time. A profile intent changes the settings only when it
/// succeeds, so a failed import or gain never reaches the file.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Settings {
    /// The chosen output, remembered by id and name: the id can change across
    /// reboots, the name is the fallback.
    pub output: Option<OutputChoice>,
    #[serde(default)]
    pub profile: ProfileSettings,
}

/// What this build needs from the PC.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    /// The driver package is test signed (until the attestation-signed driver
    /// of M5).
    pub requires_test_signing: bool,
    pub min_os_build: u32,
    /// The architecture the driver package was built for.
    pub arch: Arch,
    /// How long the engine may try to reconnect a lost output or capture
    /// before the supervisor stops it and gives the roles back.
    pub fault_grace: Duration,
    /// The state directory's `profile.toml`. When the file exists it decides
    /// the output (headphones or speakers) and the import intents are
    /// refused; only the saved gain overrides it.
    pub profile_file: Option<PathBuf>,
}

impl Config {
    /// Windows 10 2004.
    pub const MIN_OS_BUILD: u32 = 19_041;

    /// Three of the engine's longest reconnect waits (5 s each).
    pub const FAULT_GRACE: Duration = Duration::from_secs(15);

    /// The architecture this program was compiled for.
    pub const BUILD_ARCH: Arch = if cfg!(target_arch = "x86_64") {
        Arch::X64
    } else if cfg!(target_arch = "aarch64") {
        Arch::Arm64
    } else {
        Arch::Other
    };
}

impl Default for Config {
    /// This build: a test-signed package for Windows 10 2004 or newer on the
    /// compiled architecture.
    fn default() -> Self {
        Self {
            requires_test_signing: true,
            min_os_build: Self::MIN_OS_BUILD,
            arch: Self::BUILD_ARCH,
            fault_grace: Self::FAULT_GRACE,
            profile_file: None,
        }
    }
}

pub struct Supervisor<H: Host, L: Launcher> {
    /// In a cell only so [`Supervisor::snapshot`] can list the outputs afresh
    /// through `&self`; every other path reaches it with `get_mut`.
    host: RefCell<H>,
    launcher: L,
    config: Config,
    settings: Settings,
    phase: Phase,
    /// `Some` exactly while the phase is [`Phase::Running`].
    engine: Option<Running>,
    /// The engine reported a lost output or capture and is reconnecting.
    reconnecting: Option<Reconnecting>,
    /// The restore point written when the roles were taken; `Some` exactly
    /// while they are.
    taken: Option<RestorePoint>,
    driver: Option<DriverState>,
    platform: Option<Platform>,
    error: Option<UserError>,
    notes: Vec<String>,
    shell_notes: Vec<String>,
    /// The resolved profile as the screen sees it, cached: it changes only
    /// when a profile intent succeeds or the engine starts, so a snapshot
    /// never reads the disk for it.
    profile: ProfileView,
}

impl<H: Host, L: Launcher> fmt::Debug for Supervisor<H, L> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Supervisor")
            .field("phase", &self.phase)
            .field("settings", &self.settings)
            .field("roles_taken", &self.taken.is_some())
            .field("reconnecting", &self.reconnecting.is_some())
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}

impl<H: Host, L: Launcher> Supervisor<H, L> {
    /// Looks at the host once and starts in Ready when the driver is installed
    /// without a problem (its exclusive policy not missing) and the settings
    /// name an output that is present, else in Setup(Welcome). Never takes the
    /// roles here.
    pub fn new(host: H, launcher: L, config: Config, settings: Settings) -> Self {
        let mut supervisor = Self {
            host: RefCell::new(host),
            launcher,
            config,
            settings,
            phase: Phase::Setup(SetupStep::Welcome),
            engine: None,
            reconnecting: None,
            taken: None,
            driver: None,
            platform: None,
            error: None,
            notes: Vec::new(),
            shell_notes: Vec::new(),
            profile: default_profile_view(),
        };
        supervisor.refresh_profile();
        let driver = supervisor.host.get_mut().driver();
        let fine = driver_is_fine(&driver);
        supervisor.driver = Some(driver);
        if fine && supervisor.settings.output.is_some() {
            let outputs = supervisor.host.get_mut().outputs();
            if supervisor.find_output(&outputs).is_some() {
                supervisor.phase = Phase::Ready;
            }
        }
        supervisor
    }

    /// Applies one intent. A failure's `Err` is also kept in the snapshot
    /// until an intent succeeds; a refusal's is only returned.
    pub fn handle(&mut self, intent: Intent) -> Result<(), UserError> {
        match self.dispatch(intent) {
            Outcome::Done => {
                self.error = None;
                Ok(())
            }
            Outcome::Failed(error) => {
                self.error = Some(error.clone());
                Err(error)
            }
            Outcome::Refused(error) => Err(error),
        }
    }

    /// Polls the running engine and returns its notices (none outside
    /// Running).
    ///
    /// A `NeedsPrepared` notice is only remembered: the output is prepared
    /// again by [`Supervisor::prepare_again`], which the caller runs when
    /// [`Supervisor::needs_prepared`] says so. Preparing can take seconds,
    /// and the shell first tells the guard it is not to count them as a
    /// stall (ADR 0033).
    ///
    /// A lost output or capture is the engine's to reconnect: it retries
    /// without limit and says `FaultCleared` when it succeeds. So a `Fault`
    /// notice keeps Running with an error that says it is reconnecting, and
    /// `FaultCleared` clears it with a note. A fault that outlasts
    /// [`Config::fault_grace`] (from its first notice, by `now`), or a poll
    /// error, stops the engine, gives the roles back and moves to Faulted.
    pub fn tick(&mut self, now: Instant) -> Vec<SessionNotice> {
        let Some(running) = self.engine.as_mut() else {
            return Vec::new();
        };
        let notices = match running.handle.poll() {
            Ok(notices) => notices,
            Err(error) => {
                self.fault(user_error(
                    format!("엔진이 멈췄습니다: {error}"),
                    NextAction::Retry,
                ));
                return Vec::new();
            }
        };
        for notice in &notices {
            match notice {
                SessionNotice::Fault(fault) => {
                    self.error = Some(reconnecting_error(fault));
                    if self.reconnecting.is_none() {
                        self.reconnecting = Some(Reconnecting {
                            since: now,
                            fault: fault.clone(),
                        });
                    }
                }
                SessionNotice::FaultCleared => self.reconnected(),
                SessionNotice::NeedsPrepared => {
                    if let Some(running) = self.engine.as_mut() {
                        running.needs_prepared = true;
                    }
                }
                _ => {}
            }
        }
        if let Some(reconnecting) = &self.reconnecting
            && now.saturating_duration_since(reconnecting.since) >= self.config.fault_grace
        {
            let what = match &reconnecting.fault {
                EngineFault::OutputLost(detail) => {
                    format!("출력 장치와의 연결이 끊어졌습니다: {detail}")
                }
                EngineFault::CaptureLost(_) => "캡처 장치와의 연결이 끊어졌습니다.".to_owned(),
            };
            let error = user_error(
                format!(
                    "엔진이 {}초 동안 다시 연결하지 못했습니다. {what}",
                    self.config.fault_grace.as_secs()
                ),
                NextAction::Retry,
            );
            self.fault(error);
        }
        notices
    }

    /// The running engine asked for its output to be prepared again and has
    /// not taken one since.
    pub fn needs_prepared(&self) -> bool {
        self.engine
            .as_ref()
            .is_some_and(|running| running.needs_prepared)
    }

    /// True when answering `NeedsPrepared` still has slow preparation ahead.
    /// Once an output is pending, later calls only retry the nonblocking apply.
    pub fn prepare_work_pending(&self) -> bool {
        self.engine
            .as_ref()
            .is_some_and(|running| running.needs_prepared && running.pending.is_none())
    }

    /// Answers the engine's `NeedsPrepared`: prepares the profile it plays
    /// (the one launched, or the last one a profile intent handed over, with
    /// the gain set since) on this thread and hands it over. A full change
    /// queue is [`SessionNotice::PrepareRetry`]; the prepared output is kept so
    /// the next call retries only the handoff. A profile that no longer
    /// prepares (its files changed under it) stops the engine, gives the roles
    /// back and moves to Faulted, since the output cannot be rebuilt. Nothing
    /// happens unless [`Supervisor::needs_prepared`].
    pub fn prepare_again(&mut self) -> Vec<SessionNotice> {
        let Some(running) = self
            .engine
            .as_mut()
            .filter(|running| running.needs_prepared)
        else {
            return Vec::new();
        };
        let mut millis = 0.0;
        if running.pending.is_none() {
            let started = Instant::now();
            match running.profile.prepare() {
                Ok(prepared) => running.pending = Some(Box::new(prepared)),
                Err(error) => {
                    self.fault(user_error(
                        format!("엔진이 멈췄습니다: {}", SessionError::from(error)),
                        NextAction::Retry,
                    ));
                    return Vec::new();
                }
            }
            millis = started.elapsed().as_secs_f64() * 1_000.0;
        }
        let prepared = running
            .pending
            .take()
            .expect("a requested output is prepared");
        let kind = prepared.kind();
        let channels = prepared.channels();
        let directions = prepared.direction_views();
        match running
            .handle
            .apply_retaining(LiveChange::Prepared(prepared))
        {
            Ok(()) => {
                running.needs_prepared = false;
                self.profile.directions = directions;
                vec![SessionNotice::Prepared {
                    kind,
                    channels,
                    millis,
                }]
            }
            Err((SessionError::Rejected, LiveChange::Prepared(prepared))) => {
                running.pending = Some(prepared);
                vec![SessionNotice::PrepareRetry]
            }
            Err((error, _)) => {
                self.fault(user_error(
                    format!("엔진이 멈췄습니다: {error}"),
                    NextAction::Retry,
                ));
                Vec::new()
            }
        }
    }

    pub fn snapshot(&self) -> Snapshot {
        let outputs = self
            .host
            .borrow_mut()
            .outputs()
            .into_iter()
            .filter(is_choosable)
            .map(|output| OutputChoice {
                id: output.id,
                name: output.name,
            })
            .collect();
        Snapshot {
            phase: self.phase.clone(),
            profile: self.profile.clone(),
            engine: self.engine_status(),
            outputs,
            chosen_output: self.settings.output.clone(),
            driver: self.driver.clone(),
            platform: self.platform.clone(),
            roles_taken: self.taken.is_some(),
            error: self.error.clone(),
            notes: self
                .shell_notes
                .iter()
                .chain(&self.notes)
                .cloned()
                .collect(),
        }
    }

    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    /// Adds a fact supplied by the shell. Setup begins by clearing old facts.
    pub fn note(&mut self, note: String) {
        self.shell_notes.push(note);
    }

    pub fn phase(&self) -> &Phase {
        &self.phase
    }

    /// The running engine's status without listing the host's outputs, for
    /// the shell's 30 Hz meters; `None` outside Running.
    pub fn engine_status(&self) -> Option<EngineStatus> {
        self.engine.as_ref().map(|running| running.handle.status())
    }

    fn dispatch(&mut self, intent: Intent) -> Outcome {
        if self.phase == Phase::Quit {
            return refused();
        }
        let result = match intent {
            // The profile can change whenever the wizard is done; a running
            // engine takes the change live, all or nothing.
            Intent::ImportBrir(path) => match self.phase {
                Phase::Ready | Phase::Running | Phase::Faulted => self.import_brir(path),
                _ => return refused(),
            },
            Intent::ImportHeadphoneFir(path) => match self.phase {
                Phase::Ready | Phase::Running | Phase::Faulted => self.import_headphone_fir(path),
                _ => return refused(),
            },
            Intent::SetGain(db) => match self.phase {
                Phase::Ready | Phase::Running | Phase::Faulted => self.set_gain(db),
                _ => return refused(),
            },
            Intent::TestDirection(id) => match self.phase {
                Phase::Running => self.test_direction(id),
                Phase::Ready | Phase::Faulted => {
                    return Outcome::Refused(user_error(
                        "켠 뒤에 시험음을 낼 수 있습니다.",
                        NextAction::None,
                    ));
                }
                _ => return refused(),
            },
            Intent::StartDiagnosticRecording => {
                return Outcome::Refused(user_error(
                    "진단 녹음은 다음 조각에서 옵니다.",
                    NextAction::None,
                ));
            }
            Intent::BeginSetup => match self.phase {
                Phase::Setup(SetupStep::Welcome) => {
                    self.notes.clear();
                    self.shell_notes.clear();
                    self.evaluate()
                }
                _ => return refused(),
            },
            // At the output step a recheck lists the outputs again (a device
            // plugged in meanwhile) and resumes when the settings name one.
            Intent::Recheck => match self.phase {
                Phase::Setup(
                    SetupStep::Unsupported
                    | SetupStep::NotTestSigning
                    | SetupStep::RebootRequired
                    | SetupStep::Driver
                    | SetupStep::ChooseOutput,
                ) => self.evaluate(),
                Phase::Faulted => {
                    self.phase = Phase::Ready;
                    Ok(())
                }
                _ => return refused(),
            },
            Intent::InstallDriver => match self.phase {
                Phase::Setup(SetupStep::Driver) => self.install_driver(),
                _ => return refused(),
            },
            Intent::ChooseOutput(id) => match self.phase {
                Phase::Setup(SetupStep::ChooseOutput) | Phase::Ready => self.choose_output(&id),
                Phase::Running => Err(user_error(
                    "먼저 끈 뒤 출력을 바꾸십시오.",
                    NextAction::None,
                )),
                _ => return refused(),
            },
            Intent::Enable => match self.phase {
                Phase::Ready | Phase::Faulted => self.enable(),
                _ => return refused(),
            },
            Intent::Disable => match self.phase {
                Phase::Running => {
                    self.stop_engine();
                    self.restore();
                    self.phase = Phase::Ready;
                    Ok(())
                }
                _ => return refused(),
            },
            Intent::Quit => {
                self.stop_engine();
                self.restore();
                self.phase = Phase::Quit;
                Ok(())
            }
        };
        match result {
            Ok(()) => Outcome::Done,
            Err(error) => Outcome::Failed(error),
        }
    }

    /// The wizard's checks in order; the phase stops at the first that fails.
    fn evaluate(&mut self) -> Result<(), UserError> {
        let platform = self.host.get_mut().platform();
        self.platform = Some(platform.clone());
        if platform.os_build < self.config.min_os_build {
            self.phase = Phase::Setup(SetupStep::Unsupported);
            return Err(user_error(
                os_requirement(self.config.min_os_build),
                NextAction::None,
            ));
        }
        if platform.arch != self.config.arch {
            self.phase = Phase::Setup(SetupStep::Unsupported);
            return Err(user_error(
                format!(
                    "이 빌드는 {}용이지만 이 PC는 {}입니다.",
                    arch_name(self.config.arch),
                    arch_name(platform.arch)
                ),
                NextAction::None,
            ));
        }
        if self.config.requires_test_signing && !platform.test_signing {
            let mut message = "시험서명 모드가 꺼져 있습니다. Secure Boot를 끈 뒤 관리자 명령으로 \
                               bcdedit /set testsigning on을 실행하고 다시 시작하십시오."
                .to_owned();
            if platform.secure_boot == SecureBoot::Enabled {
                message.push_str(" Secure Boot가 켜져 있어 먼저 꺼야 합니다.");
            }
            self.phase = Phase::Setup(SetupStep::NotTestSigning);
            return Err(user_error(message, NextAction::Reboot));
        }
        self.evaluate_driver()
    }

    /// The driver check and what follows it.
    fn evaluate_driver(&mut self) -> Result<(), UserError> {
        let driver = self.host.get_mut().driver();
        self.driver = Some(driver.clone());
        match driver {
            DriverState::NotInstalled => {
                self.phase = Phase::Setup(SetupStep::Driver);
                Ok(())
            }
            DriverState::RebootRequired => {
                self.phase = Phase::Setup(SetupStep::RebootRequired);
                Err(reboot_required())
            }
            DriverState::Installed {
                problem_code: 52, ..
            } => {
                self.phase = Phase::Setup(SetupStep::Driver);
                Err(user_error(
                    "드라이버 서명을 받아들이지 않았습니다(문제 코드 52). \
                     시험서명 모드를 확인한 뒤 다시 설치하십시오.",
                    NextAction::Retry,
                ))
            }
            DriverState::Installed {
                problem_code: 0,
                service_running: false,
                ..
            } => {
                self.phase = Phase::Setup(SetupStep::Driver);
                Err(user_error(
                    "드라이버 서비스가 돌고 있지 않습니다. 복구를 실행하십시오.",
                    NextAction::Retry,
                ))
            }
            DriverState::Installed {
                problem_code: 0,
                service_running: true,
                exclusive_policy: PolicyState::Missing,
            } => {
                self.phase = Phase::Setup(SetupStep::Driver);
                Err(user_error(
                    "드라이버는 있지만 독점 정책이 적용되지 않았습니다. 복구를 실행하십시오.",
                    NextAction::Retry,
                ))
            }
            // An unknown policy (no endpoint active yet, or unreadable) is
            // not held against the driver.
            DriverState::Installed {
                problem_code: 0,
                service_running: true,
                ..
            } => {
                self.phase = Phase::Setup(SetupStep::ChooseOutput);
                self.resume_output()
            }
            DriverState::Installed { problem_code, .. } => {
                self.phase = Phase::Setup(SetupStep::Driver);
                Err(user_error(
                    format!(
                        "드라이버가 올바르게 설치되지 않았습니다(문제 코드 {problem_code}). \
                         복구를 실행하십시오."
                    ),
                    NextAction::Retry,
                ))
            }
        }
    }

    /// From ChooseOutput straight to Ready when the settings name an output
    /// that is present.
    fn resume_output(&mut self) -> Result<(), UserError> {
        if self.settings.output.is_none() {
            return Ok(());
        }
        let outputs = self.host.get_mut().outputs();
        if self.find_output(&outputs).is_some() {
            self.phase = Phase::Ready;
            Ok(())
        } else {
            Err(self.output_missing())
        }
    }

    fn install_driver(&mut self) -> Result<(), UserError> {
        let driver = self.host.get_mut().driver();
        let task = if driver == DriverState::NotInstalled {
            SetupTask::Install
        } else {
            SetupTask::Repair
        };
        self.driver = Some(driver);
        match self.host.get_mut().run_setup(&task) {
            Err(HostError::Declined) => {
                Err(user_error("관리자 승인을 취소했습니다.", NextAction::Retry))
            }
            Err(HostError::Setup(message)) => Err(user_error(
                format!("드라이버 설치에 실패했습니다: {message}"),
                NextAction::Retry,
            )),
            Err(error) => Err(user_error(
                format!("드라이버 설치에 실패했습니다: {error}"),
                NextAction::Retry,
            )),
            Ok(outcome) => {
                let message = outcome.message.trim();
                if !message.is_empty() {
                    self.notes.push(format!("설치 도우미: {message}"));
                }
                if outcome.reboot_required {
                    self.phase = Phase::Setup(SetupStep::RebootRequired);
                    Err(reboot_required())
                } else {
                    self.evaluate_driver()
                }
            }
        }
    }

    fn choose_output(&mut self, id: &DeviceId) -> Result<(), UserError> {
        let outputs = self.host.get_mut().outputs();
        let Some(output) = outputs.iter().find(|output| output.id == *id) else {
            return Err(user_error(
                "고른 출력 장치를 찾을 수 없습니다.",
                NextAction::None,
            ));
        };
        if output.endpoint.is_some() {
            return Err(user_error(
                "Spatial SoftReceiver 자신을 출력으로 고를 수 없습니다.",
                NextAction::None,
            ));
        }
        if output.state != DeviceState::Active {
            return Err(user_error(
                "고른 출력 장치가 꺼져 있거나 연결되어 있지 않습니다.",
                NextAction::None,
            ));
        }
        self.settings.output = Some(OutputChoice {
            id: output.id.clone(),
            name: output.name.clone(),
        });
        self.phase = Phase::Ready;
        Ok(())
    }

    /// Takes the roles, then starts the engine; on any failure the roles go
    /// back and the phase stays where it was (or returns to ChooseOutput when
    /// the chosen output is gone).
    fn enable(&mut self) -> Result<(), UserError> {
        self.notes.clear();
        let outputs = self.host.get_mut().outputs();
        let Some(ours) = outputs
            .iter()
            .find(|output| output.endpoint == Some(0) && output.state == DeviceState::Active)
            .map(|output| output.id.clone())
        else {
            self.driver = Some(self.host.get_mut().driver());
            return Err(user_error(
                "주 엔드포인트가 보이지 않습니다. 드라이버 상태를 확인하십시오.",
                NextAction::Retry,
            ));
        };
        let Some(output) = self.find_output(&outputs) else {
            self.phase = Phase::Setup(SetupStep::ChooseOutput);
            return Err(self.output_missing());
        };
        // The profile is resolved before the roles move, so a broken
        // profile.toml or a BRIR set that went missing changes nothing.
        let resolved = resolve(&self.config, &self.settings.profile)?;
        self.profile = resolved.view;
        self.take_roles(ours, output.id.clone())?;
        match self.launcher.launch(&output, &resolved.profile) {
            Ok(handle) => {
                self.engine = Some(Running {
                    handle,
                    profile: resolved.profile,
                    needs_prepared: false,
                    pending: None,
                });
                self.phase = Phase::Running;
                Ok(())
            }
            Err(error) => {
                self.restore();
                Err(user_error(
                    format!("엔진을 시작하지 못했습니다: {error}"),
                    NextAction::Retry,
                ))
            }
        }
    }

    /// Writes the restore point (with the chosen output as the fallback), then
    /// points the three roles at `ours` in [`Role::ALL`] order. A failure gives
    /// back what was already taken.
    fn take_roles(&mut self, ours: DeviceId, fallback: DeviceId) -> Result<(), UserError> {
        let host = self.host.get_mut();
        let point = RestorePoint {
            ours,
            previous: host.default_roles(),
            fallback: Some(fallback),
            app_pid: std::process::id(),
        };
        host.write_restore_point(Some(&point.to_json()))
            .map_err(|error| role_change_failed(&error))?;
        for role in Role::ALL {
            if let Err(error) = host.set_default_role(role, &point.ours) {
                let notes = restore_point::restore(&point, host);
                self.notes.extend(notes);
                return Err(role_change_failed(&error));
            }
        }
        self.taken = Some(point);
        Ok(())
    }

    /// Gives the roles back when they were taken (ADR 0023) and removes the
    /// restore point.
    fn restore(&mut self) {
        if let Some(point) = self.taken.take() {
            let notes = restore_point::restore(&point, self.host.get_mut());
            self.notes.extend(notes);
        }
    }

    /// The engine reconnected what it lost: the wait ends with a note.
    fn reconnected(&mut self) {
        if self.reconnecting.take().is_some() {
            self.error = None;
            self.notes.push("다시 연결했습니다.".to_owned());
        }
    }

    /// Stops the engine, gives the roles back and moves to Faulted with
    /// `error`.
    fn fault(&mut self, error: UserError) {
        self.stop_engine();
        self.restore();
        self.phase = Phase::Faulted;
        self.error = Some(error);
    }

    /// Stops the engine if it runs; its error is a note, not a failure.
    fn stop_engine(&mut self) {
        self.reconnecting = None;
        if let Some(running) = self.engine.take()
            && let Err(error) = running.handle.stop()
        {
            self.notes
                .push(format!("엔진을 멈추다가 오류가 났습니다: {error}"));
        }
    }

    /// Finds the chosen output: by id among the outputs the user may choose;
    /// else by name when exactly one of them has it, remembering the new id
    /// (with a note). A name shared by two outputs is never guessed.
    fn find_output(&mut self, outputs: &[OutputDevice]) -> Option<OutputDevice> {
        let choice = self.settings.output.as_mut()?;
        if let Some(found) = outputs
            .iter()
            .find(|output| is_choosable(output) && output.id == choice.id)
        {
            // The name is the fallback for the next id change; keep it current.
            choice.name.clone_from(&found.name);
            return Some(found.clone());
        }
        let mut named = outputs
            .iter()
            .filter(|output| is_choosable(output) && output.name == choice.name);
        let (Some(found), None) = (named.next(), named.next()) else {
            return None;
        };
        choice.id = found.id.clone();
        self.notes.push(format!(
            "출력 장치의 식별자가 바뀌어 이름으로 다시 찾았습니다: {}",
            found.name
        ));
        Some(found.clone())
    }

    /// The profile the next launch uses: the state directory's
    /// `profile.toml` when it exists (with the saved gain), else headphones
    /// from the imported BRIR set, else [`default_profile`] (with the saved
    /// gain). Reads the disk; the snapshot uses the cached view instead.
    pub fn output_profile(&self) -> Result<OutputProfile, UserError> {
        resolve(&self.config, &self.settings.profile).map(|resolved| resolved.profile)
    }

    fn profile_file_active(&self) -> bool {
        profile_file(&self.config).is_some()
    }

    /// Refreshes the cached profile when `profile.toml` appears or disappears.
    /// Stable file presence does no disk work; edits to an existing file are
    /// picked up by Enable or re-prepare as before.
    pub fn watch_profile_file(&mut self) {
        if self.profile_file_active() != self.profile.from_file {
            self.refresh_profile();
        }
    }

    /// Resolves the profile once at start for the snapshot. A profile that
    /// does not resolve keeps what the settings say, with no directions, and
    /// becomes a note; enabling then fails with the same sentence.
    fn refresh_profile(&mut self) {
        match resolve(&self.config, &self.settings.profile) {
            Ok(resolved) => self.profile = resolved.view,
            Err(error) => {
                self.profile = unresolved_view(&self.config, &self.settings.profile);
                self.notes.push(error.message);
            }
        }
    }

    fn import_brir(&mut self, path: PathBuf) -> Result<(), UserError> {
        if self.profile_file_active() {
            return Err(profile_file_error());
        }
        if brir_source(&path).is_none() {
            return Err(invalid_brir_error());
        }
        let mut settings = self.settings.profile.clone();
        settings.brir = Some(path);
        self.change_profile(settings)
    }

    fn import_headphone_fir(&mut self, path: PathBuf) -> Result<(), UserError> {
        if self.profile_file_active() {
            return Err(profile_file_error());
        }
        if self.settings.profile.brir.is_none() {
            return Err(user_error(
                "먼저 BRIR 세트를 가져오십시오.",
                NextAction::None,
            ));
        }
        brir::check_headphone_fir(&path).map_err(|error| {
            user_error(
                format!("헤드폰 FIR을 읽지 못했습니다: {error}"),
                NextAction::None,
            )
        })?;
        let mut settings = self.settings.profile.clone();
        settings.headphone_fir = Some(path);
        self.change_profile(settings)
    }

    /// Resolves `settings`; with an engine running, prepares the result on
    /// this thread (never the engine's real-time threads) and hands it over
    /// as one [`LiveChange::Prepared`]. Only then are the settings, the view
    /// and the profile a re-prepare builds from kept: any failure leaves
    /// them and the running output as they were.
    fn change_profile(&mut self, settings: ProfileSettings) -> Result<(), UserError> {
        let resolved = resolve(&self.config, &settings)?;
        if let Some(running) = self.engine.as_mut() {
            let prepared = resolved
                .profile
                .prepare()
                .map_err(|error| live_error(SessionError::Prepare(error)))?;
            running
                .handle
                .apply(LiveChange::Prepared(Box::new(prepared)))
                .map_err(live_error)?;
            running.profile = resolved.profile;
            running.needs_prepared = false;
            running.pending = None;
        }
        self.settings.profile = settings;
        self.profile = resolved.view;
        Ok(())
    }

    fn set_gain(&mut self, db: f32) -> Result<(), UserError> {
        if !db.is_finite() || !(GAIN_MIN_DB..=GAIN_MAX_DB).contains(&db) {
            return Err(user_error(
                "게인은 -20 dB에서 +10 dB 사이입니다.",
                NextAction::None,
            ));
        }
        if let Some(running) = self.engine.as_mut() {
            running
                .handle
                .apply(LiveChange::Gain(db))
                .map_err(live_error)?;
            set_gain_db(&mut running.profile, db);
        }
        self.settings.profile.gain_db = Some(db);
        self.profile.gain_db = db;
        Ok(())
    }

    /// The direction must be in the cached profile's layout. That is the
    /// layout of the last profile handed to the engine, which is also the
    /// one the engine checks a test tone against (the last `Prepared` it
    /// queued, installed or not): the header-only layout equals `prepare`'s
    /// (ADR 0016, 2026-10-15 amendment), and the view changes only when the
    /// engine took the profile.
    fn test_direction(&mut self, id: DirectionId) -> Result<(), UserError> {
        if id.index() >= self.profile.directions.len() {
            return Err(user_error("그 방향이 없습니다.", NextAction::None));
        }
        self.engine
            .as_mut()
            .expect("Running has an engine")
            .handle
            .apply(LiveChange::TestTone(id))
            .map_err(live_error)
    }

    fn output_missing(&self) -> UserError {
        let name = self
            .settings
            .output
            .as_ref()
            .map_or("", |choice| choice.name.as_str());
        user_error(
            format!("저장된 출력 장치를 찾을 수 없습니다: {name}"),
            NextAction::OpenSettings,
        )
    }
}

/// The gain range of [`Intent::SetGain`], the engine's own.
const GAIN_MIN_DB: f32 = -20.0;
const GAIN_MAX_DB: f32 = 10.0;

/// The profile without a `profile.toml` or an imported BRIR set: two
/// full-range speakers at ±30° on the device's first two channels, so every
/// source is panned to stereo and the app makes sound from the first day.
pub fn default_profile() -> OutputProfile {
    let speaker = |name: &str, azimuth_deg: f32, channel: usize| SpeakerConfig {
        name: name.into(),
        azimuth_deg,
        elevation_deg: 0.0,
        channel,
        distance_m: None,
        delay_ms: None,
        trim_db: 0.0,
        range: SpeakerRange::Full,
        crossover_hz: None,
        fir: None,
    };
    OutputProfile::Speakers(SpeakerProfile {
        name: "Stereo".into(),
        device_channels: 2,
        channel_mask: 3,
        gain_db: 0.0,
        limiter_ceiling_dbfs: -1.0,
        crossover_hz: 80.0,
        lfe_gain_db: 10.0,
        speakers: vec![speaker("FL", 30.0, 0), speaker("FR", -30.0, 1)],
        subwoofers: Vec::new(),
        bass_to: Vec::new(),
    })
}

fn set_gain_db(profile: &mut OutputProfile, db: f32) {
    match profile {
        OutputProfile::Headphones(profile) => profile.gain_db = db,
        OutputProfile::Speakers(profile) => profile.gain_db = db,
    }
}

/// [`default_profile`] as the screen sees it, for snapshots taken without a
/// supervisor.
pub fn default_profile_view() -> ProfileView {
    let OutputProfile::Speakers(profile) = default_profile() else {
        unreachable!("the default profile is a speaker profile");
    };
    speakers_view(&profile, ProfileKind::Stereo, false)
}

/// A resolved profile and its view.
struct Resolved {
    profile: OutputProfile,
    view: ProfileView,
}

fn profile_file(config: &Config) -> Option<&Path> {
    config.profile_file.as_deref().filter(|path| path.is_file())
}

/// The precedence of [`Supervisor::output_profile`], reading only headers.
fn resolve(config: &Config, settings: &ProfileSettings) -> Result<Resolved, UserError> {
    if let Some(path) = profile_file(config) {
        let mut profile = engine::load_output_profile(path).map_err(|error| {
            user_error(
                format!("profile.toml을 읽지 못했습니다: {error}"),
                NextAction::None,
            )
        })?;
        let view = match &mut profile {
            OutputProfile::Headphones(profile) => {
                if let Some(db) = settings.gain_db {
                    profile.gain_db = db;
                }
                headphones_view(profile, true)?
            }
            OutputProfile::Speakers(profile) => {
                if let Some(db) = settings.gain_db {
                    profile.gain_db = db;
                }
                speakers_view(profile, ProfileKind::Speakers, true)
            }
        };
        return Ok(Resolved { profile, view });
    }
    if let Some(path) = &settings.brir {
        let source = brir_source(path).ok_or_else(invalid_brir_error)?;
        let mut profile = brir::Profile {
            name: brir_name(path),
            brir: source,
            headphone_fir: settings.headphone_fir.clone(),
            gain_db: settings.gain_db.unwrap_or(0.0),
            limiter_ceiling_dbfs: -1.0,
            lfe: brir::LfePolicy::Mute,
            angles: brir::AnglePreset::Standard,
            angle_overrides: Vec::new(),
            truncate: None,
            trim_leading_delay: true,
        };
        let layout = brir::layout(&profile).map_err(brir_error)?;
        // The build logs routed LFE into the centre at 0 dB.
        if layout.find("FC").is_some() {
            profile.lfe = brir::LfePolicy::Direction {
                name: "FC".into(),
                gain_db: 0.0,
            };
        }
        let view = ProfileView {
            kind: ProfileKind::Headphones,
            name: profile.name.clone(),
            brir: Some(display(path)),
            headphone_fir: profile.headphone_fir.as_deref().map(display),
            gain_db: profile.gain_db,
            directions: direction_views(&layout),
            from_file: false,
        };
        return Ok(Resolved {
            profile: OutputProfile::Headphones(profile),
            view,
        });
    }
    let mut profile = default_profile();
    let OutputProfile::Speakers(speakers) = &mut profile else {
        unreachable!("the default profile is a speaker profile");
    };
    speakers.gain_db = settings.gain_db.unwrap_or(0.0);
    let view = speakers_view(speakers, ProfileKind::Stereo, false);
    Ok(Resolved { profile, view })
}

/// What the view says when the profile does not resolve.
fn unresolved_view(config: &Config, settings: &ProfileSettings) -> ProfileView {
    let from_file = profile_file(config).is_some();
    let headphones = !from_file && settings.brir.is_some();
    ProfileView {
        kind: if from_file {
            ProfileKind::Speakers
        } else if headphones {
            ProfileKind::Headphones
        } else {
            ProfileKind::Stereo
        },
        name: match (&settings.brir, from_file) {
            (_, true) => "profile.toml".to_owned(),
            (Some(path), false) => brir_name(path),
            (None, false) => "Stereo".to_owned(),
        },
        brir: settings.brir.as_deref().filter(|_| headphones).map(display),
        headphone_fir: settings
            .headphone_fir
            .as_deref()
            .filter(|_| headphones)
            .map(display),
        // The file's own gain is unknown when the file does not load.
        gain_db: settings.gain_db.unwrap_or(0.0),
        directions: Vec::new(),
        from_file,
    }
}

fn headphones_view(profile: &brir::Profile, from_file: bool) -> Result<ProfileView, UserError> {
    let layout = brir::layout(profile).map_err(brir_error)?;
    let brir = match &profile.brir {
        brir::BrirSource::Hesuvi(path)
        | brir::BrirSource::Hrir(path)
        | brir::BrirSource::Folder(path)
        | brir::BrirSource::Mapped { wav: path, .. } => Some(display(path)),
        brir::BrirSource::Directions(_) => None,
    };
    Ok(ProfileView {
        kind: ProfileKind::Headphones,
        name: profile.name.clone(),
        brir,
        headphone_fir: profile.headphone_fir.as_deref().map(display),
        gain_db: profile.gain_db,
        directions: direction_views(&layout),
        from_file,
    })
}

/// Speakers in profile order, which is the order `speakers::prepare` lays
/// them out in.
fn speakers_view(profile: &SpeakerProfile, kind: ProfileKind, from_file: bool) -> ProfileView {
    ProfileView {
        kind,
        name: profile.name.clone(),
        brir: None,
        headphone_fir: None,
        gain_db: profile.gain_db,
        directions: profile
            .speakers
            .iter()
            .enumerate()
            .map(|(index, speaker)| DirectionView {
                id: direction_id(index),
                name: speaker.name.clone(),
                azimuth_deg: speaker.azimuth_deg,
                elevation_deg: speaker.elevation_deg,
            })
            .collect(),
        from_file,
    }
}

fn direction_views(layout: &Layout) -> Vec<DirectionView> {
    layout
        .directions()
        .iter()
        .enumerate()
        .map(|(index, direction)| DirectionView {
            id: direction_id(index),
            name: direction.name.clone(),
            azimuth_deg: direction.azimuth_deg,
            elevation_deg: direction.elevation_deg,
        })
        .collect()
}

fn direction_id(index: usize) -> DirectionId {
    DirectionId(u16::try_from(index).expect("a layout has far fewer than 65536 directions"))
}

/// The BRIR source an imported path names: a folder of direction WAVs,
/// `hesuvi.wav`, or `hrir.wav` (any case).
fn brir_source(path: &Path) -> Option<brir::BrirSource> {
    if path.is_dir() {
        return Some(brir::BrirSource::Folder(path.to_owned()));
    }
    let name = path.file_name()?.to_str()?;
    if name.eq_ignore_ascii_case("hesuvi.wav") {
        Some(brir::BrirSource::Hesuvi(path.to_owned()))
    } else if name.eq_ignore_ascii_case("hrir.wav") {
        Some(brir::BrirSource::Hrir(path.to_owned()))
    } else {
        None
    }
}

/// A folder's name, or the name of the folder holding `hesuvi.wav` or
/// `hrir.wav` (what the measurement was saved as).
fn brir_name(path: &Path) -> String {
    let named = if path.is_dir() {
        Some(path)
    } else {
        path.parent()
    };
    named
        .and_then(Path::file_name)
        .or_else(|| path.file_stem())
        .map_or_else(
            || "BRIR".to_owned(),
            |name| name.to_string_lossy().into_owned(),
        )
}

fn display(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn profile_file_error() -> UserError {
    user_error(
        "상태 폴더의 profile.toml이 우선합니다. 그 파일을 지우거나 고치십시오.",
        NextAction::None,
    )
}

fn invalid_brir_error() -> UserError {
    user_error(
        "hesuvi.wav, hrir.wav 또는 방향별 WAV 폴더를 고르십시오.",
        NextAction::None,
    )
}

fn brir_error(error: brir::BrirError) -> UserError {
    user_error(
        format!("BRIR 세트를 읽지 못했습니다: {error}"),
        NextAction::None,
    )
}

/// A change the running engine did not take; the output is as it was.
fn live_error(error: SessionError) -> UserError {
    match error {
        SessionError::Prepare(error) => user_error(
            format!("프로필을 준비하지 못했습니다: {error}"),
            NextAction::None,
        ),
        SessionError::Rejected => user_error(
            "엔진이 변경을 받지 못했습니다. 잠시 뒤 다시 시도하십시오.",
            NextAction::Retry,
        ),
        error => user_error(
            format!("엔진에 변경을 적용하지 못했습니다: {error}"),
            NextAction::Retry,
        ),
    }
}

/// What one intent did. Only a failure is kept in the snapshot: a refusal
/// changed nothing, so the snapshot keeps the step's explanation.
enum Outcome {
    Done,
    Failed(UserError),
    Refused(UserError),
}

/// A lost output or capture the engine is reconnecting, since its first
/// notice.
struct Reconnecting {
    since: Instant,
    fault: EngineFault,
}

fn refused() -> Outcome {
    Outcome::Refused(not_allowed())
}

fn reconnecting_error(fault: &EngineFault) -> UserError {
    let message = match fault {
        EngineFault::OutputLost(detail) => {
            format!("출력 장치와의 연결이 끊어져 다시 연결하는 중입니다: {detail}")
        }
        EngineFault::CaptureLost(_) => {
            "캡처 장치와의 연결이 끊어져 다시 연결하는 중입니다.".to_owned()
        }
    };
    user_error(message, NextAction::None)
}

/// Installed without a problem, running, and not missing the exclusive
/// policy (an unknown policy counts as fine, as in `evaluate_driver`).
fn driver_is_fine(driver: &DriverState) -> bool {
    matches!(
        driver,
        DriverState::Installed {
            problem_code: 0,
            service_running: true,
            exclusive_policy,
        } if *exclusive_policy != PolicyState::Missing
    )
}

/// An output the user may choose: active, and not one of ours.
fn is_choosable(output: &OutputDevice) -> bool {
    output.state == DeviceState::Active && output.endpoint.is_none()
}

fn user_error(message: impl Into<String>, next: NextAction) -> UserError {
    UserError {
        message: message.into(),
        next,
    }
}

fn not_allowed() -> UserError {
    user_error("지금은 할 수 없는 동작입니다.", NextAction::None)
}

fn reboot_required() -> UserError {
    user_error(
        "드라이버 설치를 마치려면 다시 시작해야 합니다.",
        NextAction::Reboot,
    )
}

fn role_change_failed(error: &HostError) -> UserError {
    user_error(
        format!("기본 장치를 바꾸지 못했습니다: {error}"),
        NextAction::Retry,
    )
}

fn os_requirement(min_os_build: u32) -> String {
    if min_os_build == Config::MIN_OS_BUILD {
        "Windows 10 2004(빌드 19041) 이상이 필요합니다.".to_owned()
    } else {
        format!("Windows 빌드 {min_os_build} 이상이 필요합니다.")
    }
}

fn arch_name(arch: Arch) -> &'static str {
    match arch {
        Arch::X64 => "x64",
        Arch::Arm64 => "Arm64",
        Arch::Other => "다른 아키텍처",
    }
}

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

use std::cell::RefCell;
use std::fmt;
use std::time::{Duration, Instant};

use ssr_types::app::{Intent, NextAction, OutputChoice, Phase, SetupStep, Snapshot, UserError};
use ssr_types::{
    Arch, DeviceId, DeviceState, DriverState, EngineFault, EngineStatus, Host, HostError,
    OutputDevice, Platform, Role, SecureBoot, SetupTask,
};

use crate::restore_point::{self, RestorePoint};
use crate::session::{Session, SessionError, SessionNotice};

/// A running engine as the state machine sees it.
pub trait EngineHandle {
    fn poll(&mut self) -> Result<Vec<SessionNotice>, SessionError>;
    fn status(&self) -> EngineStatus;
    fn stop(self: Box<Self>) -> Result<(), SessionError>;
}

impl EngineHandle for Session {
    fn poll(&mut self) -> Result<Vec<SessionNotice>, SessionError> {
        Session::poll(self)
    }

    fn status(&self) -> EngineStatus {
        Session::status(self)
    }

    /// Discards the report: the shell has no use for it yet.
    fn stop(self: Box<Self>) -> Result<(), SessionError> {
        Session::stop(*self).map(drop)
    }
}

/// Starts the engine for the state machine.
pub trait Launcher {
    /// Starts the engine on `output` (an active device that is not ours).
    fn launch(&mut self, output: &OutputDevice) -> Result<Box<dyn EngineHandle>, SessionError>;
}

/// Settings the shell persists; the machine reads and updates them.
///
/// The shell persists [`Supervisor::settings`] after [`Supervisor::new`] and
/// after every [`Supervisor::handle`] that returns `Ok`, because both can
/// rewrite the stored id (an output found again by its name). A `handle` that
/// fails after such a rewrite leaves the old id persisted, which the name finds
/// again next time.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Settings {
    /// The chosen output, remembered by id and name: the id can change across
    /// reboots, the name is the fallback.
    pub output: Option<OutputChoice>,
}

/// What this build needs from the PC.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
    engine: Option<Box<dyn EngineHandle>>,
    /// The engine reported a lost output or capture and is reconnecting.
    reconnecting: Option<Reconnecting>,
    /// The restore point written when the roles were taken; `Some` exactly
    /// while they are.
    taken: Option<RestorePoint>,
    driver: Option<DriverState>,
    platform: Option<Platform>,
    error: Option<UserError>,
    notes: Vec<String>,
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
    /// without a problem and the settings name an output that is present, else
    /// in Setup(Welcome). Never takes the roles here.
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
        };
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
    /// A lost output or capture is the engine's to reconnect: it retries
    /// without limit and says `FaultCleared` when it succeeds. So a `Fault`
    /// notice keeps Running with an error that says it is reconnecting, and
    /// `FaultCleared` clears it with a note. A fault that outlasts
    /// [`Config::fault_grace`] (from its first notice, by `now`), or a poll
    /// error, stops the engine, gives the roles back and moves to Faulted.
    pub fn tick(&mut self, now: Instant) -> Vec<SessionNotice> {
        let Some(engine) = self.engine.as_mut() else {
            return Vec::new();
        };
        let notices = match engine.poll() {
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
                _ => {}
            }
        }
        if let Some(reconnecting) = &self.reconnecting
            && now.saturating_duration_since(reconnecting.since) >= self.config.fault_grace
        {
            let error = user_error(
                format!(
                    "엔진이 {}초 동안 다시 연결하지 못했습니다: {:?}",
                    self.config.fault_grace.as_secs(),
                    reconnecting.fault
                ),
                NextAction::Retry,
            );
            self.fault(error);
        }
        notices
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
            engine: self.engine.as_ref().map(|engine| engine.status()),
            outputs,
            chosen_output: self.settings.output.clone(),
            driver: self.driver.clone(),
            platform: self.platform.clone(),
            roles_taken: self.taken.is_some(),
            error: self.error.clone(),
            notes: self.notes.clone(),
        }
    }

    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    fn dispatch(&mut self, intent: Intent) -> Outcome {
        if self.phase == Phase::Quit {
            return refused();
        }
        let result = match intent {
            Intent::ImportBrir(_)
            | Intent::ImportHeadphoneFir(_)
            | Intent::SwitchProfile(_)
            | Intent::SetGain(_)
            | Intent::TestDirection(_)
            | Intent::StartDiagnosticRecording => {
                return Outcome::Refused(user_error(
                    "아직 지원하지 않는 동작입니다.",
                    NextAction::None,
                ));
            }
            Intent::BeginSetup => match self.phase {
                Phase::Setup(SetupStep::Welcome) => {
                    self.notes.clear();
                    self.evaluate()
                }
                _ => return refused(),
            },
            Intent::Recheck => match self.phase {
                Phase::Setup(
                    SetupStep::Unsupported
                    | SetupStep::NotTestSigning
                    | SetupStep::RebootRequired
                    | SetupStep::Driver,
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
        self.take_roles(ours, output.id.clone())?;
        match self.launcher.launch(&output) {
            Ok(engine) => {
                self.engine = Some(engine);
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
        if let Some(engine) = self.engine.take()
            && let Err(error) = engine.stop()
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

fn driver_is_fine(driver: &DriverState) -> bool {
    *driver
        == DriverState::Installed {
            problem_code: 0,
            service_running: true,
        }
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

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
    Intent, Language, NextAction, OutputChoice, Phase, ProfileKind, ProfileView, SetupStep,
    Snapshot, UserError,
};
use ssr_types::ports::PolicyState;
use ssr_types::{
    Arch, DeviceId, DeviceState, DirectionId, DirectionView, DriverState, EngineFault,
    EngineStatus, Host, HostError, Layout, OutputDevice, Platform, Role, SetupTask,
};

use crate::restore_point::{self, RestorePoint};
use crate::session::{Session, SessionError, SessionNotice};
use crate::text::Message;

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
#[derive(Clone, Debug)]
struct LocalizedError {
    message: Message,
    next: NextAction,
}

impl LocalizedError {
    fn render(&self, language: Language) -> UserError {
        UserError {
            message: self.message.render(language),
            next: self.next,
        }
    }
}

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
    /// `None` follows the language resolved by the shell from the OS.
    #[serde(default)]
    pub language: Option<Language>,
    /// The chosen output, remembered by id and name: the id can change across
    /// reboots, the name is the fallback.
    pub output: Option<OutputChoice>,
    #[serde(default)]
    pub profile: ProfileSettings,
}

/// What this build needs from the PC.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    /// Language resolved from the OS by the shell. Korean is the portable
    /// default and keeps legacy behavior.
    pub default_language: Language,
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
            default_language: Language::Ko,
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
    error_message: Option<LocalizedError>,
    notes: Vec<Message>,
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
        let language = settings.language.unwrap_or(config.default_language);
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
            error_message: None,
            notes: Vec::new(),
            shell_notes: Vec::new(),
            profile: default_profile_view_for(language),
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
        let language_change = matches!(&intent, Intent::SetLanguage(_));
        let previous_message = self.error_message.clone();
        match self.dispatch(intent) {
            Outcome::Done => {
                if language_change {
                    self.error = self
                        .error_message
                        .as_ref()
                        .map(|error| error.render(self.language()));
                } else {
                    self.error = None;
                    self.error_message = None;
                }
                Ok(())
            }
            Outcome::Failed(error) => {
                self.error = Some(error.clone());
                Err(error)
            }
            Outcome::Refused(error) => {
                self.error_message = previous_message;
                Err(error)
            }
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
                let error = self.message_error(
                    Message::EngineStopped {
                        detail: error.to_string(),
                    },
                    NextAction::Retry,
                );
                self.fault(error);
                return Vec::new();
            }
        };
        for notice in &notices {
            match notice {
                SessionNotice::Fault(fault) => {
                    self.error =
                        Some(self.message_error(reconnecting_message(fault), NextAction::None));
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
            let message = match &reconnecting.fault {
                EngineFault::OutputLost(detail) => Message::ReconnectTimedOutOutput {
                    seconds: self.config.fault_grace.as_secs(),
                    detail: detail.clone(),
                },
                EngineFault::CaptureLost(_) => Message::ReconnectTimedOutCapture {
                    seconds: self.config.fault_grace.as_secs(),
                },
            };
            let error = self.message_error(message, NextAction::Retry);
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
                    let error = self.message_error(
                        Message::EngineStopped {
                            detail: SessionError::from(error).to_string(),
                        },
                        NextAction::Retry,
                    );
                    self.fault(error);
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
                let error = self.message_error(
                    Message::EngineStopped {
                        detail: error.to_string(),
                    },
                    NextAction::Retry,
                );
                self.fault(error);
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
        let language = self.language();
        Snapshot {
            language,
            language_choice: self.settings.language,
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
                .cloned()
                .chain(self.notes.iter().map(|message| self.say(message.clone())))
                .collect(),
        }
    }

    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    pub fn language(&self) -> Language {
        self.settings
            .language
            .unwrap_or(self.config.default_language)
    }

    fn say(&self, message: Message) -> String {
        message.render(self.language())
    }

    fn message_error(&mut self, message: Message, next: NextAction) -> UserError {
        self.localized_error(LocalizedError { message, next })
    }

    fn localized_error(&mut self, error: LocalizedError) -> UserError {
        let rendered = error.render(self.language());
        self.error_message = Some(error);
        rendered
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
            return self.refused();
        }
        let result = match intent {
            Intent::SetLanguage(language) => {
                self.settings.language = language;
                if self.profile.kind == ProfileKind::Stereo && !self.profile.from_file {
                    self.profile.name = self.say(Message::DefaultProfileName);
                }
                Ok(())
            }
            // The profile can change whenever the wizard is done; a running
            // engine takes the change live, all or nothing.
            Intent::ImportBrir(path) => match self.phase {
                Phase::Ready | Phase::Running | Phase::Faulted => self.import_brir(path),
                _ => return self.refused(),
            },
            Intent::ImportHeadphoneFir(path) => match self.phase {
                Phase::Ready | Phase::Running | Phase::Faulted => self.import_headphone_fir(path),
                _ => return self.refused(),
            },
            Intent::SetGain(db) => match self.phase {
                Phase::Ready | Phase::Running | Phase::Faulted => self.set_gain(db),
                _ => return self.refused(),
            },
            Intent::TestDirection(id) => match self.phase {
                Phase::Running => self.test_direction(id),
                Phase::Ready | Phase::Faulted => {
                    return Outcome::Refused(
                        self.message_error(Message::TestAfterEnable, NextAction::None),
                    );
                }
                _ => return self.refused(),
            },
            Intent::StartDiagnosticRecording => {
                return Outcome::Refused(
                    self.message_error(Message::DiagnosticRecordingUnavailable, NextAction::None),
                );
            }
            Intent::BeginSetup => match self.phase {
                Phase::Setup(SetupStep::Welcome) => {
                    self.notes.clear();
                    self.shell_notes.clear();
                    self.evaluate()
                }
                _ => return self.refused(),
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
                _ => return self.refused(),
            },
            Intent::InstallDriver => match self.phase {
                Phase::Setup(SetupStep::Driver) => self.install_driver(),
                _ => return self.refused(),
            },
            Intent::ChooseOutput(id) => match self.phase {
                Phase::Setup(SetupStep::ChooseOutput) | Phase::Ready => self.choose_output(&id),
                Phase::Running => {
                    Err(self.message_error(Message::DisableBeforeChangingOutput, NextAction::None))
                }
                _ => return self.refused(),
            },
            Intent::Enable => match self.phase {
                Phase::Ready | Phase::Faulted => self.enable(),
                _ => return self.refused(),
            },
            Intent::Disable => match self.phase {
                Phase::Running => {
                    self.stop_engine();
                    self.restore();
                    self.phase = Phase::Ready;
                    Ok(())
                }
                _ => return self.refused(),
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
            return Err(self.message_error(
                Message::OsRequirement {
                    min_build: self.config.min_os_build,
                },
                NextAction::None,
            ));
        }
        if platform.arch != self.config.arch {
            self.phase = Phase::Setup(SetupStep::Unsupported);
            return Err(self.message_error(
                Message::ArchitectureMismatch {
                    build: self.config.arch,
                    pc: platform.arch,
                },
                NextAction::None,
            ));
        }
        if self.config.requires_test_signing && !platform.test_signing {
            self.phase = Phase::Setup(SetupStep::NotTestSigning);
            return Err(self.message_error(
                Message::TestSigningDisabled {
                    secure_boot: platform.secure_boot,
                },
                NextAction::Reboot,
            ));
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
                Err(self.message_error(Message::RebootRequired, NextAction::Reboot))
            }
            DriverState::Installed {
                problem_code: 52, ..
            } => {
                self.phase = Phase::Setup(SetupStep::Driver);
                Err(self.message_error(Message::DriverSignatureRejected, NextAction::Retry))
            }
            DriverState::Installed {
                problem_code: 0,
                service_running: false,
                ..
            } => {
                self.phase = Phase::Setup(SetupStep::Driver);
                Err(self.message_error(Message::DriverServiceStopped, NextAction::Retry))
            }
            DriverState::Installed {
                problem_code: 0,
                service_running: true,
                exclusive_policy: PolicyState::Missing,
            } => {
                self.phase = Phase::Setup(SetupStep::Driver);
                Err(self.message_error(Message::ExclusivePolicyMissing, NextAction::Retry))
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
                Err(self.message_error(
                    Message::DriverProblem { code: problem_code },
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
                Err(self.message_error(Message::AdminApprovalCancelled, NextAction::Retry))
            }
            Err(HostError::Setup(message)) => Err(self.message_error(
                Message::DriverInstallFailed { detail: message },
                NextAction::Retry,
            )),
            Err(error) => Err(self.message_error(
                Message::DriverInstallFailed {
                    detail: error.to_string(),
                },
                NextAction::Retry,
            )),
            Ok(outcome) => {
                let message = outcome.message.trim();
                if !message.is_empty() {
                    self.notes.push(Message::SetupHelper {
                        detail: message.to_owned(),
                    });
                }
                if outcome.reboot_required {
                    self.phase = Phase::Setup(SetupStep::RebootRequired);
                    Err(self.message_error(Message::RebootRequired, NextAction::Reboot))
                } else {
                    self.evaluate_driver()
                }
            }
        }
    }

    fn choose_output(&mut self, id: &DeviceId) -> Result<(), UserError> {
        let outputs = self.host.get_mut().outputs();
        let Some(output) = outputs.iter().find(|output| output.id == *id) else {
            return Err(self.message_error(Message::ChosenOutputMissing, NextAction::None));
        };
        if output.endpoint.is_some() {
            return Err(self.message_error(Message::OwnOutputChosen, NextAction::None));
        }
        if output.state != DeviceState::Active {
            return Err(self.message_error(Message::ChosenOutputInactive, NextAction::None));
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
            return Err(self.message_error(Message::MainEndpointMissing, NextAction::Retry));
        };
        let Some(output) = self.find_output(&outputs) else {
            self.phase = Phase::Setup(SetupStep::ChooseOutput);
            return Err(self.output_missing());
        };
        // The profile is resolved before the roles move, so a broken
        // profile.toml or a BRIR set that went missing changes nothing.
        let resolved = resolve(&self.config, &self.settings.profile, self.language())
            .map_err(|error| self.localized_error(error))?;
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
                Err(self.message_error(
                    Message::EngineStartFailed {
                        detail: error.to_string(),
                    },
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
        if let Err(error) = host.write_restore_point(Some(&point.to_json())) {
            return Err(self.message_error(
                Message::RoleChangeFailed {
                    detail: error.to_string(),
                },
                NextAction::Retry,
            ));
        }
        for role in Role::ALL {
            if let Err(error) = host.set_default_role(role, &point.ours) {
                let notes = restore_point::restore_messages(&point, host);
                self.notes.extend(notes);
                return Err(self.message_error(
                    Message::RoleChangeFailed {
                        detail: error.to_string(),
                    },
                    NextAction::Retry,
                ));
            }
        }
        self.taken = Some(point);
        Ok(())
    }

    /// Gives the roles back when they were taken (ADR 0023) and removes the
    /// restore point.
    fn restore(&mut self) {
        if let Some(point) = self.taken.take() {
            let notes = restore_point::restore_messages(&point, self.host.get_mut());
            self.notes.extend(notes);
        }
    }

    /// The engine reconnected what it lost: the wait ends with a note.
    fn reconnected(&mut self) {
        if self.reconnecting.take().is_some() {
            self.error = None;
            self.error_message = None;
            self.notes.push(Message::Reconnected);
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
            self.notes.push(Message::EngineStopFailed {
                detail: error.to_string(),
            });
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
        self.notes.push(Message::OutputIdChanged {
            name: found.name.clone(),
        });
        Some(found.clone())
    }

    /// The profile the next launch uses: the state directory's
    /// `profile.toml` when it exists (with the saved gain), else headphones
    /// from the imported BRIR set, else [`default_profile`] (with the saved
    /// gain). Reads the disk; the snapshot uses the cached view instead.
    pub fn output_profile(&self) -> Result<OutputProfile, UserError> {
        resolve(&self.config, &self.settings.profile, self.language())
            .map(|resolved| resolved.profile)
            .map_err(|error| error.render(self.language()))
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
        match resolve(&self.config, &self.settings.profile, self.language()) {
            Ok(resolved) => self.profile = resolved.view,
            Err(error) => {
                self.profile =
                    unresolved_view(&self.config, &self.settings.profile, self.language());
                self.notes.push(error.message);
            }
        }
    }

    fn import_brir(&mut self, path: PathBuf) -> Result<(), UserError> {
        if self.profile_file_active() {
            return Err(self.message_error(Message::ProfileFileTakesPrecedence, NextAction::None));
        }
        if brir_source(&path).is_none() {
            return Err(self.message_error(Message::InvalidBrirSource, NextAction::None));
        }
        let mut settings = self.settings.profile.clone();
        settings.brir = Some(path);
        self.change_profile(settings)
    }

    fn import_headphone_fir(&mut self, path: PathBuf) -> Result<(), UserError> {
        if self.profile_file_active() {
            return Err(self.message_error(Message::ProfileFileTakesPrecedence, NextAction::None));
        }
        if self.settings.profile.brir.is_none() {
            return Err(self.message_error(Message::ImportBrirFirst, NextAction::None));
        }
        if let Err(error) = brir::check_headphone_fir(&path) {
            return Err(self.message_error(
                Message::HeadphoneFirReadFailed {
                    detail: error.to_string(),
                },
                NextAction::None,
            ));
        }
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
        let resolved = resolve(&self.config, &settings, self.language())
            .map_err(|error| self.localized_error(error))?;
        if self.engine.is_some() {
            let prepared = match resolved.profile.prepare() {
                Ok(prepared) => prepared,
                Err(error) => {
                    return Err(self.localized_error(live_error(SessionError::Prepare(error))));
                }
            };
            let applied = self
                .engine
                .as_mut()
                .expect("checked above")
                .handle
                .apply(LiveChange::Prepared(Box::new(prepared)));
            if let Err(error) = applied {
                return Err(self.localized_error(live_error(error)));
            }
            let running = self.engine.as_mut().expect("checked above");
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
            return Err(self.message_error(Message::GainOutOfRange, NextAction::None));
        }
        if self.engine.is_some() {
            let applied = self
                .engine
                .as_mut()
                .expect("checked above")
                .handle
                .apply(LiveChange::Gain(db));
            if let Err(error) = applied {
                return Err(self.localized_error(live_error(error)));
            }
            set_gain_db(
                &mut self.engine.as_mut().expect("checked above").profile,
                db,
            );
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
            return Err(self.message_error(Message::DirectionMissing, NextAction::None));
        }
        let applied = self
            .engine
            .as_mut()
            .expect("Running has an engine")
            .handle
            .apply(LiveChange::TestTone(id));
        applied.map_err(|error| self.localized_error(live_error(error)))
    }

    fn refused(&mut self) -> Outcome {
        Outcome::Refused(self.message_error(Message::NotAllowed, NextAction::None))
    }

    fn output_missing(&mut self) -> UserError {
        let name = self
            .settings
            .output
            .as_ref()
            .map_or("", |choice| choice.name.as_str());
        self.message_error(
            Message::StoredOutputMissing {
                name: name.to_owned(),
            },
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
pub fn default_profile_view_for(language: Language) -> ProfileView {
    let OutputProfile::Speakers(profile) = default_profile() else {
        unreachable!("the default profile is a speaker profile");
    };
    let mut view = speakers_view(&profile, ProfileKind::Stereo, false);
    view.name = Message::DefaultProfileName.render(language);
    view
}

pub fn default_profile_view() -> ProfileView {
    default_profile_view_for(Language::Ko)
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
fn resolve(
    config: &Config,
    settings: &ProfileSettings,
    language: Language,
) -> Result<Resolved, LocalizedError> {
    if let Some(path) = profile_file(config) {
        let mut profile = engine::load_output_profile(path).map_err(|error| LocalizedError {
            message: Message::ProfileFileReadFailed {
                detail: error.to_string(),
            },
            next: NextAction::None,
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
    let mut view = speakers_view(speakers, ProfileKind::Stereo, false);
    view.name = Message::DefaultProfileName.render(language);
    Ok(Resolved { profile, view })
}

/// What the view says when the profile does not resolve.
fn unresolved_view(config: &Config, settings: &ProfileSettings, language: Language) -> ProfileView {
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
            (None, false) => Message::DefaultProfileName.render(language),
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

fn headphones_view(
    profile: &brir::Profile,
    from_file: bool,
) -> Result<ProfileView, LocalizedError> {
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

fn invalid_brir_error() -> LocalizedError {
    LocalizedError {
        message: Message::InvalidBrirSource,
        next: NextAction::None,
    }
}

fn brir_error(error: brir::BrirError) -> LocalizedError {
    LocalizedError {
        message: Message::BrirReadFailed {
            detail: error.to_string(),
        },
        next: NextAction::None,
    }
}

/// A change the running engine did not take; the output is as it was.
fn live_error(error: SessionError) -> LocalizedError {
    match error {
        SessionError::Prepare(error) => LocalizedError {
            message: Message::ProfilePrepareFailed {
                detail: error.to_string(),
            },
            next: NextAction::None,
        },
        SessionError::Rejected => LocalizedError {
            message: Message::EngineChangeRejected,
            next: NextAction::Retry,
        },
        error => LocalizedError {
            message: Message::EngineChangeFailed {
                detail: error.to_string(),
            },
            next: NextAction::Retry,
        },
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

fn reconnecting_message(fault: &EngineFault) -> Message {
    match fault {
        EngineFault::OutputLost(detail) => Message::ReconnectingOutput {
            detail: detail.clone(),
        },
        EngineFault::CaptureLost(_) => Message::ReconnectingCapture,
    }
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

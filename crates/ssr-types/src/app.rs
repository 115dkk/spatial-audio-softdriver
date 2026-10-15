//! Intent and snapshot: the whole contract between the screen and Rust.
//!
//! The screen sends only intents and draws only snapshots; it keeps no state of
//! its own, so reopening the window draws the same picture (ADR 0001). The
//! `supervisor` state machine consumes the intents and produces the snapshots
//! (ADR 0013).

use std::path::PathBuf;

use crate::layout::{DirectionId, DirectionView};
use crate::ports::{DeviceId, DriverState, Platform};
use crate::status::EngineStatus;

/// One line on the pipe from a guarded app (ADR 0033).
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Heartbeat {
    pub running: bool,
    pub output_periods: u64,
    pub capture_reads: u64,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "ts", derive(specta::Type))]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum Intent {
    BeginSetup,
    InstallDriver,
    /// Looks at the PC again after the user did something outside the app
    /// (a reboot, test signing, a repair, a device plugged in at the output
    /// step), or leaves the faulted state.
    Recheck,
    ChooseOutput(DeviceId),
    ImportBrir(PathBuf),
    ImportHeadphoneFir(PathBuf),
    SetGain(f32),
    TestDirection(DirectionId),
    Enable,
    Disable,
    StartDiagnosticRecording,
    Quit,
}

/// The first-run wizard's steps, in the order the checks run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "ts", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum SetupStep {
    Welcome,
    /// The PC cannot run this build: an unsupported OS build or architecture
    /// (the error says which).
    Unsupported,
    /// The driver package needs test signing and the PC does not have it on
    /// (Secure Boot state in the error).
    NotTestSigning,
    /// The driver is not installed or Windows reports a problem code for it.
    Driver,
    RebootRequired,
    ChooseOutput,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "ts", derive(specta::Type))]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum Phase {
    Setup(SetupStep),
    Ready,
    Running,
    Faulted,
    Quit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "ts", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum ProfileKind {
    Stereo,
    Headphones,
    Speakers,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "ts", derive(specta::Type))]
pub struct ProfileView {
    pub kind: ProfileKind,
    pub name: String,
    /// Imported BRIR set (folder, hesuvi.wav, or hrir.wav).
    pub brir: Option<String>,
    pub headphone_fir: Option<String>,
    pub gain_db: f32,
    pub directions: Vec<DirectionView>,
    /// The state directory's profile.toml takes precedence over import intents.
    pub from_file: bool,
}

/// An output the user may choose, or has chosen.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "ts", derive(specta::Type))]
pub struct OutputChoice {
    pub id: DeviceId,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "ts", derive(specta::Type))]
pub struct Snapshot {
    pub phase: Phase,
    pub profile: ProfileView,
    pub engine: Option<EngineStatus>,
    /// Outputs the user may choose: active, and never our own endpoints.
    pub outputs: Vec<OutputChoice>,
    pub chosen_output: Option<OutputChoice>,
    pub driver: Option<DriverState>,
    pub platform: Option<Platform>,
    /// The three default roles point at our main endpoint.
    pub roles_taken: bool,
    /// The last error, cleared by the next intent that succeeds.
    pub error: Option<UserError>,
    /// Facts the screen shows as is (a previous default device that
    /// disappeared, ...).
    pub notes: Vec<String>,
}

/// An error the screen shows as is: the sentence and the next action.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "ts", derive(specta::Type))]
#[error("{message}")]
pub struct UserError {
    pub message: String,
    pub next: NextAction,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "ts", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum NextAction {
    Retry,
    OpenSettings,
    /// Restart Windows, then recheck.
    Reboot,
    None,
}

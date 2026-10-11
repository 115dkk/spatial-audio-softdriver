//! Intent and snapshot: the whole contract between the screen and Rust (draft).
//!
//! The screen sends only intents and draws only snapshots; it keeps no state of
//! its own, so reopening the window draws the same picture. The supervisor
//! that consumes these arrives in M4; the variants below follow the
//! architecture document and may change when it does.

use std::path::PathBuf;

use crate::layout::DirectionId;
use crate::status::EngineStatus;

#[derive(Clone, Debug, PartialEq)]
pub enum Intent {
    BeginSetup,
    InstallDriver,
    ChooseOutput(String),
    ImportBrir(PathBuf),
    ImportHeadphoneFir(PathBuf),
    SwitchProfile(String),
    SetGain(f32),
    TestDirection(DirectionId),
    Enable,
    Disable,
    StartDiagnosticRecording,
    Quit,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Phase {
    Setup(u8),
    Ready,
    Running,
    Idle,
    Faulted(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Snapshot {
    pub phase: Phase,
    pub engine: Option<EngineStatus>,
}

/// An error the screen shows as is: the sentence and the next action.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct UserError {
    pub message: String,
    pub next: NextAction,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NextAction {
    Retry,
    OpenSettings,
    None,
}

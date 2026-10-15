#![forbid(unsafe_code)]

//! Spatial SoftReceiver: the Tauri 2 shell around the supervisor (ADR 0001).
//!
//! The library holds what the binary runs, so the integration tests can reach
//! the parts that build on every OS:
//!
//! * [`control`]: the control thread that owns the [`supervisor::Supervisor`],
//!   and the [`control::Bridge`] the shell sends intents through.
//! * [`settings`]: `settings.json` in the state directory.
//! * [`paths`]: the state directory, `ssr-setup.exe`, and the command line.
//! * [`log`]: `app.log` in the state directory.
//! * `launcher` (Windows): starts the engine on the driver ring and WASAPI.
//! * `shell` (Windows): the Tauri wiring: commands, events, tray, window.

pub mod control;
pub mod log;
pub mod paths;
pub mod settings;

#[cfg(windows)]
pub mod launcher;
#[cfg(windows)]
pub mod shell;

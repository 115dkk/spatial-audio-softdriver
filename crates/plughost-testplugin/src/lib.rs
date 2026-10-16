#![deny(unsafe_code)]

//! Test-only stereo to 5.1 matrix plugin. Never ship this library.
#[cfg(windows)]
mod plugin;
#[cfg(windows)]
mod vst3;

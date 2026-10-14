#![forbid(unsafe_code)]
#![cfg(not(windows))]

// Compile the pure playback-clock policy on Linux while the production output
// module remains Windows-only at the crate boundary.
#[path = "../src/output.rs"]
mod output;

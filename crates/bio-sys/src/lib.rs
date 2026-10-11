#![deny(unsafe_code)]
#![cfg_attr(not(windows), allow(dead_code))]

//! Every handwritten `unsafe` in the workspace, and everything Windows-only.
//!
//! * `ring`: the capture device (the driver's control device from M3; the SAL
//!   development ring before that, left out of release builds).
//! * `output`: WASAPI output on its own thread.
//! * `host`: device enumeration, notifications and the default endpoint (M4).
//! * `install`: driver package and device node, linked only by bio-setup (M4).
//!
//! The crate builds on every OS so `cargo test --workspace` runs on Linux; the
//! Windows modules are `cfg(windows)` and the rest is platform independent.

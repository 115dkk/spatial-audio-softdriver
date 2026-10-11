#![deny(unsafe_code)]
#![cfg_attr(not(windows), allow(dead_code))]

//! Every handwritten `unsafe` in the workspace, and everything Windows-only.
//!
//! * [`sal_format`] (every OS): the SpatialAudioLab ring's READ result format
//!   and its translation into the `bio_abi` record stream. Only the interface
//!   the SAL driver exposes is used (ADR 0005).
//! * `sal` (Windows): `SalRing`, a `CaptureSource` over SAL's
//!   `\\.\DolbyDecoderMat`. Development only; release builds leave it out.
//! * `output` (Windows): WASAPI output, exclusive and event-driven when the
//!   device allows, opened on the output thread (`OutputOpener`).
//! * `devices` (Windows): render endpoints by name and id.
//! * `qpc`, `mmcss` (Windows): QueryPerformanceCounter and MMCSS "Pro Audio"
//!   registration for real-time threads.
//!
//! The crate builds on every OS so `cargo test --workspace` runs on Linux; the
//! Windows modules are `cfg(windows)`. The driver's own control device
//! (`\\.\BioAtmos`) joins in M3; `host` and `install` in M4.

pub mod sal_format;

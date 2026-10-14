#![deny(unsafe_code)]
#![cfg_attr(not(windows), allow(dead_code))]

//! Every handwritten `unsafe` in the workspace, and everything Windows-only.
//!
//! * [`sal_format`] (every OS): the SpatialAudioLab ring's READ result format
//!   and its translation into the `ssr_abi` record stream. Only the interface
//!   the SAL driver exposes is used (ADR 0005).
//! * `driver_ring` (Windows): `DriverRing`, a `CaptureSource` over the Spatial
//!   SoftReceiver driver's control ring (`ssr_abi::CONTROL_DEVICE_PATH`).
//! * `sal` (Windows): `SalRing`, a `CaptureSource` over SAL's
//!   `\\.\DolbyDecoderMat`. Development only; release builds leave it out.
//! * `output` (Windows): WASAPI output, exclusive and event-driven when the
//!   device allows, opened on the output thread (`OutputOpener`).
//! * `devices` (Windows): render endpoints by name and id.
//! * `apartment` (Windows): keeps the multithreaded apartment, and the audio
//!   DLLs COM loaded into it, until the process exits.
//! * `qpc`, `mmcss` (Windows): QueryPerformanceCounter and MMCSS "Pro Audio"
//!   registration for real-time threads.
//!
//! The crate builds on every OS so `cargo test --workspace` runs on Linux; the
//! Windows modules are `cfg(windows)`; `host` and `install` join in M4.

pub mod driver_ring;
mod playback_clock;
pub mod sal_format;

#[cfg(windows)]
mod apartment;
#[cfg(windows)]
mod device;
#[cfg(windows)]
pub mod devices;
#[cfg(windows)]
pub mod mmcss;
#[cfg(windows)]
pub mod output;
#[cfg(windows)]
pub mod qpc;
#[cfg(windows)]
pub mod sal;

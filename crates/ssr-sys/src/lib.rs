#![deny(unsafe_code)]
#![cfg_attr(not(windows), allow(dead_code))]

//! The isolation crate (ADR 0022) for everything Windows-only the app links:
//! devices, the driver and the host.
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
//! * `host` (Windows): `WindowsHost`, the supervisor's `Host` port: platform
//!   facts, our device node, every render endpoint with our endpoint numbers,
//!   the default device roles (`IPolicyConfig`), the restore point file, and
//!   elevated `ssr-setup` runs (ADR 0032).
//! * `apartment` (Windows): keeps the multithreaded apartment, and the audio
//!   DLLs COM loaded into it, until the process exits.
//! * `qpc`, `mmcss` (Windows): QueryPerformanceCounter and MMCSS "Pro Audio"
//!   registration for real-time threads.
//!
//! The crate builds on every OS so `cargo test --workspace` runs on Linux; the
//! Windows modules are `cfg(windows)`. The administrator tasks `ssr-setup`
//! runs live in `ssr-setup-sys`, which only `ssr-setup` links (ADR 0032).

pub mod driver_ring;
mod playback_clock;
pub mod sal_format;
mod stall_guard;
#[cfg(any(windows, test))]
mod toast_xml;

#[cfg(windows)]
pub mod toast;

#[cfg(windows)]
mod apartment;
#[cfg(windows)]
mod device;
#[cfg(windows)]
pub mod devices;
#[cfg(windows)]
pub mod host;
#[cfg(windows)]
pub mod mmcss;
#[cfg(windows)]
pub mod output;
#[cfg(windows)]
pub mod qpc;
#[cfg(windows)]
pub mod sal;

#[cfg(windows)]
pub use host::WindowsHost;

#![forbid(unsafe_code)]

//! Types every other crate shares (CONTEXT.md names each of them).
//!
//! * [`scene`]: the [`SceneBlock`] every decoder writes and the renderer reads.
//! * [`layout`]: the directions of a BRIR set and the [`Layout`] they form.
//! * [`feeds`]: one signal per direction, the renderer's output.
//! * [`format`](mod@format): what a source writes to the virtual device.
//! * [`ports`]: the seams the engine is wired through ([`CaptureSource`],
//!   [`OutputOpener`]/[`OutputSink`]) and the supervisor's [`Host`].
//! * [`status`]: the engine state the control thread samples.
//! * [`app`]: the intent/snapshot pair between the screen and the supervisor.

pub mod app;
pub mod feeds;
pub mod format;
pub mod layout;
pub mod ports;
pub mod position;
pub mod scene;
pub mod status;

pub use feeds::Feeds;
pub use format::{BitstreamCodec, BitstreamFormat, PcmFormat, SampleType, SourceFormat};
pub use layout::{Direction, DirectionId, DirectionView, Layout, LayoutError};
pub use ports::{
    Arch, CaptureError, CaptureSource, DefaultRoles, DeviceId, DeviceState, DriverState,
    EngineThread, Host, HostError, OutputDevice, OutputError, OutputFormat, OutputLayout,
    OutputMode, OutputOpener, OutputSink, Platform, PolicyState, Role, SecureBoot, SetupOutcome,
    SetupTask, TrimUnsupported,
};
pub use position::{Speaker, direction_to_room, room_to_direction};

/// Named channel layout shared by direct inputs and speaker-profile masks.
pub fn channel_layout_preset(name: &str) -> Option<&'static [Speaker]> {
    use Speaker::*;
    Some(match name.to_ascii_lowercase().as_str() {
        "mono" => &[C],
        "stereo" | "2.0" => &[L, R],
        "3.0" => &[L, R, C],
        "quad" => &[L, R, Lrs, Rrs],
        "5.0" => &[L, R, C, Lrs, Rrs],
        "5.1" => &[L, R, C, Lfe, Lrs, Rrs],
        "7.1" => &[L, R, C, Lfe, Lrs, Rrs, Lss, Rss],
        "5.1.2" => &[L, R, C, Lfe, Lrs, Rrs, Tfl, Tfr],
        "5.1.4" => &[L, R, C, Lfe, Lrs, Rrs, Tfl, Tfr, Trl, Trr],
        "7.1.2" => &[L, R, C, Lfe, Lrs, Rrs, Lss, Rss, Tfl, Tfr],
        "7.1.4" => &[L, R, C, Lfe, Lrs, Rrs, Lss, Rss, Tfl, Tfr, Trl, Trr],
        "9.1.4" => &[L, R, C, Lfe, Lrs, Rrs, Lc, Rc, Lss, Rss, Tfl, Tfr, Trl, Trr],
        "9.1.6" => &[
            L, R, C, Lfe, Lrs, Rrs, Lc, Rc, Lss, Rss, Tfl, Tfr, Trl, Trr, Tsl, Tsr,
        ],
        _ => return None,
    })
}
pub use scene::{MAX_OBJECTS, Object, Position, SceneBlock};
pub use status::{
    ChainErrorCounts, ChainErrorKind, ClockStatus, ENGINE_ENDPOINTS, EndpointStatus, EngineFault,
    EngineStatus, LatencyStats, SceneObject, UnderrunSnapshot, UnsupportedReason,
};

/// The fixed scene sample rate every decoder produces.
pub const SCENE_RATE: u32 = 48_000;

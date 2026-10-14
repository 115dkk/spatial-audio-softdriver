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
//! * [`app`]: the intent/snapshot pair between the screen and the supervisor
//!   (a draft until M4).

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
pub use layout::{Direction, DirectionId, Layout, LayoutError};
pub use ports::{
    CaptureError, CaptureSource, Host, OutputError, OutputFormat, OutputLayout, OutputMode,
    OutputOpener, OutputSink, TrimUnsupported,
};
pub use position::{Speaker, direction_to_room, room_to_direction};
pub use scene::{MAX_OBJECTS, Object, Position, SceneBlock};
pub use status::{
    ClockStatus, ENGINE_ENDPOINTS, EndpointStatus, EngineFault, EngineStatus, LatencyStats,
    UnderrunSnapshot,
};

/// The scene sample rate every decoder produces unless the source says otherwise.
pub const SCENE_RATE: u32 = 48_000;

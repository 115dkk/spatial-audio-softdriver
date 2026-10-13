//! Engine status, sampled by the control thread at 30 Hz.
//!
//! The engine publishes these through atomics; `Engine::status` copies them
//! and computes latency percentiles on its caller without blocking the
//! real-time threads.

use crate::{format::SourceFormat, ports::OutputMode};

/// Status slots mirror the driver's fixed endpoint range without making this
/// foundational crate depend on `bio-abi`.
pub const ENGINE_ENDPOINTS: usize = 4;

/// Latency of the whole chain, from the driver's record QPC to the output
/// device's playback QPC (the time inside the source application is not part
/// of it).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LatencyStats {
    pub p50_ms: f32,
    pub p99_ms: f32,
    /// Blocks the statistics were computed from.
    pub samples: u32,
}

/// Why the engine stopped making sound on its own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EngineFault {
    CaptureLost(String),
    OutputLost(String),
}

/// The clock-domain strategy selected when the engine starts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ClockStatus {
    Trim,
    #[default]
    Asrc,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct EndpointStatus {
    pub format: Option<SourceFormat>,
    pub epoch: u32,
    /// Endpoint gain after combining its channel controls. Muting is carried
    /// separately so status never needs a `-inf` floating-point sentinel.
    pub volume_db: f32,
    pub muted: bool,
    /// Why this entrance's current format cannot be decoded. This is local to
    /// the entrance and never faults the other entrances.
    pub unsupported: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct EngineStatus {
    pub endpoints: [EndpointStatus; ENGINE_ENDPOINTS],
    pub latency: LatencyStats,
    /// Lowest crossing-ring level over the last control window, in
    /// milliseconds of rendered signal: the margin the clock controller holds
    /// at the configured target.
    pub ring_level_ms: f32,
    /// Times the output found the crossing ring empty.
    pub underruns: u64,
    /// Times the output device's stream was restarted after it stalled.
    pub output_restarts: u64,
    /// Layout of the currently open output sink.
    pub output_channels: u16,
    pub output_channel_mask: u32,
    /// Times the output sink was reopened for a changed layout.
    pub output_reopens: u64,
    /// Successful output reopens after an I/O or device error.
    pub output_reconnects: u64,
    /// Output-open attempts made while recovering from an I/O or device error.
    pub output_reconnect_attempts: u64,
    pub output_mode: OutputMode,
    pub output_fell_back: bool,
    /// Channels in the shared engine's mix format when fallback occurred.
    pub output_mix_channels: u16,
    /// Observed output periods per second divided by the nominal period rate.
    pub output_cadence_ratio: f32,
    pub output_cadence_faults: u64,
    /// Bytes the driver reported lost to overruns.
    pub lost_bytes: u64,
    /// Timeline frames emitted without data from an active entrance.
    pub lane_underrun_frames: u64,
    /// Source frames discarded because an entrance outran its bounded queue.
    pub dropped_frames: u64,
    /// Volume transitions coalesced after an entrance's bounded queue filled.
    pub volume_transitions_coalesced: u64,
    /// Rendered frames that could not enter the crossing ring.
    pub output_overflow_frames: u64,
    /// Idle-time blocks generated to advance tails and state.
    pub idle_blocks: u64,
    /// Real-time events that could not enter an event queue.
    pub dropped_events: u64,
    pub clock: ClockStatus,
    /// Current clock correction in ppm (trim or resampling ratio).
    pub clock_ppm: f64,
    pub fault: Option<EngineFault>,
}

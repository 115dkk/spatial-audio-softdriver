//! Engine status, sampled by the control thread at 30 Hz.
//!
//! The engine publishes these through atomics; `Engine::status` copies them
//! into this plain struct without blocking the real-time threads.

use crate::format::SourceFormat;

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
    /// The source format has no decoder adapter.
    UnsupportedFormat(String),
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct EngineStatus {
    pub format: Option<SourceFormat>,
    pub epoch: u32,
    /// Objects in the latest scene block (beds count as objects).
    pub objects: u32,
    pub latency: LatencyStats,
    /// Lowest crossing-ring level over the last control window, in
    /// milliseconds of binaural signal: the margin the clock controller holds
    /// at the configured target.
    pub ring_level_ms: f32,
    /// Times the output found the crossing ring empty.
    pub underruns: u64,
    /// Times the output device's stream was restarted after it stalled.
    pub output_restarts: u64,
    /// Bytes the driver reported lost to overruns.
    pub lost_bytes: u64,
    /// Current clock correction in ppm (trim or resampling ratio).
    pub clock_ppm: f64,
    pub fault: Option<EngineFault>,
}

#![forbid(unsafe_code)]

//! Real-time threads, the two-channel crossing ring, clock strategies and
//! latency measurement.
//!
//! * [`chain`]: the DSP chain both the replay tool and the live capture thread
//!   run: records in, binaural blocks out (M1).
//! * [`runtime`]: the two real-time threads, the crossing ring, the clock
//!   strategies (ASRC and trim, one PI controller) and latency statistics (M2).
//! * [`adapters`]: port adapters that need no Windows: dump capture, WAV and
//!   null output.

pub mod adapters;
pub mod chain;
mod limiter;
pub mod runtime;

pub use chain::{
    BlockOut, Chain, ChainConfig, ChainError, ChainEvent, PrepareError, Prepared, partition_for,
    scene_shape,
};
pub use runtime::{
    ClockPreference, Engine, EngineConfig, EngineEvent, LiveChange, RealtimeThread, Rejected,
    StartError, StopReport, ThreadHook, start, start_with_clock,
};

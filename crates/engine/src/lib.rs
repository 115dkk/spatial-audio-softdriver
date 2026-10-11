#![forbid(unsafe_code)]

//! Real-time threads, the two-channel crossing ring, clock strategies and
//! latency measurement.
//!
//! M1 provides the DSP [`chain`] both the replay tool and the live capture
//! thread run: records in, binaural blocks out. M2 adds the threads, the
//! crossing ring and the clock strategies around it. [`adapters`] holds the
//! port adapters that need no Windows: dump capture, WAV and null output.

pub mod adapters;
pub mod chain;
mod limiter;

pub use chain::{BlockOut, Chain, ChainConfig, ChainEvent, PrepareError, Prepared, partition_for};

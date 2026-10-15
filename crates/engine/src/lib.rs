#![forbid(unsafe_code)]

//! Real-time threads, the N-channel 48 kHz crossing ring, clock strategies and
//! latency measurement.
//!
//! * [`chain`]: the DSP chain both the replay tool and the live capture thread
//!   run: records in, rendered output blocks out (M1).
//! * [`runtime`]: the two real-time threads, the crossing ring, the clock
//!   strategies (ASRC and trim, one PI controller) and latency statistics (M2).
//! * [`adapters`]: port adapters that need no Windows: dump capture, WAV and
//!   null output.
//! * [`decode_thread`]: the job queue that moves allocating codec decoders
//!   (E-AC-3) off the capture thread onto `ssr-decode` (ADR 0031).
//! * [`test_tone`]: the 400 ms direction test signal (`LiveChange::TestTone`).

pub mod adapters;
pub mod chain;
pub mod decode_thread;
mod limiter;
pub mod mixer;
pub mod runtime;
pub mod test_tone;

/// The most output channels the crossing ring carries (ADR 0030).
pub const MAX_OUTPUT_CHANNELS: usize = 32;
// A speaker profile the speakers crate accepts must fit the ring.
const _: () = assert!(speakers::MAX_DEVICE_CHANNELS == MAX_OUTPUT_CHANNELS);

pub use chain::{
    BlockOut, Chain, ChainConfig, ChainError, ChainEvent, EndpointFilter, OutputKind,
    OutputProfile, OutputProfileError, PrepareError, Prepared, load_output_profile,
};
pub use decode::direct::{
    DirectLayout, DirectLayoutOverride, DirectLayouts, parse_speaker,
    preset as direct_layout_preset, speaker_name,
};
pub use decode_thread::DecodeWorker;
pub use mixer::{
    BedBlock, MAX_ARRIVAL_SLACK_FRAMES, MAX_DELAY_FRAMES, MAX_SCENE_OBJECTS, MIX_FRAMES, MIX_RATE,
    MixerConfig, SCENE_FLOOR_DBFS, SceneMixer,
};
pub use runtime::{
    ClockPreference, Engine, EngineConfig, EngineEvent, EngineThread, LiveChange, Rejected,
    RtEventHook, RtLoopEvent, RtScope, RtSection, StartError, StartOptions, StopReport, ThreadHook,
    start, start_with, start_with_clock,
};

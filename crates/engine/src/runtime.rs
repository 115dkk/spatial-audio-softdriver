//! The real-time engine (M2): the capture/DSP thread runs the [`Chain`], the
//! output thread feeds the device, and a two-channel crossing ring joins the
//! two clock domains (ADR 0012).
//!
//! ```text
//! CaptureSource ──read──▶ capture/DSP thread ──Chain──▶ crossing ring (2 ch, rtrb)
//!                                  ▲                          │
//!                         trim ppm │ (Trim strategy)          ▼
//!                                  └──────────────── output thread ──▶ OutputSink
//!                                                    (ring level → PI → ppm;
//!                                                     ASRC strategy resamples here)
//! ```
//!
//! Neither real-time thread allocates or locks after start-up. The control
//! thread talks to them through lock-free queues: [`Engine::apply`] sends a
//! [`LiveChange`] in, [`Engine::poll`] drains [`EngineEvent`]s out and drops
//! the buffers the capture thread retired (old `Prepared` sets), so freeing
//! happens on the control thread. [`Engine::status`] reads atomics only.
//!
//! [`Chain`]: crate::Chain

use std::sync::Arc;
use std::time::Duration;

use bio_types::{
    CaptureSource, EngineFault, EngineStatus, OutputError, OutputOpener, SourceFormat,
};

use crate::chain::{ChainConfig, Prepared};

/// Which real-time thread a [`ThreadHook`] runs on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RealtimeThread {
    Capture,
    Output,
}

/// Called first thing on each real-time thread, e.g. to register it with MMCSS
/// "Pro Audio" (bio-sys provides that on Windows; tests pass nothing).
pub type ThreadHook = Arc<dyn Fn(RealtimeThread) + Send + Sync>;

/// How the two clock domains are reconciled (ADR 0012).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ClockPreference {
    /// Trim when the capture source accepts it and the scene rate equals the
    /// device rate; ASRC otherwise.
    #[default]
    Auto,
    /// Always resample the binaural signal on the output thread.
    Asrc,
    /// Always trim the source; refused at start when the source cannot.
    Trim,
}

#[derive(Clone)]
pub struct EngineConfig {
    pub chain: ChainConfig,
    /// Crossing-ring level the PI controller holds, in milliseconds of
    /// binaural signal (the game preset uses 10, music 40).
    pub target_ms: f32,
    /// Crossing-ring capacity in milliseconds; must exceed `target_ms` by at
    /// least one source block.
    pub capacity_ms: f32,
    pub clock: ClockPreference,
    /// Size of the buffer one `CaptureSource::read` may fill.
    pub read_buffer_bytes: usize,
    /// How long one read may wait before the capture thread checks for a stop.
    pub read_wait: Duration,
    pub thread_hook: Option<ThreadHook>,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            chain: ChainConfig::default(),
            target_ms: 10.0,
            capacity_ms: 80.0,
            clock: ClockPreference::Auto,
            read_buffer_bytes: 512 * 1024,
            read_wait: Duration::from_millis(20),
            thread_hook: None,
        }
    }
}

impl std::fmt::Debug for EngineConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EngineConfig")
            .field("chain", &self.chain)
            .field("target_ms", &self.target_ms)
            .field("capacity_ms", &self.capacity_ms)
            .field("clock", &self.clock)
            .field("read_buffer_bytes", &self.read_buffer_bytes)
            .field("read_wait", &self.read_wait)
            .field("thread_hook", &self.thread_hook.is_some())
            .finish()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum StartError {
    #[error("the output device could not be opened: {0}")]
    Output(#[from] OutputError),
    #[error("the configuration is invalid: {0}")]
    Config(&'static str),
    #[error("the capture source cannot trim its rate, and Trim was required")]
    TrimUnsupported,
    #[error("a real-time thread could not be started: {0}")]
    Thread(String),
}

/// A change the control thread hands to the capture thread.
#[derive(Debug)]
pub enum LiveChange {
    /// A new BRIR set, prepared on the control thread; the old one comes back
    /// through [`Engine::poll`] to be dropped there.
    Prepared(Box<Prepared>),
    /// Mutes or unmutes the output (ramped, no click).
    Mute(bool),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the engine's change queue is full; try again after the next poll")]
pub struct Rejected;

/// What the real-time threads report to the control thread.
#[derive(Debug, Clone, PartialEq)]
pub enum EngineEvent {
    /// A new source format; the control thread prepares a BRIR for its scene
    /// shape (`scene_shape`) and applies it.
    FormatChanged { epoch: u32, format: SourceFormat },
    /// Blocks are being dropped until a fitting BRIR arrives.
    NeedsBrir { rate: u32, block: usize },
    /// The driver reported lost bytes.
    Overrun { lost_bytes: u64 },
    /// The engine stopped making sound; it keeps running until stopped.
    Fault(EngineFault),
}

/// Totals when the engine stops.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StopReport {
    pub blocks: u64,
    pub underruns: u64,
    pub lost_bytes: u64,
    pub final_status: EngineStatus,
}

/// A running engine. Dropping it stops the threads, like [`Engine::stop`].
#[derive(Debug)]
pub struct Engine {
    // Implementation detail: thread handles, stop flag, atomics for status,
    // rtrb producers/consumers for changes, events and retired buffers.
    _private: (),
}

/// Starts both real-time threads and returns once the output device is open
/// (or failed to open). Errors after that arrive as `EngineEvent::Fault` and
/// in `EngineStatus::fault`.
pub fn start<C, O>(config: EngineConfig, capture: C, output: O) -> Result<Engine, StartError>
where
    C: CaptureSource,
    O: OutputOpener,
{
    let _ = (config, capture, output);
    todo!("M2: engine runtime worker")
}

impl Engine {
    /// Queues a change for the capture thread; never blocks.
    pub fn apply(&self, change: LiveChange) -> Result<(), Rejected> {
        let _ = change;
        todo!("M2: engine runtime worker")
    }

    /// Drains pending events and drops retired buffers. Call from the control
    /// thread, about 30 times a second.
    pub fn poll(&mut self) -> Vec<EngineEvent> {
        todo!("M2: engine runtime worker")
    }

    /// Reads the status atomics; never blocks the real-time threads.
    pub fn status(&self) -> EngineStatus {
        todo!("M2: engine runtime worker")
    }

    /// Stops both threads, joins them, and reports totals.
    pub fn stop(self) -> StopReport {
        todo!("M2: engine runtime worker")
    }
}

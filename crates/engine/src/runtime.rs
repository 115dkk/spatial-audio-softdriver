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
//! The crossing ring supports scene rates of 8, 16, 32, 44.1, 48, 96 and
//! 192 kHz. An output period may contain at most 16,384 frames. These bounds
//! let both threads allocate their worst-case buffers before either
//! steady-state loop.
//!
//! [`Chain`]: crate::Chain

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread::JoinHandle;
use std::time::Duration;

use bio_types::{
    BitstreamCodec, BitstreamFormat, CaptureError, CaptureSource, EngineFault, EngineStatus,
    LatencyStats, OutputError, OutputOpener, OutputSink, PcmFormat, SampleType, SourceFormat,
};
use rtrb::{Consumer, Producer, PushError, RingBuffer};
use rubato::audioadapter::AdapterMut;
use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::audioadapter_buffers::owned::InterleavedOwned;
use rubato::{
    Adjustable, Async, FixedAsync, Resampler, SincInterpolationParameters, WindowFunction,
};

use crate::Chain;
use crate::chain::{ChainConfig, ChainError, ChainEvent, Prepared};

const DEFAULT_QPC_FREQUENCY: u64 = 10_000_000;
const MAX_SCENE_RATE: u32 = 192_000;
const SUPPORTED_SCENE_RATES: [u32; 7] = [8_000, 16_000, 32_000, 44_100, 48_000, 96_000, 192_000];
const MAX_OUTPUT_PERIOD_FRAMES: usize = 16_384;
const MAX_ASRC_INPUT_FRAMES: usize = MAX_OUTPUT_PERIOD_FRAMES * 24 + 512;
const EVENT_QUEUE_CAPACITY: usize = 256;
const CHANGE_QUEUE_CAPACITY: usize = 16;
const RETIRED_QUEUE_CAPACITY: usize = CHANGE_QUEUE_CAPACITY + 1;
const TIMING_QUEUE_CAPACITY: usize = 256;
const LATENCY_SAMPLES: usize = 1_024;
const ASRC_MAX_RELATIVE_RATIO: f64 = 1.002_1;
const MUTE_RAMP_MS: f64 = 5.0;
const PI_LIMIT_PPM: f64 = 2_000.0;
// Blocks land in the crossing ring whole (10 ms of PCM or MAT, 32 ms of
// E-AC-3) while the output takes one period at a time, so the level is a
// sawtooth whose trough, just before the next block lands, is how close the
// output came to running dry. The controller holds that trough at target_ms:
// it reads the lowest level left after each period over a window, and acts
// once per window. The error is in milliseconds; P turns one millisecond into
// 60 ppm and I adds 3 ppm per millisecond-second. Larger gains chase the
// sawtooth itself (2,000 and 200 swung the ratio between the ±2,000 ppm limits
// on hardware). The quantized simulation in the tests holds the trough within
// ±1.5 ms of target at ±300 ppm skew without reaching the limit; a QPC-paced
// source against a USB DAC crystal is within about ±100 ppm, and the limit
// only bounds transients.
const PI_KP: f64 = 60.0;
const PI_KI: f64 = 3.0;
const CONTROL_WINDOW_SECONDS: f64 = 0.2;
const FAULT_NONE: u32 = 0;
const FAULT_CAPTURE: u32 = 1;
const FAULT_OUTPUT: u32 = 2;
const FAULT_UNSUPPORTED: u32 = 3;

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
pub struct Engine {
    shared: Arc<Shared>,
    changes: RefCell<Producer<LiveChange>>,
    capture_events: Consumer<RtEvent>,
    output_events: Consumer<RtEvent>,
    retired: Consumer<Box<Prepared>>,
    capture_thread: Option<JoinHandle<Option<Box<Prepared>>>>,
    output_thread: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Engine")
            .field("status", &self.status())
            .finish_non_exhaustive()
    }
}

/// Starts both real-time threads and returns once the output device is open
/// (or failed to open). Errors after that arrive as `EngineEvent::Fault` and
/// in `EngineStatus::fault`.
pub fn start<C, O>(config: EngineConfig, capture: C, output: O) -> Result<Engine, StartError>
where
    C: CaptureSource,
    O: OutputOpener,
{
    start_with_clock(config, capture, output, DEFAULT_QPC_FREQUENCY)
}

/// Starts the engine with the QPC frequency used to convert timestamps to
/// seconds. Windows callers should pass the value returned by
/// `QueryPerformanceFrequency`; tests and replay adapters may pass the
/// frequency of their synthetic clock.
pub fn start_with_clock<C, O>(
    config: EngineConfig,
    capture: C,
    output: O,
    qpc_frequency: u64,
) -> Result<Engine, StartError>
where
    C: CaptureSource,
    O: OutputOpener,
{
    validate_config(&config, qpc_frequency)?;

    let shared = Arc::new(Shared::new());
    let (changes, change_consumer) = RingBuffer::new(CHANGE_QUEUE_CAPACITY);
    let (capture_event_producer, capture_events) = RingBuffer::new(EVENT_QUEUE_CAPACITY);
    let (output_event_producer, output_events) = RingBuffer::new(EVENT_QUEUE_CAPACITY);
    let (retired_producer, retired) = RingBuffer::new(RETIRED_QUEUE_CAPACITY);
    let sample_capacity = capacity_frames(config.capacity_ms, MAX_SCENE_RATE)
        .checked_mul(2)
        .ok_or(StartError::Config("crossing-ring capacity overflows usize"))?;
    let target_frames_at_max_rate = capacity_frames(config.target_ms, MAX_SCENE_RATE);
    if sample_capacity / 2 <= target_frames_at_max_rate.saturating_add(config.chain.max_block) {
        shared.stop.store(true, Ordering::Release);
        return Err(StartError::Config(
            "capacity_ms must exceed target_ms by at least max_block at 192 kHz",
        ));
    }
    let (audio_producer, audio_consumer) = RingBuffer::new(sample_capacity);
    let (timing_producer, timing_consumer) = RingBuffer::new(TIMING_QUEUE_CAPACITY);
    let (start_sender, start_receiver) = std::sync::mpsc::sync_channel(0);

    let output_shared = Arc::clone(&shared);
    let output_config = config.clone();
    let output_thread = std::thread::Builder::new()
        .name("bio-output".into())
        .spawn(move || {
            output_thread_main(
                output_config,
                output,
                audio_consumer,
                timing_consumer,
                output_shared,
                qpc_frequency,
                output_event_producer,
                start_sender,
            );
        })
        .map_err(|error| StartError::Thread(error.to_string()))?;

    let output_format = match start_receiver.recv() {
        Ok(Ok(format)) => format,
        Ok(Err(error)) => {
            shared.stop.store(true, Ordering::Release);
            let _ = output_thread.join();
            return Err(StartError::Output(error));
        }
        Err(error) => {
            shared.stop.store(true, Ordering::Release);
            let _ = output_thread.join();
            return Err(StartError::Thread(format!(
                "output thread ended during start: {error}"
            )));
        }
    };
    if output_format.channels != 2 {
        shared.stop.store(true, Ordering::Release);
        let _ = output_thread.join();
        return Err(StartError::Config("the output sink must have two channels"));
    }
    if output_format.rate == 0 || output_format.period_frames == 0 {
        shared.stop.store(true, Ordering::Release);
        let _ = output_thread.join();
        return Err(StartError::Config(
            "the output format has a zero rate or period",
        ));
    }
    if output_format.period_frames > MAX_OUTPUT_PERIOD_FRAMES {
        shared.stop.store(true, Ordering::Release);
        let _ = output_thread.join();
        return Err(StartError::Config(
            "the output period exceeds the runtime's 16,384-frame limit",
        ));
    }

    // Probe rate trim on the thread that owns the capture source. The probe and
    // the source move together into the capture thread, so no adapter call crosses
    // threads. Auto records the result now and makes the final choice when the
    // first scene rate arrives.
    let mut capture = capture;
    let trim_supported = capture.set_rate_trim(0.0).is_ok();
    if config.clock == ClockPreference::Trim && !trim_supported {
        shared.stop.store(true, Ordering::Release);
        let _ = output_thread.join();
        return Err(StartError::TrimUnsupported);
    }
    let required_trim = config.clock == ClockPreference::Trim;
    let initial_mode = match config.clock {
        ClockPreference::Asrc => ClockMode::Asrc,
        ClockPreference::Trim => ClockMode::Trim,
        ClockPreference::Auto if trim_supported => ClockMode::PendingAuto,
        ClockPreference::Auto => ClockMode::Asrc,
    };
    shared
        .clock_mode
        .store(initial_mode.as_u32(), Ordering::Release);
    shared
        .device_rate
        .store(output_format.rate, Ordering::Release);

    let capture_shared = Arc::clone(&shared);
    let capture_thread = match std::thread::Builder::new()
        .name("bio-capture-dsp".into())
        .spawn(move || {
            capture_thread_main(
                config,
                capture,
                trim_supported,
                required_trim,
                change_consumer,
                capture_event_producer,
                retired_producer,
                audio_producer,
                timing_producer,
                capture_shared,
            )
        }) {
        Ok(thread) => thread,
        Err(error) => {
            shared.stop.store(true, Ordering::Release);
            let _ = output_thread.join();
            return Err(StartError::Thread(error.to_string()));
        }
    };

    Ok(Engine {
        shared,
        changes: RefCell::new(changes),
        capture_events,
        output_events,
        retired,
        capture_thread: Some(capture_thread),
        output_thread: Some(output_thread),
    })
}

impl Engine {
    /// Queues a change for the capture thread; never blocks.
    pub fn apply(&self, change: LiveChange) -> Result<(), Rejected> {
        self.changes.borrow_mut().push(change).map_err(|_| Rejected)
    }

    /// Drains pending events and drops retired buffers. Call from the control
    /// thread, about 30 times a second.
    pub fn poll(&mut self) -> Vec<EngineEvent> {
        while self.retired.pop().is_ok() {}
        let capacity = self.capture_events.slots() + self.output_events.slots();
        let mut events = Vec::with_capacity(capacity);
        while let Ok(event) = self.capture_events.pop() {
            events.push(event.into_public(&self.shared));
        }
        while let Ok(event) = self.output_events.pop() {
            events.push(event.into_public(&self.shared));
        }
        events
    }

    /// Reads the status atomics; never blocks the real-time threads.
    pub fn status(&self) -> EngineStatus {
        self.shared.status()
    }

    /// Stops both threads, joins them, and reports totals.
    pub fn stop(mut self) -> StopReport {
        self.stop_and_join()
    }

    fn stop_and_join(&mut self) -> StopReport {
        self.shared.stop.store(true, Ordering::Release);
        if let Some(thread) = self.capture_thread.take()
            && let Ok(retired) = thread.join()
        {
            drop(retired);
        }
        if let Some(thread) = self.output_thread.take() {
            let _ = thread.join();
        }
        while self.retired.pop().is_ok() {}
        StopReport {
            blocks: self.shared.blocks.load(Ordering::Acquire),
            underruns: self.shared.underruns.load(Ordering::Acquire),
            lost_bytes: self.shared.lost_bytes.load(Ordering::Acquire),
            final_status: self.shared.status(),
        }
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        let _ = self.stop_and_join();
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ClockMode {
    PendingAuto = 0,
    Asrc = 1,
    Trim = 2,
}

impl ClockMode {
    fn as_u32(self) -> u32 {
        self as u32
    }

    fn from_u32(value: u32) -> Self {
        match value {
            1 => Self::Asrc,
            2 => Self::Trim,
            _ => Self::PendingAuto,
        }
    }
}

#[derive(Debug)]
struct Shared {
    stop: AtomicBool,
    capture_done: AtomicBool,
    capture_started: AtomicBool,
    scene_rate: AtomicU32,
    device_rate: AtomicU32,
    clock_mode: AtomicU32,
    trim_ppm_bits: AtomicU64,
    mute: AtomicBool,
    blocks: AtomicU64,
    underruns: AtomicU64,
    output_restarts: AtomicU64,
    lost_bytes: AtomicU64,
    dropped_events: AtomicU64,
    ring_level_bits: AtomicU32,
    clock_ppm_bits: AtomicU64,
    latency_p50_bits: AtomicU32,
    latency_p99_bits: AtomicU32,
    latency_samples: AtomicU32,
    format_seq: AtomicU32,
    format_tag: AtomicU32,
    format_a: AtomicU64,
    format_b: AtomicU64,
    format_c: AtomicU64,
    epoch: AtomicU32,
    fault: AtomicU32,
    /// Why the output sink failed, set once by the output thread as it stops.
    /// The error was already formatted by the sink; the thread ends after it.
    output_error: OnceLock<String>,
}

impl Shared {
    fn new() -> Self {
        Self {
            stop: AtomicBool::new(false),
            capture_done: AtomicBool::new(false),
            capture_started: AtomicBool::new(false),
            scene_rate: AtomicU32::new(0),
            device_rate: AtomicU32::new(0),
            clock_mode: AtomicU32::new(ClockMode::PendingAuto.as_u32()),
            trim_ppm_bits: AtomicU64::new(0.0_f64.to_bits()),
            mute: AtomicBool::new(false),
            blocks: AtomicU64::new(0),
            underruns: AtomicU64::new(0),
            output_restarts: AtomicU64::new(0),
            lost_bytes: AtomicU64::new(0),
            dropped_events: AtomicU64::new(0),
            ring_level_bits: AtomicU32::new(0.0_f32.to_bits()),
            clock_ppm_bits: AtomicU64::new(0.0_f64.to_bits()),
            latency_p50_bits: AtomicU32::new(0.0_f32.to_bits()),
            latency_p99_bits: AtomicU32::new(0.0_f32.to_bits()),
            latency_samples: AtomicU32::new(0),
            format_seq: AtomicU32::new(0),
            format_tag: AtomicU32::new(0),
            format_a: AtomicU64::new(0),
            format_b: AtomicU64::new(0),
            format_c: AtomicU64::new(0),
            epoch: AtomicU32::new(0),
            fault: AtomicU32::new(FAULT_NONE),
            output_error: OnceLock::new(),
        }
    }

    fn output_error(&self) -> String {
        self.output_error
            .get()
            .cloned()
            .unwrap_or_else(|| "output sink failed".into())
    }

    fn publish_format(&self, epoch: u32, format: SourceFormat) {
        self.format_seq.fetch_add(1, Ordering::AcqRel);
        let (tag, a, b, c) = encode_format(format);
        self.format_tag.store(tag, Ordering::Relaxed);
        self.format_a.store(a, Ordering::Relaxed);
        self.format_b.store(b, Ordering::Relaxed);
        self.format_c.store(c, Ordering::Relaxed);
        self.epoch.store(epoch, Ordering::Relaxed);
        self.format_seq.fetch_add(1, Ordering::Release);
    }

    fn read_format(&self) -> (Option<SourceFormat>, u32) {
        loop {
            let before = self.format_seq.load(Ordering::Acquire);
            if before & 1 != 0 {
                std::hint::spin_loop();
                continue;
            }
            let tag = self.format_tag.load(Ordering::Relaxed);
            let a = self.format_a.load(Ordering::Relaxed);
            let b = self.format_b.load(Ordering::Relaxed);
            let c = self.format_c.load(Ordering::Relaxed);
            let epoch = self.epoch.load(Ordering::Relaxed);
            let after = self.format_seq.load(Ordering::Acquire);
            if before == after {
                return (decode_format(tag, a, b, c), epoch);
            }
        }
    }

    fn status(&self) -> EngineStatus {
        let (format, epoch) = self.read_format();
        EngineStatus {
            format,
            epoch,
            objects: 0,
            latency: LatencyStats {
                p50_ms: f32::from_bits(self.latency_p50_bits.load(Ordering::Acquire)),
                p99_ms: f32::from_bits(self.latency_p99_bits.load(Ordering::Acquire)),
                samples: self.latency_samples.load(Ordering::Acquire),
            },
            ring_level_ms: f32::from_bits(self.ring_level_bits.load(Ordering::Acquire)),
            underruns: self.underruns.load(Ordering::Acquire),
            output_restarts: self.output_restarts.load(Ordering::Acquire),
            lost_bytes: self.lost_bytes.load(Ordering::Acquire),
            clock_ppm: f64::from_bits(self.clock_ppm_bits.load(Ordering::Acquire)),
            fault: match self.fault.load(Ordering::Acquire) {
                FAULT_CAPTURE => Some(EngineFault::CaptureLost("capture source failed".into())),
                FAULT_OUTPUT => Some(EngineFault::OutputLost(self.output_error())),
                FAULT_UNSUPPORTED => Some(EngineFault::UnsupportedFormat(
                    "source format is unsupported".into(),
                )),
                _ => None,
            },
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum RtEvent {
    FormatChanged { epoch: u32, format: SourceFormat },
    NeedsBrir { rate: u32, block: usize },
    Overrun { lost_bytes: u64 },
    CaptureFault,
    OutputFault,
    UnsupportedFormat,
    SceneRateUnsupported,
}

impl RtEvent {
    fn into_public(self, shared: &Shared) -> EngineEvent {
        match self {
            Self::FormatChanged { epoch, format } => EngineEvent::FormatChanged { epoch, format },
            Self::NeedsBrir { rate, block } => EngineEvent::NeedsBrir { rate, block },
            Self::Overrun { lost_bytes } => EngineEvent::Overrun { lost_bytes },
            Self::CaptureFault => {
                EngineEvent::Fault(EngineFault::CaptureLost("capture source failed".into()))
            }
            Self::OutputFault => EngineEvent::Fault(EngineFault::OutputLost(shared.output_error())),
            Self::UnsupportedFormat => EngineEvent::Fault(EngineFault::UnsupportedFormat(
                "source format is unsupported".into(),
            )),
            Self::SceneRateUnsupported => EngineEvent::Fault(EngineFault::UnsupportedFormat(
                "scene rate is not one of 8, 16, 32, 44.1, 48, 96 or 192 kHz".into(),
            )),
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct TimingMarker {
    stream_frame: u64,
    frames: u32,
    source_qpc: u64,
    rate: u32,
}

struct EventWriter {
    producer: Producer<RtEvent>,
    shared: Arc<Shared>,
}

impl EventWriter {
    fn push(&mut self, event: RtEvent) {
        if self.producer.push(event).is_err() {
            self.shared.dropped_events.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn fault(&mut self, fault: u32, event: RtEvent) {
        self.shared.fault.store(fault, Ordering::Release);
        self.push(event);
    }
}

#[allow(clippy::too_many_arguments)]
fn capture_thread_main<C: CaptureSource>(
    config: EngineConfig,
    mut capture: C,
    trim_supported: bool,
    required_trim: bool,
    mut changes: Consumer<LiveChange>,
    event_producer: Producer<RtEvent>,
    mut retired: Producer<Box<Prepared>>,
    mut audio: Producer<f32>,
    mut timing: Producer<TimingMarker>,
    shared: Arc<Shared>,
) -> Option<Box<Prepared>> {
    if let Some(hook) = &config.thread_hook {
        hook(RealtimeThread::Capture);
    }
    let mut read_buffer = vec![0_u8; config.read_buffer_bytes];
    let mut chain = Chain::new(config.chain);
    let mut events = EventWriter {
        producer: event_producer,
        shared: Arc::clone(&shared),
    };
    let mut stream_frame = 0_u64;
    let mut applied_trim = f64::NAN;
    let mut pending_retired: Option<Box<Prepared>> = None;
    let device_rate = shared.device_rate.load(Ordering::Acquire);
    shared.capture_started.store(true, Ordering::Release);
    while !shared.stop.load(Ordering::Acquire) {
        drain_changes(
            &mut changes,
            &mut chain,
            &mut retired,
            &mut pending_retired,
            &shared,
        );

        let requested_trim = f64::from_bits(shared.trim_ppm_bits.load(Ordering::Acquire));
        if trim_supported
            && (!applied_trim.is_finite() || (requested_trim - applied_trim).abs() > 0.1)
            && capture.set_rate_trim(requested_trim).is_ok()
        {
            applied_trim = requested_trim;
        }

        let bytes = match capture.read(&mut read_buffer, config.read_wait) {
            Ok(bytes) => bytes,
            Err(CaptureError::Ended) => break,
            Err(_) => {
                events.fault(FAULT_CAPTURE, RtEvent::CaptureFault);
                break;
            }
        };
        if bytes == 0 {
            continue;
        }

        let shared_ref = &shared;
        let mut handle = |event: ChainEvent<'_>| match event {
            ChainEvent::Block(block) => {
                let frames = block.left.len();
                if frames == 0 || block.right.len() != frames {
                    return;
                }
                let samples = frames.saturating_mul(2);
                if audio.slots() < samples || timing.slots() == 0 {
                    return;
                }
                for (&left, &right) in block.left.iter().zip(block.right) {
                    if audio.push(left).is_err() || audio.push(right).is_err() {
                        break;
                    }
                }
                let marker = TimingMarker {
                    stream_frame,
                    frames: frames.min(u32::MAX as usize) as u32,
                    source_qpc: block.source_qpc,
                    rate: block.rate,
                };
                let _ = timing.push(marker);
                stream_frame = stream_frame.saturating_add(frames as u64);
                shared_ref.blocks.fetch_add(1, Ordering::Relaxed);
                shared_ref.scene_rate.store(block.rate, Ordering::Release);
                settle_clock_mode(shared_ref, trim_supported, block.rate, device_rate);
            }
            ChainEvent::FormatChanged { epoch, format } => {
                shared_ref.publish_format(epoch, format);
                if let Some((rate, _)) = crate::scene_shape(&format) {
                    if !SUPPORTED_SCENE_RATES.contains(&rate) {
                        events.fault(FAULT_UNSUPPORTED, RtEvent::SceneRateUnsupported);
                    } else {
                        shared_ref.scene_rate.store(rate, Ordering::Release);
                        if required_trim && rate != device_rate {
                            events.fault(FAULT_UNSUPPORTED, RtEvent::UnsupportedFormat);
                        } else {
                            settle_clock_mode(shared_ref, trim_supported, rate, device_rate);
                        }
                    }
                }
                events.push(RtEvent::FormatChanged { epoch, format });
            }
            ChainEvent::NeedsBrir { rate, block } => {
                events.push(RtEvent::NeedsBrir { rate, block });
            }
            ChainEvent::Overrun { lost_bytes } => {
                shared_ref
                    .lost_bytes
                    .fetch_add(lost_bytes, Ordering::Relaxed);
                events.push(RtEvent::Overrun { lost_bytes });
            }
            ChainEvent::Error(error) => {
                if is_unsupported(&error) {
                    events.fault(FAULT_UNSUPPORTED, RtEvent::UnsupportedFormat);
                }
            }
        };
        chain.push_records(&read_buffer[..bytes], &mut handle);
    }

    drain_changes(
        &mut changes,
        &mut chain,
        &mut retired,
        &mut pending_retired,
        &shared,
    );
    shared.capture_done.store(true, Ordering::Release);
    pending_retired
}

fn drain_changes(
    changes: &mut Consumer<LiveChange>,
    chain: &mut Chain,
    retired: &mut Producer<Box<Prepared>>,
    pending_retired: &mut Option<Box<Prepared>>,
    shared: &Shared,
) {
    if let Some(old) = pending_retired.take() {
        match retired.push(old) {
            Ok(()) => {}
            Err(PushError::Full(old)) => *pending_retired = Some(old),
        }
    }
    while pending_retired.is_none() {
        let Ok(change) = changes.pop() else {
            break;
        };
        match change {
            LiveChange::Prepared(prepared) => {
                if let Some(old) = chain.set_prepared(prepared) {
                    match retired.push(old) {
                        Ok(()) => {}
                        Err(PushError::Full(old)) => *pending_retired = Some(old),
                    }
                }
            }
            LiveChange::Mute(mute) => shared.mute.store(mute, Ordering::Release),
        }
    }
}

fn settle_clock_mode(shared: &Shared, trim_supported: bool, scene_rate: u32, device_rate: u32) {
    if ClockMode::from_u32(shared.clock_mode.load(Ordering::Acquire)) == ClockMode::PendingAuto {
        let mode = if trim_supported && scene_rate == device_rate {
            ClockMode::Trim
        } else {
            ClockMode::Asrc
        };
        let _ = shared.clock_mode.compare_exchange(
            ClockMode::PendingAuto.as_u32(),
            mode.as_u32(),
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }
}

fn is_unsupported(error: &ChainError) -> bool {
    matches!(
        error,
        ChainError::Decode(decode::DecodeError::UnsupportedCodec(_))
            | ChainError::Decode(decode::DecodeError::Unsupported { .. })
    )
}

#[allow(clippy::too_many_arguments)]
fn output_thread_main<O: OutputOpener>(
    config: EngineConfig,
    output: O,
    audio: Consumer<f32>,
    timing: Consumer<TimingMarker>,
    shared: Arc<Shared>,
    qpc_frequency: u64,
    event_producer: Producer<RtEvent>,
    start_sender: std::sync::mpsc::SyncSender<Result<bio_types::OutputFormat, OutputError>>,
) {
    if let Some(hook) = &config.thread_hook {
        hook(RealtimeThread::Output);
    }
    let mut sink = match output.open() {
        Ok(sink) => sink,
        Err(error) => {
            let _ = start_sender.send(Err(error));
            return;
        }
    };
    let format = sink.format();
    if start_sender.send(Ok(format)).is_err() {
        return;
    }

    let mut output = OutputState::new(
        audio,
        timing,
        format.rate,
        format.period_frames,
        config.target_ms,
        qpc_frequency,
        event_producer,
        Arc::clone(&shared),
    );
    while !shared.stop.load(Ordering::Acquire) {
        let mut fill = |buffer: &mut [f32]| output.fill(buffer);
        match sink.next_period(&mut fill) {
            Ok(qpc) => {
                let restarts = sink.restarts();
                if restarts != shared.output_restarts.load(Ordering::Relaxed) {
                    shared.output_restarts.store(restarts, Ordering::Relaxed);
                    // Blocks kept landing while the device stalled; prime
                    // again so the excess goes instead of becoming latency.
                    output.priming = true;
                }
                output.finish_period(qpc);
            }
            Err(error) => {
                let _ = shared.output_error.set(error.to_string());
                shared.fault.store(FAULT_OUTPUT, Ordering::Release);
                output.events.push(RtEvent::OutputFault);
                break;
            }
        }
        if shared.capture_started.load(Ordering::Acquire)
            && shared.capture_done.load(Ordering::Acquire)
            && output.drained()
        {
            output.write_drain_tail(&mut sink);
            break;
        }
    }
}

struct OutputState {
    audio: Consumer<f32>,
    timing: Consumer<TimingMarker>,
    events: EventWriter,
    shared: Arc<Shared>,
    device_rate: u32,
    target_ms: f64,
    qpc_frequency: u64,
    controller: TroughController,
    direct: DirectClock,
    asrc: AsrcClock,
    strategy_mode: ClockMode,
    stream_frame: u64,
    pending_marker: Option<TimingMarker>,
    period_frames: usize,
    period_ring_level_frames: usize,
    /// Ring level left after this period's frames were taken.
    period_trough_frames: usize,
    /// Set at start, after an underrun or a device restart, and when the
    /// strategy or scene rate changes: output silence until the ring holds target + one block + one
    /// period, then drop what lies beyond target + one block, so the trough
    /// starts at the target instead of at zero or a block above it.
    priming: bool,
    /// Crossing-ring capacity in frames per channel.
    capacity_frames: usize,
    period_stream_start: u64,
    period_source_frames: usize,
    period_had_audio: bool,
    mute_gain: f32,
    latency: LatencyWindow,
}

impl OutputState {
    #[allow(clippy::too_many_arguments)]
    fn new(
        audio: Consumer<f32>,
        timing: Consumer<TimingMarker>,
        device_rate: u32,
        period_frames: usize,
        target_ms: f32,
        qpc_frequency: u64,
        event_producer: Producer<RtEvent>,
        shared: Arc<Shared>,
    ) -> Self {
        let asrc = AsrcClock::new(period_frames, device_rate);
        let capacity_frames = audio.buffer().capacity() / 2;
        let events = EventWriter {
            producer: event_producer,
            shared: Arc::clone(&shared),
        };
        Self {
            audio,
            timing,
            events,
            shared,
            device_rate,
            target_ms: f64::from(target_ms),
            qpc_frequency,
            controller: TroughController::new(),
            direct: DirectClock,
            asrc,
            strategy_mode: ClockMode::PendingAuto,
            stream_frame: 0,
            pending_marker: None,
            period_frames,
            period_ring_level_frames: 0,
            period_trough_frames: 0,
            priming: true,
            capacity_frames,
            period_stream_start: 0,
            period_source_frames: 0,
            period_had_audio: false,
            mute_gain: 1.0,
            latency: LatencyWindow::new(),
        }
    }

    fn fill(&mut self, buffer: &mut [f32]) {
        buffer.fill(0.0);
        self.period_ring_level_frames = self.audio.slots() / 2;
        self.period_stream_start = self.stream_frame;
        self.period_source_frames = 0;
        self.discard_stale_markers();
        self.period_had_audio = false;

        let scene_rate = self.shared.scene_rate.load(Ordering::Acquire);
        let mode = ClockMode::from_u32(self.shared.clock_mode.load(Ordering::Acquire));
        if scene_rate == 0 || mode == ClockMode::PendingAuto {
            self.apply_mute(buffer);
            return;
        }
        if mode != self.strategy_mode || self.asrc.scene_rate != scene_rate {
            self.strategy_mode = mode;
            self.controller.reset();
            self.asrc.reset(scene_rate, self.device_rate);
            self.priming = true;
        }
        // Once the source has ended, play out whatever is left.
        if self.priming && !self.shared.capture_done.load(Ordering::Acquire) {
            // The direct clock copies the period's frames from the ring as is.
            let take = match mode {
                ClockMode::Trim => buffer.len() / 2,
                ClockMode::Asrc | ClockMode::PendingAuto => self.asrc.frames_needed(),
            };
            let Some(excess) = self.priming_excess(scene_rate, take) else {
                self.apply_mute(buffer);
                return;
            };
            self.discard(excess);
            self.priming = false;
        }

        let frames = buffer.len() / 2;
        let produced = match mode {
            ClockMode::Trim => self.direct.render(
                &mut self.audio,
                &mut buffer[..frames * 2],
                &mut self.stream_frame,
            ),
            ClockMode::Asrc | ClockMode::PendingAuto => {
                self.asrc.ending = self.shared.capture_done.load(Ordering::Acquire);
                self.asrc.render(
                    &mut self.audio,
                    &mut buffer[..frames * 2],
                    scene_rate,
                    self.device_rate,
                    &mut self.stream_frame,
                )
            }
        };
        self.period_source_frames = produced.source_frames;
        self.period_had_audio = produced.output_frames != 0;
        if produced.output_frames < frames {
            self.shared.underruns.fetch_add(1, Ordering::Relaxed);
            self.priming = true;
        }
        self.period_trough_frames = self.audio.slots() / 2;
        self.apply_mute(buffer);
    }

    /// `None` until the ring holds target + the next block + this period's
    /// take (capped one block below capacity, where the capture side would
    /// start dropping blocks instead); then the frames beyond target plus the
    /// larger of a block and the take. Blocks land whole, so without that drop
    /// the trough would start anywhere up to a block (32 ms of E-AC-3) above
    /// the target and the slow controller would take a minute or more to bring
    /// it down. The take is in scene frames and, for the ASRC, includes the
    /// filter's lookahead before its first render: a period longer than a
    /// block (20 ms exclusive against 10 ms PCM) that kept only target + block
    /// came up short on every first render and primed again forever.
    fn priming_excess(&self, scene_rate: u32, take: usize) -> Option<usize> {
        let rate = f64::from(scene_rate);
        let block_frames = self.pending_marker.map_or(0.0, |marker| {
            let marker_rate = if marker.rate == 0 {
                scene_rate
            } else {
                marker.rate
            };
            f64::from(marker.frames) * rate / f64::from(marker_rate)
        });
        let target_frames = self.target_ms * rate / 1_000.0;
        let take = take as f64;
        let threshold =
            (target_frames + block_frames + take).min(self.capacity_frames as f64 - block_frames);
        let level = self.period_ring_level_frames as f64;
        if level < threshold {
            return None;
        }
        let keep = (target_frames + block_frames.max(take)).min(threshold);
        Some((level - keep).max(0.0) as usize)
    }

    /// Drops `frames` from the head of the ring, keeping the stream position
    /// and the latency markers in step.
    fn discard(&mut self, frames: usize) {
        if frames == 0 {
            return;
        }
        if let Ok(chunk) = self.audio.read_chunk(frames * 2) {
            chunk.commit_all();
        }
        self.stream_frame = self.stream_frame.saturating_add(frames as u64);
        self.period_stream_start = self.stream_frame;
        self.discard_stale_markers();
    }

    fn apply_mute(&mut self, buffer: &mut [f32]) {
        let target = if self.shared.mute.load(Ordering::Acquire) {
            0.0_f32
        } else {
            1.0_f32
        };
        let ramp_frames =
            ((f64::from(self.device_rate) * MUTE_RAMP_MS / 1_000.0).round() as usize).max(1);
        let step = 1.0 / ramp_frames as f32;
        for frame in buffer.as_chunks_mut::<2>().0 {
            if self.mute_gain < target {
                self.mute_gain = (self.mute_gain + step).min(target);
            } else if self.mute_gain > target {
                self.mute_gain = (self.mute_gain - step).max(target);
            }
            frame[0] *= self.mute_gain;
            frame[1] *= self.mute_gain;
        }
    }

    fn finish_period(&mut self, playback_qpc: u64) {
        let scene_rate = self.shared.scene_rate.load(Ordering::Acquire);
        if scene_rate == 0 {
            return;
        }
        let to_ms = |frames: usize| frames as f64 * 1_000.0 / f64::from(scene_rate);
        if !self.period_had_audio {
            // Priming or starved: show the level that is building up.
            self.shared.ring_level_bits.store(
                (to_ms(self.period_ring_level_frames) as f32).to_bits(),
                Ordering::Release,
            );
            return;
        }
        let period_seconds = self.period_frames as f64 / f64::from(self.device_rate);
        if let Some((trough_ms, ppm)) = self.controller.observe(
            to_ms(self.period_trough_frames),
            period_seconds,
            self.target_ms,
            self.strategy_mode,
        ) {
            self.shared
                .ring_level_bits
                .store((trough_ms as f32).to_bits(), Ordering::Release);
            self.shared
                .clock_ppm_bits
                .store(ppm.to_bits(), Ordering::Release);
            match self.strategy_mode {
                ClockMode::Trim => self
                    .shared
                    .trim_ppm_bits
                    .store(ppm.to_bits(), Ordering::Release),
                // The ASRC ratio is output/input. A positive controller value
                // means the ring is high, so consume faster by reducing it.
                ClockMode::Asrc => self.asrc.set_ppm(ppm, scene_rate, self.device_rate),
                ClockMode::PendingAuto => {}
            }
        }
        self.consume_markers(playback_qpc, scene_rate);
    }

    fn discard_stale_markers(&mut self) {
        if self.pending_marker.is_none() {
            self.pending_marker = self.timing.pop().ok();
        }
        while self.pending_marker.is_some_and(|marker| {
            marker.stream_frame.saturating_add(u64::from(marker.frames)) <= self.stream_frame
        }) {
            self.pending_marker = self.timing.pop().ok();
        }
    }

    fn consume_markers(&mut self, playback_qpc: u64, scene_rate: u32) {
        if self.pending_marker.is_none() {
            self.pending_marker = self.timing.pop().ok();
        }
        while let Some(marker) = self.pending_marker {
            let period_end = self
                .period_stream_start
                .saturating_add(self.period_source_frames as u64);
            if marker.stream_frame >= period_end {
                break;
            }
            if marker.stream_frame.saturating_add(u64::from(marker.frames))
                <= self.period_stream_start
            {
                self.pending_marker = self.timing.pop().ok();
                continue;
            }
            let source_offset = marker.stream_frame.saturating_sub(self.period_stream_start) as f64;
            let marker_rate = if marker.rate == 0 {
                scene_rate
            } else {
                marker.rate
            };
            let device_offset = match self.strategy_mode {
                ClockMode::Trim => source_offset,
                ClockMode::Asrc => {
                    source_offset * f64::from(self.device_rate) / f64::from(marker_rate)
                }
                ClockMode::PendingAuto => 0.0,
            };
            let frame_qpc = playback_qpc.saturating_add(
                (device_offset * self.qpc_frequency as f64 / f64::from(self.device_rate)).round()
                    as u64,
            );
            if frame_qpc >= marker.source_qpc {
                let latency_ms =
                    (frame_qpc - marker.source_qpc) as f64 * 1_000.0 / self.qpc_frequency as f64;
                self.latency.push(latency_ms as f32, &self.shared);
            }
            self.pending_marker = self.timing.pop().ok();
        }
    }

    fn drained(&self) -> bool {
        self.audio.slots() == 0 && self.asrc.buffered_frames() == 0
    }

    fn write_drain_tail<S: OutputSink>(&mut self, sink: &mut S) {
        if self.strategy_mode != ClockMode::Asrc {
            return;
        }
        let periods = self.asrc.drain_periods();
        for _ in 0..periods {
            let mut fill = |buffer: &mut [f32]| {
                buffer.fill(0.0);
                self.asrc.flush(buffer);
                self.apply_mute(buffer);
            };
            if let Err(error) = sink.next_period(&mut fill) {
                let _ = self.shared.output_error.set(error.to_string());
                self.shared.fault.store(FAULT_OUTPUT, Ordering::Release);
                self.events.push(RtEvent::OutputFault);
                break;
            }
        }
    }
}

#[derive(Clone, Copy)]
struct Rendered {
    source_frames: usize,
    output_frames: usize,
}

trait ClockStrategy {
    fn render(
        &mut self,
        audio: &mut Consumer<f32>,
        output: &mut [f32],
        scene_rate: u32,
        device_rate: u32,
        stream_frame: &mut u64,
    ) -> Rendered;
}

struct DirectClock;

impl DirectClock {
    fn render(
        &mut self,
        audio: &mut Consumer<f32>,
        output: &mut [f32],
        stream_frame: &mut u64,
    ) -> Rendered {
        <Self as ClockStrategy>::render(self, audio, output, 1, 1, stream_frame)
    }
}

impl ClockStrategy for DirectClock {
    fn render(
        &mut self,
        audio: &mut Consumer<f32>,
        output: &mut [f32],
        _scene_rate: u32,
        _device_rate: u32,
        stream_frame: &mut u64,
    ) -> Rendered {
        let available_samples = audio.slots().min(output.len());
        let samples = available_samples - available_samples % 2;
        if samples != 0 {
            let (popped, _) = audio.pop_partial_slice(&mut output[..samples]);
            debug_assert_eq!(popped.len(), samples);
        }
        let frames = samples / 2;
        *stream_frame = stream_frame.saturating_add(frames as u64);
        Rendered {
            source_frames: frames,
            output_frames: frames,
        }
    }
}

struct AsrcClock {
    resampler: Option<Async<f32>>,
    prepared: Vec<(u32, Async<f32>)>,
    input: InterleavedOwned<f32>,
    nominal_ratio: f64,
    scene_rate: u32,
    device_rate: u32,
    period_output_frames: usize,
    /// The source has ended: a last take shorter than the resampler asks for
    /// is completed with silence instead of waiting for frames that will not
    /// come.
    ending: bool,
}

impl AsrcClock {
    fn new(period_frames: usize, device_rate: u32) -> Self {
        // A 256-tap Blackman-Harris sinc with cubic interpolation is rubato's
        // documented high-quality starting point. Stereo uses the combined-sinc
        // optimization, making cubic suitable for a 48 kHz real-time stream.
        let parameters = SincInterpolationParameters::new(256, WindowFunction::BlackmanHarris2);
        let mut prepared = Vec::with_capacity(SUPPORTED_SCENE_RATES.len());
        for scene_rate in SUPPORTED_SCENE_RATES {
            let ratio = f64::from(device_rate) / f64::from(scene_rate);
            if let Ok(resampler) = Async::new_sinc(
                ratio,
                ASRC_MAX_RELATIVE_RATIO,
                &parameters,
                period_frames,
                2,
                FixedAsync::Output,
            ) {
                prepared.push((scene_rate, resampler));
            }
        }
        Self {
            resampler: None,
            prepared,
            input: InterleavedOwned::new(0.0, 2, MAX_ASRC_INPUT_FRAMES),
            nominal_ratio: 1.0,
            scene_rate: 0,
            device_rate,
            period_output_frames: period_frames,
            ending: false,
        }
    }

    fn reset(&mut self, scene_rate: u32, device_rate: u32) {
        if scene_rate == 0 || device_rate != self.device_rate {
            self.resampler = None;
            return;
        }
        if self.scene_rate == scene_rate
            && let Some(resampler) = &mut self.resampler
        {
            resampler.reset();
            return;
        }
        if let Some(active) = self.resampler.take() {
            self.prepared.push((self.scene_rate, active));
        }
        let Some(index) = self
            .prepared
            .iter()
            .position(|(prepared_rate, _)| *prepared_rate == scene_rate)
        else {
            self.scene_rate = 0;
            return;
        };
        let (_, mut resampler) = self.prepared.swap_remove(index);
        resampler.reset();
        self.nominal_ratio = f64::from(device_rate) / f64::from(scene_rate);
        self.resampler = Some(resampler);
        self.scene_rate = scene_rate;
    }

    fn set_ppm(&mut self, ppm: f64, scene_rate: u32, device_rate: u32) {
        if self.scene_rate != scene_rate || self.resampler.is_none() {
            self.reset(scene_rate, device_rate);
        }
        if let Some(resampler) = &mut self.resampler {
            let ratio = self.nominal_ratio * (1.0 - ppm * 1.0e-6);
            let _ = resampler.set_resample_ratio(ratio, true);
        }
    }

    fn render(
        &mut self,
        audio: &mut Consumer<f32>,
        output: &mut [f32],
        scene_rate: u32,
        device_rate: u32,
        stream_frame: &mut u64,
    ) -> Rendered {
        <Self as ClockStrategy>::render(self, audio, output, scene_rate, device_rate, stream_frame)
    }

    fn buffered_frames(&self) -> usize {
        0
    }

    /// Scene frames the next render takes from the ring: one period at the
    /// current ratio, plus the filter's lookahead before the first render.
    fn frames_needed(&self) -> usize {
        self.resampler
            .as_ref()
            .map_or(self.period_output_frames, |resampler| {
                resampler.input_frames_next()
            })
    }

    fn drain_periods(&self) -> usize {
        self.resampler.as_ref().map_or(0, |resampler| {
            resampler.output_delay().div_ceil(self.period_output_frames)
        })
    }

    fn flush(&mut self, output: &mut [f32]) {
        let Some(resampler) = &mut self.resampler else {
            return;
        };
        let input_frames = resampler.input_frames_next();
        if input_frames > MAX_ASRC_INPUT_FRAMES {
            return;
        }
        for frame in 0..input_frames {
            let _ = self.input.write_sample(0, frame, &0.0);
            let _ = self.input.write_sample(1, frame, &0.0);
        }
        let output_frames_capacity = output.len() / 2;
        let Ok(mut output_adapter) = InterleavedSlice::new_mut(output, 2, output_frames_capacity)
        else {
            return;
        };
        let _ = resampler.process_into_buffer(&self.input, &mut output_adapter, None);
    }
}

impl ClockStrategy for AsrcClock {
    fn render(
        &mut self,
        audio: &mut Consumer<f32>,
        output: &mut [f32],
        scene_rate: u32,
        device_rate: u32,
        stream_frame: &mut u64,
    ) -> Rendered {
        if self.scene_rate != scene_rate || self.resampler.is_none() {
            self.reset(scene_rate, device_rate);
        }
        let Some(resampler) = &mut self.resampler else {
            return Rendered {
                source_frames: 0,
                output_frames: 0,
            };
        };
        let input_frames = resampler.input_frames_next();
        let available = audio.slots() / 2;
        // Before the end the whole take must be there; at the end the frames
        // left over go out followed by silence, so the ring drains.
        let short = available < input_frames && (!self.ending || available == 0);
        if input_frames > MAX_ASRC_INPUT_FRAMES || short {
            return Rendered {
                source_frames: 0,
                output_frames: 0,
            };
        }
        let real = available.min(input_frames);
        for frame in 0..input_frames {
            let (left, right) = if frame < real {
                (
                    audio.pop().expect("availability was checked"),
                    audio.pop().expect("availability was checked"),
                )
            } else {
                (0.0, 0.0)
            };
            let _ = self.input.write_sample(0, frame, &left);
            let _ = self.input.write_sample(1, frame, &right);
        }
        let output_frames_capacity = output.len() / 2;
        let Ok(mut output_adapter) = InterleavedSlice::new_mut(output, 2, output_frames_capacity)
        else {
            return Rendered {
                source_frames: 0,
                output_frames: 0,
            };
        };
        let processed = resampler.process_into_buffer(&self.input, &mut output_adapter, None);
        let Ok((consumed, produced)) = processed else {
            return Rendered {
                source_frames: 0,
                output_frames: 0,
            };
        };
        let output_frames = output_frames_capacity.min(produced);
        let consumed = consumed.min(real);
        *stream_frame = stream_frame.saturating_add(consumed as u64);
        Rendered {
            source_frames: consumed,
            output_frames,
        }
    }
}

struct PiController {
    integral: f64,
}

impl PiController {
    fn new() -> Self {
        Self { integral: 0.0 }
    }

    fn update(&mut self, error_ms: f64, seconds: f64) -> f64 {
        self.integral =
            (self.integral + error_ms * seconds).clamp(-PI_LIMIT_PPM / PI_KI, PI_LIMIT_PPM / PI_KI);
        (PI_KP * error_ms + PI_KI * self.integral).clamp(-PI_LIMIT_PPM, PI_LIMIT_PPM)
    }
}

/// The PI controller fed with the ring's trough: the lowest level left after
/// an output period, taken over a [`CONTROL_WINDOW_SECONDS`] window.
struct TroughController {
    pi: PiController,
    trough_ms: f64,
    seconds: f64,
}

impl TroughController {
    fn new() -> Self {
        Self {
            pi: PiController::new(),
            trough_ms: f64::INFINITY,
            seconds: 0.0,
        }
    }

    fn reset(&mut self) {
        *self = Self::new();
    }

    /// Records the level left after one period; when a window completes,
    /// returns its trough and the new correction in ppm (positive: the source
    /// is fast, so ASRC consumes faster; trim slows the source with negative).
    fn observe(
        &mut self,
        level_ms: f64,
        period_seconds: f64,
        target_ms: f64,
        mode: ClockMode,
    ) -> Option<(f64, f64)> {
        self.trough_ms = self.trough_ms.min(level_ms);
        self.seconds += period_seconds;
        if self.seconds < CONTROL_WINDOW_SECONDS {
            return None;
        }
        let trough_ms = self.trough_ms;
        let error_ms = match mode {
            ClockMode::Trim => target_ms - trough_ms,
            ClockMode::Asrc => trough_ms - target_ms,
            ClockMode::PendingAuto => 0.0,
        };
        let ppm = self.pi.update(error_ms, self.seconds);
        self.trough_ms = f64::INFINITY;
        self.seconds = 0.0;
        Some((trough_ms, ppm))
    }
}

struct LatencyWindow {
    values: [f32; LATENCY_SAMPLES],
    scratch: [f32; LATENCY_SAMPLES],
    next: usize,
    len: usize,
}

impl LatencyWindow {
    fn new() -> Self {
        Self {
            values: [0.0; LATENCY_SAMPLES],
            scratch: [0.0; LATENCY_SAMPLES],
            next: 0,
            len: 0,
        }
    }

    fn push(&mut self, value: f32, shared: &Shared) {
        if !value.is_finite() {
            return;
        }
        self.values[self.next] = value;
        self.next = (self.next + 1) % LATENCY_SAMPLES;
        self.len = (self.len + 1).min(LATENCY_SAMPLES);
        self.scratch[..self.len].copy_from_slice(&self.values[..self.len]);
        self.scratch[..self.len].sort_unstable_by(f32::total_cmp);
        let p50 = self.scratch[percentile_index(self.len, 50)];
        let p99 = self.scratch[percentile_index(self.len, 99)];
        shared
            .latency_p50_bits
            .store(p50.to_bits(), Ordering::Relaxed);
        shared
            .latency_p99_bits
            .store(p99.to_bits(), Ordering::Relaxed);
        shared
            .latency_samples
            .store(self.len as u32, Ordering::Release);
    }
}

fn percentile_index(len: usize, percentile: usize) -> usize {
    ((len - 1) * percentile + 50) / 100
}

fn validate_config(config: &EngineConfig, qpc_frequency: u64) -> Result<(), StartError> {
    if !config.target_ms.is_finite() || config.target_ms < 0.0 {
        return Err(StartError::Config(
            "target_ms must be finite and non-negative",
        ));
    }
    if !config.capacity_ms.is_finite()
        || config.capacity_ms <= 0.0
        || config.capacity_ms <= config.target_ms
    {
        return Err(StartError::Config(
            "capacity_ms must be finite and greater than target_ms",
        ));
    }
    if config.read_buffer_bytes == 0 {
        return Err(StartError::Config("read_buffer_bytes must be nonzero"));
    }
    if qpc_frequency == 0 {
        return Err(StartError::Config("QPC frequency must be nonzero"));
    }
    Ok(())
}

fn capacity_frames(milliseconds: f32, rate: u32) -> usize {
    (f64::from(milliseconds) * f64::from(rate) / 1_000.0).ceil() as usize
}

fn encode_format(format: SourceFormat) -> (u32, u64, u64, u64) {
    match format {
        SourceFormat::Pcm(pcm) => {
            let sample_type = match pcm.sample_type {
                SampleType::Int => 0_u64,
                SampleType::Float => 1_u64,
            };
            let a = u64::from(pcm.channels)
                | (u64::from(pcm.sample_rate) << 16)
                | (u64::from(pcm.bits_per_sample) << 48);
            let b = u64::from(pcm.valid_bits)
                | (sample_type << 16)
                | (u64::from(pcm.channel_mask) << 32);
            (1, a, b, 0)
        }
        SourceFormat::Bitstream(stream) => {
            let (codec, other) = encode_codec(stream.codec);
            let a = u64::from(codec)
                | (u64::from(stream.container_rate) << 8)
                | (u64::from(stream.container_channels) << 40);
            let b = u64::from(stream.encoded_rate) | (u64::from(stream.encoded_channels) << 32);
            (2, a, b, u64::from(other))
        }
    }
}

fn decode_format(tag: u32, a: u64, b: u64, c: u64) -> Option<SourceFormat> {
    match tag {
        1 => Some(SourceFormat::Pcm(PcmFormat {
            channels: a as u16,
            sample_rate: (a >> 16) as u32,
            bits_per_sample: (a >> 48) as u16,
            valid_bits: b as u16,
            sample_type: if (b >> 16) & 1 == 0 {
                SampleType::Int
            } else {
                SampleType::Float
            },
            channel_mask: (b >> 32) as u32,
        })),
        2 => Some(SourceFormat::Bitstream(BitstreamFormat {
            codec: decode_codec(a as u8, c as u32),
            container_rate: (a >> 8) as u32,
            container_channels: (a >> 40) as u16,
            encoded_rate: b as u32,
            encoded_channels: (b >> 32) as u32,
        })),
        _ => None,
    }
}

fn encode_codec(codec: BitstreamCodec) -> (u8, u32) {
    match codec {
        BitstreamCodec::Ac3 => (0, 0),
        BitstreamCodec::Eac3 => (1, 0),
        BitstreamCodec::Eac3Atmos => (2, 0),
        BitstreamCodec::TrueHd => (3, 0),
        BitstreamCodec::Mat20 => (4, 0),
        BitstreamCodec::Mat21 => (5, 0),
        BitstreamCodec::Dts => (6, 0),
        BitstreamCodec::DtsHd => (7, 0),
        BitstreamCodec::Other(value) => (8, value),
    }
}

fn decode_codec(codec: u8, other: u32) -> BitstreamCodec {
    match codec {
        0 => BitstreamCodec::Ac3,
        1 => BitstreamCodec::Eac3,
        2 => BitstreamCodec::Eac3Atmos,
        3 => BitstreamCodec::TrueHd,
        4 => BitstreamCodec::Mat20,
        5 => BitstreamCodec::Mat21,
        6 => BitstreamCodec::Dts,
        7 => BitstreamCodec::DtsHd,
        _ => BitstreamCodec::Other(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct ClockRun {
        underruns: u32,
        clamped: u32,
        /// Lowest and highest window trough over the second half, in ms.
        trough: (f64, f64),
        /// Lowest and highest correction over the second half, in ppm.
        ppm: (f64, f64),
        final_ppm: f64,
    }

    /// The crossing ring at 48 kHz as the runtime sees it: blocks of `block`
    /// frames land whole at the source rate (device rate times 1 + skew, plus
    /// the trim), the output takes `period` frames per period (more or fewer
    /// by the ASRC correction), priming waits for target + block + period and
    /// drops what lies beyond target plus the larger of block and period, and
    /// the controller reads the level left after each period.
    fn simulate_clock(
        skew_ppm: f64,
        block: usize,
        period: usize,
        trim: bool,
        seconds: f64,
    ) -> ClockRun {
        let rate = 48_000.0;
        let target = 10.0;
        let to_ms = |frames: f64| frames * 1_000.0 / rate;
        let mode = if trim {
            ClockMode::Trim
        } else {
            ClockMode::Asrc
        };
        let period_seconds = period as f64 / rate;
        let periods = (seconds / period_seconds) as usize;
        let mut controller = TroughController::new();
        let (mut level, mut arriving, mut correction) = (0.0_f64, 0.0_f64, 0.0_f64);
        let mut priming = true;
        let mut run = ClockRun {
            underruns: 0,
            clamped: 0,
            trough: (f64::INFINITY, f64::NEG_INFINITY),
            ppm: (f64::INFINITY, f64::NEG_INFINITY),
            final_ppm: 0.0,
        };
        for index in 0..periods {
            let source_ppm = skew_ppm + if trim { correction } else { 0.0 };
            arriving += period as f64 * (1.0 + source_ppm * 1.0e-6);
            while arriving >= block as f64 {
                arriving -= block as f64;
                level += block as f64;
            }
            if priming {
                if to_ms(level) < target + to_ms(block as f64) + to_ms(period as f64) {
                    continue;
                }
                level = level.min(target * rate / 1_000.0 + block.max(period) as f64);
                priming = false;
            }
            let consumed_ppm = if trim { 0.0 } else { correction };
            let wanted = period as f64 * (1.0 + consumed_ppm * 1.0e-6);
            if level < wanted {
                run.underruns += 1;
                priming = true;
                continue;
            }
            level -= wanted;
            let Some((trough, ppm)) =
                controller.observe(to_ms(level), period_seconds, target, mode)
            else {
                continue;
            };
            correction = ppm;
            if ppm.abs() >= PI_LIMIT_PPM {
                run.clamped += 1;
            }
            if index * 2 >= periods {
                run.trough = (run.trough.0.min(trough), run.trough.1.max(trough));
                run.ppm = (run.ppm.0.min(ppm), run.ppm.1.max(ppm));
            }
        }
        run.final_ppm = correction;
        run
    }

    #[test]
    fn pi_controller_clamps_and_integrates() {
        let mut controller = PiController::new();
        assert_eq!(controller.update(100.0, 0.01), PI_LIMIT_PPM);
        let mut controller = PiController::new();
        let first = controller.update(0.5, 0.01);
        let later = (0..100).fold(first, |_, _| controller.update(0.5, 0.01));
        assert!(later > first);
    }

    #[test]
    fn clock_holds_the_trough_without_chasing_the_block_sawtooth() {
        // (block, period): PCM or MAT at a 3 ms exclusive period, E-AC-3 at
        // 3 ms, E-AC-3 at a 10 ms shared period, PCM and E-AC-3 at a 20 ms
        // exclusive period.
        for (block, period) in [
            (480, 144),
            (1_536, 144),
            (1_536, 480),
            (480, 960),
            (1_536, 960),
        ] {
            for trim in [false, true] {
                let run = simulate_clock(0.0, block, period, trim, 120.0);
                let label = format!("block {block} period {period} trim {trim}");
                assert_eq!(run.underruns, 0, "{label}");
                assert_eq!(run.clamped, 0, "{label}");
                // The trough read at 10 ms periods steps by up to 2 ms as the
                // 32 ms blocks fall into different periods, hence 150 ppm.
                assert!(
                    run.ppm.0 >= -150.0 && run.ppm.1 <= 150.0,
                    "{label}: correction {:?} ppm with no skew",
                    run.ppm
                );
            }
        }
    }

    #[test]
    fn clock_follows_three_hundred_ppm_of_skew_for_asrc_and_trim() {
        for (block, period) in [(480, 144), (1_536, 144), (1_536, 480)] {
            for trim in [false, true] {
                for skew in [-300.0, 300.0] {
                    let run = simulate_clock(skew, block, period, trim, 300.0);
                    let label = format!("block {block} period {period} trim {trim} skew {skew}");
                    assert_eq!(run.underruns, 0, "{label}");
                    assert_eq!(run.clamped, 0, "{label}");
                    assert!(
                        run.trough.0 >= 8.0 && run.trough.1 <= 12.0,
                        "{label}: trough {:?} ms",
                        run.trough
                    );
                    let expected = if trim { -skew } else { skew };
                    assert!(
                        (run.final_ppm - expected).abs() <= 100.0,
                        "{label}: correction {} ppm",
                        run.final_ppm
                    );
                }
            }
        }
    }

    #[test]
    fn clock_with_a_long_period_stays_bounded() {
        // A 10 ms shared period with 10 ms PCM blocks, and a 20 ms exclusive
        // period with PCM or E-AC-3: as the arrival phase drifts past the
        // period boundary the real trough moves by up to a block, so the
        // controller cannot hold it within the 2 ms above. It must still not
        // swing to its limit or starve the output more than now and then.
        for (block, period) in [(480, 480), (480, 960), (1_536, 960)] {
            for skew in [-300.0, -100.0, 100.0, 300.0] {
                let run = simulate_clock(skew, block, period, false, 300.0);
                let label = format!("block {block} period {period} skew {skew}");
                assert_eq!(run.clamped, 0, "{label}");
                assert!(run.underruns <= 3, "{label}: {} underruns", run.underruns);
            }
        }
    }

    /// Drives the real output state (ring, priming, ASRC) with whole blocks
    /// landing in step with the periods, as a paced source does, and returns
    /// the underruns and the periods that carried audio.
    fn play_output(block: usize, period: usize, periods: usize) -> (u64, usize) {
        let rate = 48_000;
        let shared = Arc::new(Shared::new());
        shared.scene_rate.store(rate, Ordering::Release);
        shared
            .clock_mode
            .store(ClockMode::Asrc.as_u32(), Ordering::Release);
        shared.capture_started.store(true, Ordering::Release);
        let (mut audio_in, audio) = RingBuffer::<f32>::new(80 * 48 * 2);
        let (mut timing_in, timing) = RingBuffer::<TimingMarker>::new(TIMING_QUEUE_CAPACITY);
        let (events, _events_out) = RingBuffer::<RtEvent>::new(EVENT_QUEUE_CAPACITY);
        let mut output = OutputState::new(
            audio,
            timing,
            rate,
            period,
            10.0,
            DEFAULT_QPC_FREQUENCY,
            events,
            Arc::clone(&shared),
        );
        let mut buffer = vec![0.0; period * 2];
        let (mut arriving, mut stream_frame, mut played) = (0, 0_u64, 0);
        for index in 0..periods {
            arriving += period;
            while arriving >= block {
                arriving -= block;
                for _ in 0..block * 2 {
                    audio_in.push(0.25).expect("the ring has room");
                }
                let _ = timing_in.push(TimingMarker {
                    stream_frame,
                    frames: block as u32,
                    source_qpc: 0,
                    rate,
                });
                stream_frame += block as u64;
            }
            output.fill(&mut buffer);
            played += usize::from(output.period_had_audio);
            output.finish_period(index as u64 * 200_000);
        }
        (shared.underruns.load(Ordering::Acquire), played)
    }

    #[test]
    fn the_end_of_a_source_drains_without_underruns() {
        // DD+ blocks against a 10 ms shared period: when the source ends, the
        // frames left in the ring are fewer than the ASRC's next take. They
        // must still go out, the ring must drain, and no period after the end
        // may count as an underrun.
        let rate = 48_000;
        let (block, period) = (1_536, 480);
        let shared = Arc::new(Shared::new());
        shared.scene_rate.store(rate, Ordering::Release);
        shared
            .clock_mode
            .store(ClockMode::Asrc.as_u32(), Ordering::Release);
        shared.capture_started.store(true, Ordering::Release);
        let (mut audio_in, audio) = RingBuffer::<f32>::new(80 * 48 * 2);
        let (mut timing_in, timing) = RingBuffer::<TimingMarker>::new(TIMING_QUEUE_CAPACITY);
        let (events, _events_out) = RingBuffer::<RtEvent>::new(EVENT_QUEUE_CAPACITY);
        let mut output = OutputState::new(
            audio,
            timing,
            rate,
            period,
            10.0,
            DEFAULT_QPC_FREQUENCY,
            events,
            Arc::clone(&shared),
        );
        let mut buffer = vec![0.0; period * 2];
        let (mut arriving, mut stream_frame) = (0, 0_u64);
        for index in 0..103 {
            arriving += period;
            while arriving >= block {
                arriving -= block;
                for _ in 0..block * 2 {
                    audio_in.push(0.25).expect("the ring has room");
                }
                let _ = timing_in.push(TimingMarker {
                    stream_frame,
                    frames: block as u32,
                    source_qpc: 0,
                    rate,
                });
                stream_frame += block as u64;
            }
            output.fill(&mut buffer);
            output.finish_period(index as u64 * 100_000);
        }
        assert_eq!(shared.underruns.load(Ordering::Acquire), 0);
        assert!(
            output.audio.slots() > 0,
            "the ring holds the last frames when the source ends"
        );
        shared.capture_done.store(true, Ordering::Release);
        let mut periods = 0;
        while !output.drained() {
            periods += 1;
            assert!(periods <= 20, "the ring did not drain");
            output.fill(&mut buffer);
            output.finish_period(0);
        }
        assert_eq!(shared.underruns.load(Ordering::Acquire), 0);
    }

    #[test]
    fn a_period_longer_than_a_block_plays_after_priming() {
        // 20 and 40 ms exclusive periods against 10 ms PCM blocks, and 20 ms
        // against 32 ms E-AC-3 blocks: priming must leave target plus the
        // ASRC's whole first take (the period and the filter's lookahead), or
        // the first render comes up short and primes again forever.
        for (block, period) in [(480, 960), (480, 1_920), (1_536, 960), (480, 144)] {
            let (underruns, played) = play_output(block, period, 400);
            assert_eq!(underruns, 0, "block {block} period {period}");
            assert!(
                played >= 390,
                "block {block} period {period}: {played} of 400 periods carried audio"
            );
        }
    }

    #[test]
    fn latency_window_publishes_percentiles_without_heap_storage() {
        let shared = Shared::new();
        let mut window = LatencyWindow::new();
        for value in 1..=100 {
            window.push(value as f32, &shared);
        }
        let status = shared.status();
        assert_eq!(status.latency.samples, 100);
        assert_eq!(status.latency.p50_ms, 51.0);
        assert_eq!(status.latency.p99_ms, 99.0);
    }

    #[test]
    fn format_seqlock_round_trips_formats() {
        let shared = Shared::new();
        let formats = [
            SourceFormat::Pcm(PcmFormat {
                channels: 2,
                sample_rate: 48_000,
                bits_per_sample: 32,
                valid_bits: 24,
                sample_type: SampleType::Float,
                channel_mask: 3,
            }),
            SourceFormat::Bitstream(BitstreamFormat {
                codec: BitstreamCodec::Eac3Atmos,
                container_rate: 192_000,
                container_channels: 2,
                encoded_rate: 48_000,
                encoded_channels: u32::MAX,
            }),
            SourceFormat::Bitstream(BitstreamFormat {
                codec: BitstreamCodec::Other(0xdead_beef),
                container_rate: 192_000,
                container_channels: 8,
                encoded_rate: 96_000,
                encoded_channels: 0xfeed_beef,
            }),
        ];
        for (epoch, format) in formats.into_iter().enumerate() {
            shared.publish_format(epoch as u32, format);
            assert_eq!(shared.read_format(), (Some(format), epoch as u32));
        }
    }
}

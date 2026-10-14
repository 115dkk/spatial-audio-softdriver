//! The real-time engine (M2): the capture/DSP thread runs the [`Chain`], the
//! output thread feeds the device, and an N-channel 48 kHz crossing ring joins
//! the two clock domains (ADR 0012).
//!
//! ```text
//! CaptureSource ──read──▶ capture/DSP thread ──Chain──▶ crossing ring (N ch, 48 kHz, rtrb)
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
//! happens on the control thread. [`Engine::status`] copies atomics and sorts a
//! latency snapshot without blocking either real-time thread.
//!
//! The crossing ring carries the fixed 48 kHz scene timeline. An output period
//! may contain at most 16,384 frames and device rates below 8 kHz are rejected.
//! These bounds let both threads allocate their worst-case buffers before either
//! steady-state loop.
//!
//! [`Chain`]: crate::Chain

use std::cell::RefCell;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use rtrb::{Consumer, Producer, PushError, RingBuffer};
use rubato::audioadapter::AdapterMut;
use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::audioadapter_buffers::owned::InterleavedOwned;
use rubato::{
    Adjustable, Async, FixedAsync, Resampler, SincInterpolationParameters, WindowFunction,
};
use ssr_types::{
    BitstreamCodec, BitstreamFormat, CaptureError, CaptureSource, ChainErrorCounts, ChainErrorKind,
    ClockStatus, ENGINE_ENDPOINTS, EndpointStatus, EngineFault, EngineStatus, LatencyStats,
    OutputError, OutputFormat, OutputLayout, OutputMode, OutputOpener, OutputSink, PcmFormat,
    SampleType, SourceFormat, UnderrunSnapshot,
};

use crate::Chain;
use crate::chain::{ChainConfig, ChainError, ChainEvent, Prepared};
use crate::{MAX_OUTPUT_CHANNELS, MIX_FRAMES};

const DEFAULT_QPC_FREQUENCY: u64 = 10_000_000;
const _: [(); ssr_abi::MAX_ENDPOINTS] = [(); ENGINE_ENDPOINTS];
const _: [(); ENGINE_ENDPOINTS] = [(); ssr_abi::MAX_ENDPOINTS];
const MIN_DEVICE_RATE: u32 = 8_000;
const MAX_OUTPUT_PERIOD_FRAMES: usize = 16_384;
// A 16,384-frame period at the lowest accepted 8 kHz device rate needs at
// most six times as many 48 kHz input frames, plus rubato's 512-frame lookahead.
const MAX_ASRC_INPUT_FRAMES: usize =
    MAX_OUTPUT_PERIOD_FRAMES * crate::MIX_RATE.div_ceil(MIN_DEVICE_RATE) as usize + 512;
const EVENT_QUEUE_CAPACITY: usize = 256;
const CHANGE_QUEUE_CAPACITY: usize = 16;
const RETIRED_QUEUE_CAPACITY: usize = CHANGE_QUEUE_CAPACITY + 1;
const TIMING_QUEUE_CAPACITY: usize = 256;
const LATENCY_SAMPLES: usize = 1_024;
const ASRC_MAX_RELATIVE_RATIO: f64 = 1.002_1;
const MUTE_RAMP_MS: f64 = 5.0;
const BOUNDARY_FADE_MS: f64 = 2.0;
const RECONNECT_INITIAL_MS: u64 = 250;
const RECONNECT_MAX_MS: u64 = 5_000;
const CADENCE_WINDOW_SECONDS: f64 = 2.0;
const CADENCE_FAULT_SECONDS: f64 = 3.0;
const PI_LIMIT_PPM: f64 = 2_000.0;
// Deliveries land in the crossing ring whole (10 ms of PCM or MAT, 32 ms of
// E-AC-3 as several mixer blocks) while the output takes one period at a
// time, so the level is a sawtooth whose trough, just before the next
// delivery lands, is how close the output came to running dry. The controller
// holds that trough at target_ms plus what one late delivery costs a take
// (trough_setpoint_frames): it reads the lowest level left after each period
// over a window, and acts
// once per window. The error is in milliseconds; P turns one millisecond into
// 60 ppm and I adds 3 ppm per millisecond-second. Larger gains chase the
// sawtooth itself (2,000 and 200 swung the ratio between the ±2,000 ppm limits
// on hardware). The quantized simulation in the tests holds the trough within
// ±1.5 ms of target at ±300 ppm skew without reaching the limit; a QPC-paced
// source against a USB DAC crystal is within about ±100 ppm, and the limit
// only bounds transients.
const PI_KP: f64 = 60.0;
const PI_KI: f64 = 3.0;
// The integral learns only within this many milliseconds of the setpoint; P
// alone drains larger errors (see PiController::update). With a period at
// least as long as a delivery the trough is flat between phase slips and
// steps by a whole delivery at each, so the skew shows only as those steps:
// the band holds one 10 ms block (PCM, MAT and the mixer's blocks) and 2 ms
// of jitter. At 6 ms the integral never learned a slip, P alone took about
// 20 s to climb back each time, and a late block in that time emptied the
// ring. At 16 ms it learned the E-AC-3 sawtooth at a 3 ms period as skew.
const PI_INTEGRAL_BAND_MS: f64 = 12.0;
const CONTROL_WINDOW_SECONDS: f64 = 0.2;
// An overflow burst reports at most one event per this many nanoseconds.
const OVERFLOW_REPORT_NS: u64 = 1_000_000_000;
// Priming waits for the largest delivery among this many recent capture steps
// that delivered audio (see ArrivalPeak). A DD+ sync frame is 3.2 mixer blocks,
// so deliveries run three blocks four times and then four; eight steps always
// hold a four-block one, and a one-off backlog is forgotten soon after.
const ARRIVAL_WINDOW: usize = 8;
const FAULT_NONE: u32 = 0;
const FAULT_CAPTURE: u32 = 1;
const FAULT_OUTPUT: u32 = 2;
const UNSUPPORTED_NONE: u32 = 0;
const UNSUPPORTED_FORMAT: u32 = 1;
const UNSUPPORTED_SCENE_RATE: u32 = 2;
const UNSUPPORTED_DIRECT_LAYOUT: u32 = 3;

/// Which real-time thread a [`ThreadHook`] runs on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RealtimeThread {
    Capture,
    Output,
}

/// Called first thing on each real-time thread, e.g. to register it with MMCSS
/// "Pro Audio" (ssr-sys provides that on Windows; tests pass nothing).
pub type ThreadHook = Arc<dyn Fn(RealtimeThread) + Send + Sync>;

/// One complete allocation-free real-time loop iteration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RtSection {
    Capture,
    Output,
}

/// Runs one real-time section. Tests may wrap `work`, for example in
/// `assert_no_alloc`; production callers normally leave this unset.
pub type RtScope = Arc<dyn Fn(RtSection, &mut dyn FnMut()) + Send + Sync>;

/// Notifications emitted outside the real-time sections.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RtLoopEvent {
    CaptureEnd,
    OutputEnd,
    OutputOpenBegin,
    OutputOpenEnd,
    CaptureFinished,
    /// The output is about to write what the resampler holds after the last
    /// rendered period, then stop.
    OutputTailBegin,
    /// The output thread left because the stream ended, not because the
    /// engine was stopped.
    OutputFinished,
}

pub type RtEventHook = Arc<dyn Fn(RtLoopEvent) + Send + Sync>;

/// How the two clock domains are reconciled (ADR 0012).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ClockPreference {
    /// Trim when the capture source accepts it and the device runs at 48 kHz;
    /// ASRC otherwise.
    #[default]
    Auto,
    /// Always resample the rendered output on the output thread.
    Asrc,
    /// Always trim the source; refused at start when the source cannot.
    Trim,
}

#[derive(Clone)]
pub struct EngineConfig {
    pub chain: ChainConfig,
    /// Crossing-ring level the PI controller holds, in milliseconds of
    /// rendered output (the game preset uses 10, music 40).
    pub target_ms: f32,
    /// Crossing-ring capacity in milliseconds; must exceed `target_ms` by at
    /// least `chain.max_block` frames at 48 kHz, the largest delivery a decoder
    /// hands over at once (one E-AC-3 sync frame by default). The controller
    /// holds the trough at `target_ms` plus one output period only while the
    /// ring also has room for one delivery above that, so the capacity has to
    /// cover target, period and delivery with room to spare; the default (320
    /// ms, 15,360 frames) is the ring the runtime always had.
    pub capacity_ms: f32,
    pub clock: ClockPreference,
    /// Size of the buffer one `CaptureSource::read` may fill.
    pub read_buffer_bytes: usize,
    /// How long one read may wait before the capture thread checks for a stop.
    pub read_wait: Duration,
    pub thread_hook: Option<ThreadHook>,
    pub rt_scope: Option<RtScope>,
    pub rt_event: Option<RtEventHook>,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            chain: ChainConfig::default(),
            target_ms: 10.0,
            capacity_ms: 320.0,
            clock: ClockPreference::Auto,
            read_buffer_bytes: 512 * 1024,
            read_wait: Duration::from_millis(20),
            thread_hook: None,
            rt_scope: None,
            rt_event: None,
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
            .field("rt_scope", &self.rt_scope.is_some())
            .field("rt_event", &self.rt_event.is_some())
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
    /// Changes the output-stage LFE route in dB, ramped over one mix block.
    LfeGain(f32),
    /// Changes the crossing-ring trough target from the next output period.
    TargetMs(f32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the engine's change queue is full; try again after the next poll")]
pub struct Rejected;

/// What the real-time threads report to the control thread.
#[derive(Debug, Clone, PartialEq)]
pub enum EngineEvent {
    /// A new source format observed on an input endpoint.
    FormatChanged {
        endpoint: u16,
        epoch: u32,
        format: SourceFormat,
    },
    /// Blocks are being dropped until an output is prepared.
    NeedsPrepared,
    /// The driver reported lost bytes.
    Overrun { lost_bytes: u64 },
    /// A rendered non-silent block could not enter the crossing ring.
    OutputOverflow { frames: u64 },
    /// The output sink reopened for a new profile layout.
    OutputLayoutChanged { channels: u16, channel_mask: u32 },
    /// The output sink was reconnected after a device or I/O failure.
    OutputReconnected,
    /// An active global fault was cleared after recovery.
    FaultCleared,
    /// The device kept an out-of-range cadence for three seconds.
    OutputCadenceFault { ratio: f32 },
    /// The cadence guard requested one larger exclusive period.
    OutputCadenceReopen { period_ms: f32 },
    /// The engine stopped making sound; it keeps running until stopped.
    Fault(EngineFault),
}

/// Totals when the engine stops.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StopReport {
    pub blocks: u64,
    pub underruns: u64,
    pub lost_bytes: u64,
    pub lane_underrun_frames: u64,
    pub dropped_frames: u64,
    pub output_overflow_frames: u64,
    pub idle_blocks: u64,
    pub dropped_events: u64,
    pub output_channels: u16,
    pub output_channel_mask: u32,
    pub output_reopens: u64,
    pub final_status: EngineStatus,
}

#[derive(Debug)]
pub struct StartOptions {
    pub qpc_frequency: u64,
    pub prepared: Option<Box<Prepared>>,
}

impl Default for StartOptions {
    fn default() -> Self {
        Self {
            qpc_frequency: DEFAULT_QPC_FREQUENCY,
            prepared: None,
        }
    }
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
    start_with(
        config,
        capture,
        output,
        StartOptions {
            qpc_frequency: DEFAULT_QPC_FREQUENCY,
            prepared: None,
        },
    )
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
    start_with(
        config,
        capture,
        output,
        StartOptions {
            qpc_frequency,
            prepared: None,
        },
    )
}

/// Starts the engine with optional DSP state installed before capture begins.
pub fn start_with<C, O>(
    config: EngineConfig,
    capture: C,
    output: O,
    options: StartOptions,
) -> Result<Engine, StartError>
where
    C: CaptureSource,
    O: OutputOpener,
{
    let StartOptions {
        qpc_frequency,
        prepared,
    } = options;
    validate_config(&config, qpc_frequency)?;

    let shared = Arc::new(Shared::new());
    shared
        .target_ms_bits
        .store(config.target_ms.to_bits(), Ordering::Release);
    shared.max_target_ms_bits.store(
        (config.capacity_ms - config.chain.max_block as f32 * 1_000.0 / crate::MIX_RATE as f32)
            .to_bits(),
        Ordering::Release,
    );
    let (changes, change_consumer) = RingBuffer::new(CHANGE_QUEUE_CAPACITY);
    let (capture_event_producer, capture_events) = RingBuffer::new(EVENT_QUEUE_CAPACITY);
    let (output_event_producer, output_events) = RingBuffer::new(EVENT_QUEUE_CAPACITY);
    let (retired_producer, retired) = RingBuffer::new(RETIRED_QUEUE_CAPACITY);
    let ring_capacity_frames = capacity_frames(config.capacity_ms, crate::MIX_RATE);
    let sample_capacity = ring_capacity_frames
        .checked_mul(MAX_OUTPUT_CHANNELS)
        .ok_or(StartError::Config("crossing-ring capacity overflows usize"))?;
    // One whole decoder delivery must fit above the level the controller
    // holds, or every burst drops blocks.
    let target_frames = capacity_frames(config.target_ms, crate::MIX_RATE);
    if ring_capacity_frames <= target_frames.saturating_add(config.chain.max_block) {
        shared.stop.store(true, Ordering::Release);
        return Err(StartError::Config(
            "capacity_ms must exceed target_ms by at least max_block frames at 48 kHz",
        ));
    }
    let (audio_producer, audio_consumer) = RingBuffer::new(sample_capacity);
    let (timing_producer, timing_consumer) = RingBuffer::new(TIMING_QUEUE_CAPACITY);
    let (start_sender, start_receiver) = std::sync::mpsc::sync_channel(0);
    let (clock_sender, clock_receiver) = std::sync::mpsc::sync_channel(0);
    let initial_layout = prepared
        .as_ref()
        .map_or(OutputLayout::STEREO, |prepared| prepared.layout());

    let output_shared = Arc::clone(&shared);
    let output_config = config.clone();
    let output_thread = std::thread::Builder::new()
        .name("ssr-output".into())
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
                clock_receiver,
                initial_layout,
                ring_capacity_frames,
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
    // Probe rate trim on the thread that owns the capture source. The probe and
    // the source move together into the capture thread, so no adapter call crosses
    // threads. Reopens use the stored capability rather than the current mode.
    let mut capture = capture;
    let trim_supported = capture.set_rate_trim(0.0).is_ok();
    if config.clock == ClockPreference::Trim && !trim_supported {
        shared.stop.store(true, Ordering::Release);
        drop(clock_sender);
        let _ = output_thread.join();
        return Err(StartError::TrimUnsupported);
    }
    shared
        .trim_available
        .store(trim_supported, Ordering::Release);
    let initial_mode =
        match select_output_clock(config.clock, trim_supported, output_format, initial_layout) {
            Ok(mode) => mode,
            Err(error) => {
                shared.stop.store(true, Ordering::Release);
                drop(clock_sender);
                let _ = output_thread.join();
                return Err(StartError::Config(error));
            }
        };
    shared
        .clock_mode
        .store(initial_mode.as_u32(), Ordering::Release);
    if clock_sender.send(()).is_err() {
        shared.stop.store(true, Ordering::Release);
        let _ = output_thread.join();
        return Err(StartError::Thread(
            "output thread ended during clock selection".to_owned(),
        ));
    }
    shared.publish_output_format(output_format);

    // Chain owns the large FIR tables used by every entrance. Build and seed it
    // on the control thread, before the capture thread enters its real-time loop.
    let mut chain = Chain::with_qpc_frequency(config.chain, qpc_frequency);
    if let Some(prepared) = prepared {
        drop(chain.set_prepared(prepared));
    }
    let capture_shared = Arc::clone(&shared);
    let capture_thread = match std::thread::Builder::new()
        .name("ssr-capture-dsp".into())
        .spawn(move || {
            capture_thread_main(
                config,
                capture,
                trim_supported,
                change_consumer,
                capture_event_producer,
                retired_producer,
                audio_producer,
                timing_producer,
                capture_shared,
                chain,
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
        if matches!(change, LiveChange::LfeGain(db) if !db.is_finite() || !(-40.0..=20.0).contains(&db))
            || matches!(change, LiveChange::TargetMs(ms) if !valid_target_ms(ms, &self.shared))
        {
            return Err(Rejected);
        }
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

    /// Copies status atomics and computes latency percentiles on this caller;
    /// never blocks either real-time thread.
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
            lane_underrun_frames: self.shared.lane_underrun_frames.load(Ordering::Acquire),
            dropped_frames: self.shared.dropped_frames.load(Ordering::Acquire),
            output_overflow_frames: self.shared.output_overflow_frames.load(Ordering::Acquire),
            idle_blocks: self.shared.idle_blocks.load(Ordering::Acquire),
            dropped_events: self.shared.dropped_events.load(Ordering::Acquire),
            output_channels: self.shared.output_channels.load(Ordering::Acquire) as u16,
            output_channel_mask: self.shared.output_channel_mask.load(Ordering::Acquire),
            output_reopens: self.shared.output_reopens.load(Ordering::Acquire),
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
    Asrc = 1,
    Trim = 2,
}

impl ClockMode {
    fn as_u32(self) -> u32 {
        self as u32
    }

    fn from_u32(value: u32) -> Self {
        match value {
            2 => Self::Trim,
            _ => Self::Asrc,
        }
    }
}

#[derive(Debug)]
struct EndpointShared {
    format_seq: AtomicU32,
    format_tag: AtomicU32,
    format_a: AtomicU64,
    format_b: AtomicU64,
    format_c: AtomicU64,
    epoch: AtomicU32,
    unsupported: AtomicU32,
    volume_bits: AtomicU32,
    muted: AtomicBool,
    chain_errors: [AtomicU64; ChainErrorKind::COUNT],
}

impl EndpointShared {
    fn new() -> Self {
        Self {
            format_seq: AtomicU32::new(0),
            format_tag: AtomicU32::new(0),
            format_a: AtomicU64::new(0),
            format_b: AtomicU64::new(0),
            format_c: AtomicU64::new(0),
            epoch: AtomicU32::new(0),
            unsupported: AtomicU32::new(UNSUPPORTED_NONE),
            volume_bits: AtomicU32::new(0.0_f32.to_bits()),
            muted: AtomicBool::new(false),
            chain_errors: std::array::from_fn(|_| AtomicU64::new(0)),
        }
    }

    fn publish(&self, epoch: u32, format: SourceFormat, unsupported: u32) {
        self.format_seq.fetch_add(1, Ordering::AcqRel);
        let (tag, a, b, c) = encode_format(format);
        self.format_tag.store(tag, Ordering::Relaxed);
        self.format_a.store(a, Ordering::Relaxed);
        self.format_b.store(b, Ordering::Relaxed);
        self.format_c.store(c, Ordering::Relaxed);
        self.epoch.store(epoch, Ordering::Relaxed);
        self.unsupported.store(unsupported, Ordering::Relaxed);
        self.format_seq.fetch_add(1, Ordering::Release);
    }

    fn publish_volume(&self, gain: f32, muted: bool) {
        self.format_seq.fetch_add(1, Ordering::AcqRel);
        let db = if muted || gain <= 0.0 {
            0.0
        } else {
            20.0 * gain.log10()
        };
        self.volume_bits.store(db.to_bits(), Ordering::Relaxed);
        self.muted.store(muted, Ordering::Relaxed);
        self.format_seq.fetch_add(1, Ordering::Release);
    }

    fn note_chain_error(&self, kind: ChainErrorKind) {
        self.chain_errors[kind.index()].fetch_add(1, Ordering::Relaxed);
    }

    fn read_chain_errors(&self) -> ChainErrorCounts {
        ChainErrorCounts::from_fn(|kind| self.chain_errors[kind.index()].load(Ordering::Relaxed))
    }

    fn read(&self) -> (Option<SourceFormat>, u32, u32, f32, bool) {
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
            let unsupported = self.unsupported.load(Ordering::Relaxed);
            let volume_db = f32::from_bits(self.volume_bits.load(Ordering::Relaxed));
            let muted = self.muted.load(Ordering::Relaxed);
            std::sync::atomic::fence(Ordering::Acquire);
            let after = self.format_seq.load(Ordering::Relaxed);
            if before == after {
                return (
                    decode_format(tag, a, b, c),
                    epoch,
                    unsupported,
                    volume_db,
                    muted,
                );
            }
        }
    }
}

#[derive(Debug)]
struct Shared {
    stop: AtomicBool,
    capture_done: AtomicBool,
    capture_input_ended: AtomicBool,
    capture_started: AtomicBool,
    scene_rate: AtomicU32,
    device_rate: AtomicU32,
    clock_mode: AtomicU32,
    trim_available: AtomicBool,
    trim_ppm_bits: AtomicU64,
    mute: AtomicBool,
    blocks: AtomicU64,
    underruns: AtomicU64,
    /// Origin of the nanosecond stamps below.
    started: Instant,
    /// When the capture side last put a block into the crossing ring.
    last_block_ns: AtomicU64,
    /// The last underrun as the output saw it ([`UnderrunSnapshot`]). The
    /// output thread is the only writer; the sequence is odd while it writes.
    underrun_sequence: AtomicU64,
    underrun_fields: [AtomicU32; 6],
    output_restarts: AtomicU64,
    output_channels: AtomicU32,
    output_channel_mask: AtomicU32,
    output_reopens: AtomicU64,
    output_reconnects: AtomicU64,
    output_reconnect_attempts: AtomicU64,
    output_mode: AtomicU32,
    output_fell_back: AtomicBool,
    output_mix_channels: AtomicU32,
    output_cadence_ratio_bits: AtomicU32,
    output_cadence_faults: AtomicU64,
    target_ms_bits: AtomicU32,
    max_target_ms_bits: AtomicU32,
    latency_generation: AtomicU64,
    latency_reset_requested: AtomicU64,
    latency_next: AtomicU64,
    latency_sequences: [AtomicU64; LATENCY_SAMPLES],
    latency_values: [AtomicU32; LATENCY_SAMPLES],
    lost_bytes: AtomicU64,
    lane_underrun_frames: AtomicU64,
    /// Frames per channel of the largest recent capture-step delivery into
    /// the crossing ring ([`ArrivalPeak`]); the output primes for it.
    arrival_frames: AtomicU64,
    dropped_frames: AtomicU64,
    volume_transitions_coalesced: AtomicU64,
    output_overflow_frames: AtomicU64,
    idle_blocks: AtomicU64,
    dropped_events: AtomicU64,
    ring_level_bits: AtomicU32,
    clock_ppm_bits: AtomicU64,
    endpoints: [EndpointShared; ENGINE_ENDPOINTS],
    unattributed_chain_errors: [AtomicU64; ChainErrorKind::COUNT],
    fault: AtomicU32,
    output_error_seq: AtomicU32,
    output_error: [AtomicU32; 256],
}

impl Shared {
    fn new() -> Self {
        Self {
            stop: AtomicBool::new(false),
            capture_done: AtomicBool::new(false),
            capture_input_ended: AtomicBool::new(false),
            capture_started: AtomicBool::new(false),
            scene_rate: AtomicU32::new(0),
            device_rate: AtomicU32::new(0),
            clock_mode: AtomicU32::new(ClockMode::Asrc.as_u32()),
            trim_available: AtomicBool::new(false),
            trim_ppm_bits: AtomicU64::new(0.0_f64.to_bits()),
            mute: AtomicBool::new(false),
            blocks: AtomicU64::new(0),
            underruns: AtomicU64::new(0),
            started: Instant::now(),
            last_block_ns: AtomicU64::new(0),
            underrun_sequence: AtomicU64::new(0),
            underrun_fields: std::array::from_fn(|_| AtomicU32::new(0)),
            output_restarts: AtomicU64::new(0),
            output_channels: AtomicU32::new(0),
            output_channel_mask: AtomicU32::new(0),
            output_reopens: AtomicU64::new(0),
            output_reconnects: AtomicU64::new(0),
            output_reconnect_attempts: AtomicU64::new(0),
            output_mode: AtomicU32::new(OutputMode::Exclusive as u32),
            output_fell_back: AtomicBool::new(false),
            output_mix_channels: AtomicU32::new(0),
            output_cadence_ratio_bits: AtomicU32::new(1.0_f32.to_bits()),
            output_cadence_faults: AtomicU64::new(0),
            target_ms_bits: AtomicU32::new(10.0_f32.to_bits()),
            max_target_ms_bits: AtomicU32::new(0.0_f32.to_bits()),
            latency_generation: AtomicU64::new(2),
            latency_reset_requested: AtomicU64::new(0),
            latency_next: AtomicU64::new(0),
            latency_sequences: std::array::from_fn(|_| AtomicU64::new(0)),
            latency_values: std::array::from_fn(|_| AtomicU32::new(0)),
            lost_bytes: AtomicU64::new(0),
            lane_underrun_frames: AtomicU64::new(0),
            arrival_frames: AtomicU64::new(0),
            dropped_frames: AtomicU64::new(0),
            volume_transitions_coalesced: AtomicU64::new(0),
            output_overflow_frames: AtomicU64::new(0),
            idle_blocks: AtomicU64::new(0),
            dropped_events: AtomicU64::new(0),
            ring_level_bits: AtomicU32::new(0.0_f32.to_bits()),
            clock_ppm_bits: AtomicU64::new(0.0_f64.to_bits()),
            endpoints: std::array::from_fn(|_| EndpointShared::new()),
            unattributed_chain_errors: std::array::from_fn(|_| AtomicU64::new(0)),
            fault: AtomicU32::new(FAULT_NONE),
            output_error_seq: AtomicU32::new(0),
            output_error: std::array::from_fn(|_| AtomicU32::new(0)),
        }
    }

    fn publish_output_error(&self, error: &str) {
        self.output_error_seq.fetch_add(1, Ordering::AcqRel);
        let bytes = error.as_bytes();
        for (index, slot) in self.output_error.iter().enumerate() {
            slot.store(
                bytes.get(index).copied().map_or(0, u32::from),
                Ordering::Relaxed,
            );
        }
        self.output_error_seq.fetch_add(1, Ordering::Release);
    }

    fn output_error(&self) -> String {
        loop {
            let before = self.output_error_seq.load(Ordering::Acquire);
            if before & 1 != 0 {
                std::hint::spin_loop();
                continue;
            }
            let mut bytes = Vec::with_capacity(self.output_error.len());
            for slot in &self.output_error {
                let byte = slot.load(Ordering::Relaxed) as u8;
                if byte == 0 {
                    break;
                }
                bytes.push(byte);
            }
            std::sync::atomic::fence(Ordering::Acquire);
            if before == self.output_error_seq.load(Ordering::Relaxed) {
                return String::from_utf8_lossy(&bytes).into_owned();
            }
        }
    }

    fn publish_output_format(&self, format: OutputFormat) {
        self.device_rate.store(format.rate, Ordering::Release);
        self.output_channels
            .store(u32::from(format.channels), Ordering::Release);
        self.output_channel_mask
            .store(format.channel_mask, Ordering::Release);
        self.output_mode
            .store(format.mode as u32, Ordering::Release);
        self.output_fell_back
            .store(format.fell_back, Ordering::Release);
        self.output_mix_channels
            .store(u32::from(format.mix_channels), Ordering::Release);
    }

    fn request_latency_reset(&self) {
        self.latency_reset_requested.fetch_add(1, Ordering::Release);
    }

    fn reset_latency(&self) -> u64 {
        // Odd generations are an in-progress reset. Clearing the published
        // length before making the next even generation visible prevents a
        // status reader from pairing a fresh generation with stale samples.
        let resetting = self.latency_generation.fetch_add(1, Ordering::AcqRel) + 1;
        debug_assert_ne!(resetting & 1, 0);
        self.latency_next.store(0, Ordering::Release);
        let generation = resetting.wrapping_add(1);
        self.latency_generation.store(generation, Ordering::Release);
        generation
    }

    fn latency(&self) -> LatencyStats {
        loop {
            let generation = self.latency_generation.load(Ordering::Acquire);
            if generation & 1 != 0 {
                std::hint::spin_loop();
                continue;
            }
            let next = self.latency_next.load(Ordering::Acquire);
            let len = usize::try_from(next.min(LATENCY_SAMPLES as u64)).unwrap_or(LATENCY_SAMPLES);
            if len == 0 {
                if generation == self.latency_generation.load(Ordering::Acquire) {
                    return LatencyStats::default();
                }
                continue;
            }
            let start = next.saturating_sub(len as u64);
            let mut values = Vec::with_capacity(len);
            let mut complete = true;
            for sequence in start..next {
                let index = sequence as usize % LATENCY_SAMPLES;
                if self.latency_sequences[index].load(Ordering::Acquire) != sequence.wrapping_add(1)
                {
                    complete = false;
                    break;
                }
                values.push(f32::from_bits(
                    self.latency_values[index].load(Ordering::Relaxed),
                ));
                std::sync::atomic::fence(Ordering::Acquire);
                if self.latency_sequences[index].load(Ordering::Relaxed) != sequence.wrapping_add(1)
                {
                    complete = false;
                    break;
                }
            }
            if complete && generation == self.latency_generation.load(Ordering::Acquire) {
                values.sort_unstable_by(f32::total_cmp);
                return LatencyStats {
                    p50_ms: values[percentile_index(len, 50)],
                    p99_ms: values[percentile_index(len, 99)],
                    samples: len as u32,
                };
            }
        }
    }

    fn publish_format(&self, endpoint: u16, epoch: u32, format: SourceFormat, unsupported: u32) {
        if let Some(status) = self.endpoints.get(endpoint as usize) {
            status.publish(epoch, format, unsupported);
        }
    }

    fn publish_unsupported(&self, endpoint: u16, code: u32) {
        if let Some(status) = self.endpoints.get(endpoint as usize) {
            status.format_seq.fetch_add(1, Ordering::AcqRel);
            status.unsupported.store(code, Ordering::Relaxed);
            status.format_seq.fetch_add(1, Ordering::Release);
        }
    }

    fn publish_volume(&self, endpoint: u16, gain: f32, muted: bool) {
        if let Some(status) = self.endpoints.get(endpoint as usize) {
            status.publish_volume(gain, muted);
        }
    }

    fn note_chain_error(&self, endpoint: Option<u16>, kind: ChainErrorKind) {
        match endpoint {
            Some(endpoint) => {
                if let Some(status) = self.endpoints.get(endpoint as usize) {
                    status.note_chain_error(kind);
                }
            }
            None => {
                self.unattributed_chain_errors[kind.index()].fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    fn read_unattributed_chain_errors(&self) -> ChainErrorCounts {
        ChainErrorCounts::from_fn(|kind| {
            self.unattributed_chain_errors[kind.index()].load(Ordering::Relaxed)
        })
    }

    fn read_endpoint(&self, endpoint: usize) -> EndpointStatus {
        let status = &self.endpoints[endpoint];
        let (format, epoch, unsupported_code, volume_db, muted) = status.read();
        let unsupported = match unsupported_code {
            UNSUPPORTED_FORMAT => Some("source format is unsupported".to_owned()),
            UNSUPPORTED_SCENE_RATE => {
                Some("scene rate cannot be converted to the 48 kHz mix rate".to_owned())
            }
            UNSUPPORTED_DIRECT_LAYOUT => Some(
                "PCM with channel mask 0 has no direct layout for its channel count".to_owned(),
            ),
            _ => None,
        };
        EndpointStatus {
            format,
            epoch,
            volume_db,
            muted,
            unsupported,
            chain_errors: status.read_chain_errors(),
        }
    }

    fn elapsed_ns(&self) -> u64 {
        u64::try_from(self.started.elapsed().as_nanos()).unwrap_or(u64::MAX)
    }

    fn note_block(&self) {
        self.last_block_ns
            .store(self.elapsed_ns(), Ordering::Release);
    }

    fn publish_underrun(&self, mut snapshot: UnderrunSnapshot) {
        let since = self
            .elapsed_ns()
            .saturating_sub(self.last_block_ns.load(Ordering::Acquire));
        snapshot.since_block_ms = since as f32 / 1.0e6;
        let sequence = self.underrun_sequence.load(Ordering::Relaxed);
        self.underrun_sequence
            .store(sequence.wrapping_add(1), Ordering::Release);
        let fields = [
            snapshot.ring_frames,
            snapshot.marker_frames,
            snapshot.layout_frames,
            snapshot.needed_frames,
            snapshot.produced_frames,
            snapshot.since_block_ms.to_bits(),
        ];
        for (slot, value) in self.underrun_fields.iter().zip(fields) {
            slot.store(value, Ordering::Release);
        }
        self.underrun_sequence
            .store(sequence.wrapping_add(2), Ordering::Release);
    }

    fn last_underrun(&self) -> Option<UnderrunSnapshot> {
        for _ in 0..4 {
            let before = self.underrun_sequence.load(Ordering::Acquire);
            if before == 0 {
                return None;
            }
            let fields = self
                .underrun_fields
                .each_ref()
                .map(|slot| slot.load(Ordering::Acquire));
            if before.is_multiple_of(2) && self.underrun_sequence.load(Ordering::Acquire) == before
            {
                return Some(UnderrunSnapshot {
                    ring_frames: fields[0],
                    marker_frames: fields[1],
                    layout_frames: fields[2],
                    needed_frames: fields[3],
                    produced_frames: fields[4],
                    since_block_ms: f32::from_bits(fields[5]),
                });
            }
            std::hint::spin_loop();
        }
        None
    }

    fn status(&self) -> EngineStatus {
        EngineStatus {
            endpoints: std::array::from_fn(|endpoint| self.read_endpoint(endpoint)),
            unattributed_chain_errors: self.read_unattributed_chain_errors(),
            latency: self.latency(),
            ring_level_ms: f32::from_bits(self.ring_level_bits.load(Ordering::Acquire)),
            underruns: self.underruns.load(Ordering::Acquire),
            last_underrun: self.last_underrun(),
            output_restarts: self.output_restarts.load(Ordering::Acquire),
            output_rate: self.device_rate.load(Ordering::Acquire),
            trim_available: self.trim_available.load(Ordering::Acquire),
            output_channels: self.output_channels.load(Ordering::Acquire) as u16,
            output_channel_mask: self.output_channel_mask.load(Ordering::Acquire),
            output_reopens: self.output_reopens.load(Ordering::Acquire),
            output_reconnects: self.output_reconnects.load(Ordering::Acquire),
            output_reconnect_attempts: self.output_reconnect_attempts.load(Ordering::Acquire),
            output_mode: if self.output_mode.load(Ordering::Acquire) == OutputMode::Shared as u32 {
                OutputMode::Shared
            } else {
                OutputMode::Exclusive
            },
            output_fell_back: self.output_fell_back.load(Ordering::Acquire),
            output_mix_channels: self.output_mix_channels.load(Ordering::Acquire) as u16,
            output_cadence_ratio: f32::from_bits(
                self.output_cadence_ratio_bits.load(Ordering::Acquire),
            ),
            output_cadence_faults: self.output_cadence_faults.load(Ordering::Acquire),
            lost_bytes: self.lost_bytes.load(Ordering::Acquire),
            lane_underrun_frames: self.lane_underrun_frames.load(Ordering::Acquire),
            dropped_frames: self.dropped_frames.load(Ordering::Acquire),
            volume_transitions_coalesced: self.volume_transitions_coalesced.load(Ordering::Acquire),
            output_overflow_frames: self.output_overflow_frames.load(Ordering::Acquire),
            idle_blocks: self.idle_blocks.load(Ordering::Acquire),
            dropped_events: self.dropped_events.load(Ordering::Acquire),
            clock: match ClockMode::from_u32(self.clock_mode.load(Ordering::Acquire)) {
                ClockMode::Trim => ClockStatus::Trim,
                ClockMode::Asrc => ClockStatus::Asrc,
            },
            clock_ppm: f64::from_bits(self.clock_ppm_bits.load(Ordering::Acquire)),
            fault: match self.fault.load(Ordering::Acquire) {
                value if value & FAULT_OUTPUT != 0 => {
                    Some(EngineFault::OutputLost(self.output_error()))
                }
                value if value & FAULT_CAPTURE != 0 => {
                    Some(EngineFault::CaptureLost("capture source failed".into()))
                }
                _ => None,
            },
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum RtEvent {
    FormatChanged {
        endpoint: u16,
        epoch: u32,
        format: SourceFormat,
    },
    NeedsPrepared,
    Overrun {
        lost_bytes: u64,
    },
    CaptureFault,
    OutputFault,
    OutputOverflow {
        frames: u64,
    },
    OutputLayoutChanged {
        layout: OutputLayout,
    },
    OutputReconnected,
    FaultCleared,
    OutputCadenceFault {
        ratio: f32,
    },
    OutputCadenceReopen {
        period_ms: f32,
    },
}

impl RtEvent {
    fn into_public(self, shared: &Shared) -> EngineEvent {
        match self {
            Self::FormatChanged {
                endpoint,
                epoch,
                format,
            } => EngineEvent::FormatChanged {
                endpoint,
                epoch,
                format,
            },
            Self::NeedsPrepared => EngineEvent::NeedsPrepared,
            Self::Overrun { lost_bytes } => EngineEvent::Overrun { lost_bytes },
            Self::CaptureFault => {
                EngineEvent::Fault(EngineFault::CaptureLost("capture source failed".into()))
            }
            Self::OutputFault => EngineEvent::Fault(EngineFault::OutputLost(shared.output_error())),
            Self::OutputOverflow { frames } => EngineEvent::OutputOverflow { frames },
            Self::OutputLayoutChanged { layout } => EngineEvent::OutputLayoutChanged {
                channels: layout.channels,
                channel_mask: layout.channel_mask,
            },
            Self::OutputReconnected => EngineEvent::OutputReconnected,
            Self::FaultCleared => EngineEvent::FaultCleared,
            Self::OutputCadenceFault { ratio } => EngineEvent::OutputCadenceFault { ratio },
            Self::OutputCadenceReopen { period_ms } => {
                EngineEvent::OutputCadenceReopen { period_ms }
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct TimingMarker {
    stream_frame: u64,
    frames: u32,
    source_qpc: u64,
    rate: u32,
    layout: OutputLayout,
}

struct EventWriter {
    producer: Producer<RtEvent>,
    shared: Arc<Shared>,
    /// Overflowed frames not yet reported in an event.
    overflow_pending: u64,
    /// When the last overflow event went out, from [`Shared::elapsed_ns`].
    overflow_reported_ns: Option<u64>,
}

impl EventWriter {
    fn new(producer: Producer<RtEvent>, shared: Arc<Shared>) -> Self {
        Self {
            producer,
            shared,
            overflow_pending: 0,
            overflow_reported_ns: None,
        }
    }

    fn push(&mut self, event: RtEvent) {
        if self.producer.push(event).is_err() {
            self.shared.dropped_events.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Reports `frames` that did not fit the crossing ring. While the output
    /// is stalled every delivery overflows; one event per block filled the
    /// event queue (1,148 dropped events in a CI soak whose device stopped)
    /// and printed a line per 10 ms. A burst reports its first block at once,
    /// then at most once a second, then what is left when a block fits again
    /// ([`Self::overflow_ended`]); `output_overflow_frames` stays exact.
    fn overflow(&mut self, frames: u64) {
        self.overflow_pending = self.overflow_pending.saturating_add(frames);
        let now = self.shared.elapsed_ns();
        if self
            .overflow_reported_ns
            .is_none_or(|last| now.saturating_sub(last) >= OVERFLOW_REPORT_NS)
        {
            self.report_overflow(now);
        }
    }

    /// A block fitted: reports the rest of an overflow burst.
    fn overflow_ended(&mut self) {
        if self.overflow_pending != 0 {
            let now = self.shared.elapsed_ns();
            self.report_overflow(now);
        }
        self.overflow_reported_ns = None;
    }

    fn report_overflow(&mut self, now: u64) {
        let frames = std::mem::take(&mut self.overflow_pending);
        self.push(RtEvent::OutputOverflow { frames });
        self.overflow_reported_ns = Some(now);
    }

    fn fault(&mut self, fault: u32, event: RtEvent) {
        let previous = self.shared.fault.fetch_or(fault, Ordering::AcqRel);
        if previous & fault == 0 {
            self.push(event);
        }
    }

    fn clear_fault(&mut self, fault: u32) {
        let previous = self.shared.fault.fetch_and(!fault, Ordering::AcqRel);
        if previous & fault != 0 && previous & !fault == FAULT_NONE {
            self.push(RtEvent::FaultCleared);
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CaptureStep {
    Continue,
    Finished,
    Lost,
}

fn apply_requested_trim<C: CaptureSource>(
    capture: &mut C,
    trim_supported: bool,
    shared: &Shared,
    applied_trim: &mut f64,
) {
    let requested_trim =
        if ClockMode::from_u32(shared.clock_mode.load(Ordering::Acquire)) == ClockMode::Trim {
            f64::from_bits(shared.trim_ppm_bits.load(Ordering::Acquire))
        } else {
            0.0
        };
    let changed = if requested_trim == 0.0 {
        *applied_trim != 0.0
    } else {
        !applied_trim.is_finite() || (requested_trim - *applied_trim).abs() > 0.1
    };
    if trim_supported && changed && capture.set_rate_trim(requested_trim).is_ok() {
        *applied_trim = requested_trim;
    }
}

#[allow(clippy::too_many_arguments)]
fn capture_rt_step<C: CaptureSource>(
    capture: &mut C,
    read_wait: Duration,
    trim_supported: bool,
    changes: &mut Consumer<LiveChange>,
    retired: &mut Producer<Box<Prepared>>,
    audio: &mut Producer<f32>,
    timing: &mut Producer<TimingMarker>,
    shared: &Shared,
    chain: &mut Chain,
    events: &mut EventWriter,
    read_buffer: &mut [u8],
    stream_frame: &mut u64,
    applied_trim: &mut f64,
    pending_retired: &mut Option<Box<Prepared>>,
    finishing: &mut bool,
) -> CaptureStep {
    drain_changes(changes, chain, retired, pending_retired, shared);
    apply_requested_trim(capture, trim_supported, shared, applied_trim);
    if *finishing {
        let block_samples = chain
            .output_channels()
            .unwrap_or(MAX_OUTPUT_CHANNELS)
            .saturating_mul(MIX_FRAMES);
        let block_budget = audio
            .slots()
            .checked_div(block_samples)
            .unwrap_or(0)
            .min(timing.slots());
        return if chain.finish_step(block_budget, &mut |event| {
            handle_chain_event(event, audio, timing, stream_frame, events, shared);
        }) {
            CaptureStep::Finished
        } else {
            CaptureStep::Continue
        };
    }

    let bytes = match capture.read(read_buffer, read_wait) {
        Ok(bytes) => bytes,
        Err(CaptureError::Ended) => {
            shared.capture_input_ended.store(true, Ordering::Release);
            chain.begin_finish();
            *finishing = true;
            return CaptureStep::Continue;
        }
        Err(_) => return CaptureStep::Lost,
    };
    chain.push_records(&read_buffer[..bytes], &mut |event| {
        handle_chain_event(event, audio, timing, stream_frame, events, shared);
    });
    if let Some(qpc) = capture.now_qpc() {
        chain.idle(qpc, &mut |event| {
            handle_chain_event(event, audio, timing, stream_frame, events, shared);
        });
    }
    shared
        .lane_underrun_frames
        .store(chain.lane_underrun_frames(), Ordering::Release);
    shared
        .dropped_frames
        .store(chain.dropped_frames(), Ordering::Release);
    shared
        .volume_transitions_coalesced
        .store(chain.volume_transitions_coalesced(), Ordering::Release);
    CaptureStep::Continue
}

#[allow(clippy::too_many_arguments)]
fn capture_thread_main<C: CaptureSource>(
    config: EngineConfig,
    mut capture: C,
    trim_supported: bool,
    mut changes: Consumer<LiveChange>,
    event_producer: Producer<RtEvent>,
    mut retired: Producer<Box<Prepared>>,
    mut audio: Producer<f32>,
    mut timing: Producer<TimingMarker>,
    shared: Arc<Shared>,
    mut chain: Chain,
) -> Option<Box<Prepared>> {
    if let Some(hook) = &config.thread_hook {
        hook(RealtimeThread::Capture);
    }
    let mut read_buffer = vec![0_u8; config.read_buffer_bytes];
    let mut events = EventWriter::new(event_producer, Arc::clone(&shared));
    let mut stream_frame = 0_u64;
    let mut applied_trim = f64::NAN;
    let mut pending_retired: Option<Box<Prepared>> = None;
    let mut arrivals = ArrivalPeak::new();
    shared.capture_started.store(true, Ordering::Release);
    let mut finishing = false;
    while !shared.stop.load(Ordering::Acquire) {
        let mut step = None;
        let step_start = stream_frame;
        let mut work = || {
            step = Some(capture_rt_step(
                &mut capture,
                config.read_wait,
                trim_supported,
                &mut changes,
                &mut retired,
                &mut audio,
                &mut timing,
                &shared,
                &mut chain,
                &mut events,
                &mut read_buffer,
                &mut stream_frame,
                &mut applied_trim,
                &mut pending_retired,
                &mut finishing,
            ));
        };
        if let Some(scope) = &config.rt_scope {
            scope(RtSection::Capture, &mut work);
        } else {
            work();
        }
        let delivered = stream_frame.saturating_sub(step_start);
        if delivered != 0 {
            shared
                .arrival_frames
                .store(arrivals.record(delivered), Ordering::Release);
        }
        if let Some(hook) = &config.rt_event {
            hook(RtLoopEvent::CaptureEnd);
        }
        match step.expect("the real-time scope must run its closure exactly once") {
            CaptureStep::Continue => {
                if finishing {
                    std::thread::yield_now();
                }
            }
            CaptureStep::Finished => break,
            CaptureStep::Lost => {
                events.fault(FAULT_CAPTURE, RtEvent::CaptureFault);
                let mut delay = 0;
                loop {
                    if delay != 0 && wait_for_stop(&shared, Duration::from_millis(delay)) {
                        shared.capture_done.store(true, Ordering::Release);
                        if let Some(hook) = &config.rt_event {
                            hook(RtLoopEvent::CaptureFinished);
                        }
                        return pending_retired;
                    }
                    match capture.reconnect() {
                        Ok(()) => {
                            applied_trim = f64::NAN;
                            shared.request_latency_reset();
                            events.clear_fault(FAULT_CAPTURE);
                            break;
                        }
                        Err(_) => {
                            delay = if delay == 0 {
                                RECONNECT_INITIAL_MS
                            } else {
                                (delay * 2).min(RECONNECT_MAX_MS)
                            };
                        }
                    }
                }
            }
        }
    }

    drain_changes(
        &mut changes,
        &mut chain,
        &mut retired,
        &mut pending_retired,
        &shared,
    );
    shared
        .lane_underrun_frames
        .store(chain.lane_underrun_frames(), Ordering::Release);
    shared
        .dropped_frames
        .store(chain.dropped_frames(), Ordering::Release);
    shared
        .volume_transitions_coalesced
        .store(chain.volume_transitions_coalesced(), Ordering::Release);
    shared.capture_done.store(true, Ordering::Release);
    if let Some(hook) = &config.rt_event {
        hook(RtLoopEvent::CaptureFinished);
    }
    pending_retired
}

fn handle_chain_event(
    event: ChainEvent<'_>,
    audio: &mut Producer<f32>,
    timing: &mut Producer<TimingMarker>,
    stream_frame: &mut u64,
    events: &mut EventWriter,
    shared: &Shared,
) {
    match event {
        ChainEvent::Block(block) => {
            push_block(block, audio, timing, stream_frame, events, shared);
        }
        ChainEvent::OutputChannels { .. } => {}
        ChainEvent::FormatChanged {
            endpoint,
            epoch,
            format,
            support,
        } => {
            let previous_epoch = shared
                .endpoints
                .get(endpoint as usize)
                .map_or(epoch, |status| status.epoch.load(Ordering::Acquire));
            // The mixer chooses the latency QPC from the minimum
            // (lane delay, lane index), so any endpoint can become the source.
            if previous_epoch != epoch {
                shared.request_latency_reset();
            }
            let unsupported = match support {
                crate::chain::FormatSupport::Supported => UNSUPPORTED_NONE,
                crate::chain::FormatSupport::UnsupportedFormat => UNSUPPORTED_FORMAT,
                crate::chain::FormatSupport::UnsupportedSceneRate => UNSUPPORTED_SCENE_RATE,
                crate::chain::FormatSupport::UnsupportedDirectLayout => UNSUPPORTED_DIRECT_LAYOUT,
            };
            shared.publish_format(endpoint, epoch, format, unsupported);
            events.push(RtEvent::FormatChanged {
                endpoint,
                epoch,
                format,
            });
        }
        ChainEvent::NeedsPrepared => events.push(RtEvent::NeedsPrepared),
        ChainEvent::Overrun { lost_bytes, .. } => {
            shared.lost_bytes.fetch_add(lost_bytes, Ordering::Relaxed);
            events.push(RtEvent::Overrun { lost_bytes });
        }
        ChainEvent::Volume {
            endpoint,
            gain,
            muted,
        } => shared.publish_volume(endpoint, gain, muted),
        ChainEvent::Error { endpoint, error } => {
            shared.note_chain_error(endpoint, crate::chain::chain_error_kind(&error));
            if let Some(endpoint) = endpoint {
                match error {
                    ChainError::UnsupportedSceneRate(_) => {
                        shared.publish_unsupported(endpoint, UNSUPPORTED_SCENE_RATE);
                    }
                    ChainError::Decode(decode::DecodeError::NoDirectLayout(_)) => {
                        shared.publish_unsupported(endpoint, UNSUPPORTED_DIRECT_LAYOUT);
                    }
                    _ if is_unsupported(&error) => {
                        shared.publish_unsupported(endpoint, UNSUPPORTED_FORMAT);
                    }
                    _ => {}
                }
            }
        }
    }
}

fn push_block(
    block: crate::chain::BlockOut<'_>,
    audio: &mut Producer<f32>,
    timing: &mut Producer<TimingMarker>,
    stream_frame: &mut u64,
    events: &mut EventWriter,
    shared: &Shared,
) {
    let frames = block.frames;
    if frames == 0 {
        return;
    }
    debug_assert_eq!(block.rate, crate::MIX_RATE);
    let samples = frames.saturating_mul(block.channels);
    if audio.slots() < samples || timing.slots() == 0 {
        if !(block.idle && block.silent) {
            shared
                .output_overflow_frames
                .fetch_add(frames as u64, Ordering::Relaxed);
            events.overflow(frames as u64);
        }
        return;
    }
    events.overflow_ended();

    // Publish metadata first. The capacity preflight above means all following
    // pushes succeed, so the output cannot observe samples without a marker.
    let _ = timing.push(TimingMarker {
        stream_frame: *stream_frame,
        frames: frames.min(u32::MAX as usize) as u32,
        source_qpc: block.source_qpc,
        rate: block.rate,
        layout: OutputLayout {
            channels: block.channels as u16,
            channel_mask: block.channel_mask,
        },
    });
    for frame in 0..frames {
        for channel in 0..block.channels {
            let _ = audio.push(block.channel(channel)[frame]);
        }
    }
    *stream_frame = stream_frame.saturating_add(frames as u64);
    let previous_rate = shared.scene_rate.swap(crate::MIX_RATE, Ordering::Release);
    debug_assert!(previous_rate == 0 || previous_rate == crate::MIX_RATE);
    if block.idle {
        shared.idle_blocks.fetch_add(1, Ordering::Relaxed);
    } else {
        shared.blocks.fetch_add(1, Ordering::Relaxed);
    }
    shared.note_block();
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
            LiveChange::LfeGain(db) => chain.set_lfe_gain(10.0_f32.powf(db / 20.0)),
            LiveChange::TargetMs(ms) => {
                shared.target_ms_bits.store(ms.to_bits(), Ordering::Release)
            }
        }
    }
}

fn boundary_fade_frames(rate: u32) -> usize {
    ((f64::from(rate) * BOUNDARY_FADE_MS / 1_000.0).round() as usize).max(1)
}

/// Gain of drain frame `frame` of an ASRC tail. The old resampler's last
/// `real_frames` drained frames are real signal; the boundary fade ends at zero
/// on the last of them, over its last `fade_frames` (the rest of the fade,
/// `lead_frames`, lies in the last rendered period when the delay is shorter
/// than the fade). Real frames before the fade play at full level and frames
/// after the signal are silent.
fn asrc_drain_gain(
    frame: usize,
    real_frames: usize,
    fade_frames: usize,
    lead_frames: usize,
) -> f32 {
    if frame >= real_frames {
        return 0.0;
    }
    let fade_start = real_frames - fade_frames.min(real_frames);
    if frame < fade_start {
        return 1.0;
    }
    let total = lead_frames + fade_frames;
    if total <= 1 {
        return 0.0;
    }
    let offset = lead_frames + (frame - fade_start);
    (total - 1 - offset) as f32 / (total - 1) as f32
}

#[derive(Debug, PartialEq, Eq)]
struct TailFade {
    /// Fade frames in the final rendered period, ending at `period_signal_end`.
    lead_frames: usize,
    /// Frame of the final rendered period after the signal's last; frames
    /// from here to the period's end become zero. Unused without a period.
    period_signal_end: usize,
    /// Real-signal frames in the drain, and the fade's frames among them.
    drain_real_frames: usize,
    drain_fade_frames: usize,
}

/// Plans the ASRC boundary fade across the last rendered period and its drain.
/// `rendered` is `(period_frames, real_output_frames)` of the final rendered
/// period when the fade may still touch it, or `None` when that period already
/// went to the sink.
fn plan_asrc_tail_fade(delay: usize, fade: usize, rendered: Option<(usize, usize)>) -> TailFade {
    let Some((period_frames, real_output_frames)) = rendered else {
        return TailFade {
            lead_frames: 0,
            period_signal_end: 0,
            drain_real_frames: delay,
            drain_fade_frames: fade.min(delay),
        };
    };
    let signal_end = real_output_frames.saturating_add(delay);
    if signal_end >= period_frames {
        let drain_real_frames = signal_end - period_frames;
        let drain_fade_frames = fade.min(drain_real_frames);
        TailFade {
            lead_frames: fade.saturating_sub(drain_fade_frames).min(period_frames),
            period_signal_end: period_frames,
            drain_real_frames,
            drain_fade_frames,
        }
    } else {
        TailFade {
            lead_frames: fade.min(signal_end),
            period_signal_end: signal_end,
            drain_real_frames: 0,
            drain_fade_frames: 0,
        }
    }
}

fn apply_fade_in(
    buffer: &mut [f32],
    channels: usize,
    rate: u32,
    audible_frames: usize,
    remaining: &mut usize,
) {
    if *remaining == 0 || audible_frames == 0 {
        return;
    }
    let total = boundary_fade_frames(rate);
    let used = total.saturating_sub(*remaining);
    for (offset, frame) in buffer
        .chunks_exact_mut(channels)
        .take(audible_frames)
        .enumerate()
    {
        if offset >= *remaining {
            break;
        }
        let gain = (used + offset) as f32 / total as f32;
        for sample in frame {
            *sample *= gain;
        }
    }
    *remaining = remaining.saturating_sub(audible_frames);
}

fn select_output_clock(
    preference: ClockPreference,
    trim_available: bool,
    format: OutputFormat,
    layout: OutputLayout,
) -> Result<ClockMode, &'static str> {
    if format.channels != layout.channels {
        return Err("the output sink channel count does not match the requested layout");
    }
    if format.rate < MIN_DEVICE_RATE || format.period_frames == 0 {
        return Err("the output format rate is below 8 kHz or its period is zero");
    }
    if format.period_frames > MAX_OUTPUT_PERIOD_FRAMES {
        return Err("the output period exceeds the runtime's 16,384-frame limit");
    }
    match preference {
        ClockPreference::Trim if format.rate != crate::MIX_RATE => {
            Err("Trim requires a 48 kHz output device for the common mix rate")
        }
        ClockPreference::Trim => Ok(ClockMode::Trim),
        ClockPreference::Auto if trim_available && format.rate == crate::MIX_RATE => {
            Ok(ClockMode::Trim)
        }
        ClockPreference::Auto | ClockPreference::Asrc => Ok(ClockMode::Asrc),
    }
}

fn configure_reopened_output(
    output: &mut OutputState,
    shared: &Shared,
    preference: ClockPreference,
    layout: OutputLayout,
    format: OutputFormat,
) -> Result<(), &'static str> {
    let mode = select_output_clock(
        preference,
        shared.trim_available.load(Ordering::Acquire),
        format,
        layout,
    )?;
    if mode != ClockMode::Trim {
        shared
            .trim_ppm_bits
            .store(0.0_f64.to_bits(), Ordering::Release);
    }
    shared.clock_mode.store(mode.as_u32(), Ordering::Release);
    output.reconfigure(mode, layout, format.rate, format.period_frames);
    shared.publish_output_format(format);
    Ok(())
}

fn wait_for_stop(shared: &Shared, duration: Duration) -> bool {
    // Reopening and its backoff are outside the real-time processing loop: the
    // output thread owns no live device and processes no audio here, so adapter
    // allocation is allowed. Short waits keep Engine::stop prompt.
    let mut remaining = duration;
    while !remaining.is_zero() {
        if shared.stop.load(Ordering::Acquire) {
            return true;
        }
        let step = remaining.min(Duration::from_millis(10));
        std::thread::sleep(step);
        remaining = remaining.saturating_sub(step);
    }
    shared.stop.load(Ordering::Acquire)
}

fn is_unsupported(error: &ChainError) -> bool {
    matches!(
        error,
        ChainError::Decode(decode::DecodeError::UnsupportedCodec(_))
            | ChainError::Decode(decode::DecodeError::Unsupported { .. })
            | ChainError::Decode(decode::DecodeError::NoDirectLayout(_))
            | ChainError::UnsupportedSceneRate(_)
    )
}

#[derive(Clone, Copy, Debug)]
enum ReopenStep {
    ChangeLayout(OutputLayout),
    GrowCadence(Duration),
}

#[derive(Debug)]
enum OutputStep {
    Continue,
    ChangeLayout(OutputLayout),
    Lost(OutputError),
    GrowCadence(Duration),
}

fn output_rt_step<S: OutputSink>(
    output: &mut OutputState,
    sink: &mut S,
    shared: &Shared,
    qpc_frequency: u64,
    sink_restart_baseline: &mut u64,
    cadence_period_grown: bool,
) -> OutputStep {
    if let Some(layout) = output.pending_layout_change() {
        return OutputStep::ChangeLayout(layout);
    }
    output.latency.apply_requested_reset(shared);
    let mut fill = |buffer: &mut [f32]| output.fill(buffer);
    let qpc = match sink.next_period(&mut fill) {
        Ok(qpc) => qpc,
        Err(error) => return OutputStep::Lost(error),
    };
    let restarts = sink.restarts();
    if restarts > *sink_restart_baseline {
        shared
            .output_restarts
            .fetch_add(restarts - *sink_restart_baseline, Ordering::Relaxed);
        *sink_restart_baseline = restarts;
        output.priming = true;
        output.fade_in_remaining = boundary_fade_frames(output.device_rate);
        output.latency.reset(shared);
        output.cadence.reset();
    }
    output.finish_period(qpc);
    if let Some(ratio) =
        output
            .cadence
            .observe(qpc, sink.played_frames(), qpc_frequency, output.device_rate)
    {
        shared
            .output_cadence_ratio_bits
            .store(ratio.to_bits(), Ordering::Release);
        if output.cadence.faulted() {
            shared.output_cadence_faults.fetch_add(1, Ordering::Relaxed);
            output.events.push(RtEvent::OutputCadenceFault { ratio });
            if !cadence_period_grown && sink.format().mode == OutputMode::Exclusive {
                let current = Duration::from_secs_f64(
                    output.period_frames as f64 / f64::from(output.device_rate),
                );
                let grown = current.mul_f64(2.0).min(Duration::from_millis(20));
                if grown > current {
                    return OutputStep::GrowCadence(grown);
                }
            }
        }
    }
    OutputStep::Continue
}

/// Once capture has ended and every frame is rendered, nothing is left to play
/// on a reopened sink, so a layout or cadence reopen gives way to finishing
/// (the tail still owed goes to the current sink).
fn end_before_reopen(step: OutputStep, ended: bool) -> OutputStep {
    match step {
        OutputStep::ChangeLayout(_) | OutputStep::GrowCadence(_) if ended => OutputStep::Continue,
        step => step,
    }
}

fn complete_reopen_step(reopen: ReopenStep) -> OutputStep {
    match reopen {
        ReopenStep::ChangeLayout(layout) => OutputStep::ChangeLayout(layout),
        ReopenStep::GrowCadence(period) => OutputStep::GrowCadence(period),
    }
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
    start_sender: std::sync::mpsc::SyncSender<Result<ssr_types::OutputFormat, OutputError>>,
    clock_receiver: std::sync::mpsc::Receiver<()>,
    initial_layout: OutputLayout,
    capacity_frames: usize,
) {
    if let Some(hook) = &config.thread_hook {
        hook(RealtimeThread::Output);
    }
    let mut opener = output;
    let mut sink = match opener.open(initial_layout) {
        Ok(sink) => sink,
        Err(error) => {
            let _ = start_sender.send(Err(error));
            return;
        }
    };
    let format = sink.format();
    if start_sender.send(Ok(format)).is_err() || clock_receiver.recv().is_err() {
        return;
    }

    shared.publish_output_format(format);
    let mut cadence_period_grown = false;
    let mut sink_restart_baseline = sink.restarts();
    let mut output = OutputState::new(
        audio,
        timing,
        format.rate,
        format.period_frames,
        config.target_ms,
        qpc_frequency,
        event_producer,
        Arc::clone(&shared),
        initial_layout,
        capacity_frames,
    );
    let mut draining_reopen = None;
    while !shared.stop.load(Ordering::Acquire) {
        // Checked before every period: the end of capture can become visible
        // after the last take was rendered, and the resampler's delayed tail
        // then follows that period directly instead of an empty one.
        if output.ended() {
            if let Some(hook) = &config.rt_event {
                hook(RtLoopEvent::OutputTailBegin);
            }
            let mut drain_result = None;
            let mut drain = || drain_result = Some(output.write_terminal_tail(&mut sink));
            if let Some(scope) = &config.rt_scope {
                scope(RtSection::Output, &mut drain);
            } else {
                drain();
            }
            if let Some(Err(error)) = drain_result {
                shared.publish_output_error(&error.to_string());
                output.events.fault(FAULT_OUTPUT, RtEvent::OutputFault);
            }
            if let Some(hook) = &config.rt_event {
                hook(RtLoopEvent::OutputFinished);
            }
            break;
        }
        let mut step = None;
        let mut work = || {
            step = Some(if let Some(reopen) = draining_reopen {
                match output.write_reopen_tail_period(&mut sink) {
                    Ok(true) => complete_reopen_step(reopen),
                    Ok(false) => OutputStep::Continue,
                    Err(error) => OutputStep::Lost(error),
                }
            } else {
                output_rt_step(
                    &mut output,
                    &mut sink,
                    &shared,
                    qpc_frequency,
                    &mut sink_restart_baseline,
                    cadence_period_grown,
                )
            });
        };
        if let Some(scope) = &config.rt_scope {
            scope(RtSection::Output, &mut work);
        } else {
            work();
        }
        if let Some(hook) = &config.rt_event {
            hook(RtLoopEvent::OutputEnd);
        }
        // One decision after the period: whether capture has ended decides both
        // what happens to a reopen and to a reopen drain.
        let ended = output.ended();
        let mut step = end_before_reopen(
            step.expect("the real-time scope must run its closure exactly once"),
            ended,
        );
        if ended {
            draining_reopen = None;
            if let OutputStep::Lost(error) = &step {
                // Nothing is left to play, so there is nothing to reconnect for.
                shared.publish_output_error(&error.to_string());
                output.events.fault(FAULT_OUTPUT, RtEvent::OutputFault);
                if let Some(hook) = &config.rt_event {
                    hook(RtLoopEvent::OutputFinished);
                }
                break;
            }
        }
        if draining_reopen.is_none() {
            let reopen = match step {
                OutputStep::ChangeLayout(layout) => Some(ReopenStep::ChangeLayout(layout)),
                OutputStep::GrowCadence(period) if opener.set_period(period) => {
                    cadence_period_grown = true;
                    Some(ReopenStep::GrowCadence(period))
                }
                OutputStep::GrowCadence(_) => {
                    step = OutputStep::Continue;
                    None
                }
                _ => None,
            };
            if let Some(reopen) = reopen
                && output.begin_reopen_tail()
            {
                draining_reopen = Some(reopen);
                step = OutputStep::Continue;
            }
        } else if !matches!(step, OutputStep::Continue) {
            draining_reopen = None;
        }
        match step {
            OutputStep::Continue => {}
            OutputStep::ChangeLayout(mut layout) => {
                drop(sink);
                let mut delay = 0;
                loop {
                    if delay != 0 && wait_for_stop(&shared, Duration::from_millis(delay)) {
                        return;
                    }
                    if output.ended() {
                        // Capture ended while no sink was open and nothing is queued:
                        // there is nothing left to play on a new one.
                        if let Some(hook) = &config.rt_event {
                            hook(RtLoopEvent::OutputFinished);
                        }
                        return;
                    }
                    if let Some(hook) = &config.rt_event {
                        hook(RtLoopEvent::OutputOpenBegin);
                    }
                    let opened = opener.open(layout);
                    match opened {
                        Ok(reopened) => {
                            let format = reopened.format();
                            match configure_reopened_output(
                                &mut output,
                                &shared,
                                config.clock,
                                layout,
                                format,
                            ) {
                                Ok(()) => {
                                    if let Some(hook) = &config.rt_event {
                                        hook(RtLoopEvent::OutputOpenEnd);
                                    }
                                    sink = reopened;
                                    break;
                                }
                                Err(error) => shared.publish_output_error(error),
                            }
                        }
                        Err(OutputError::FormatRejected(message)) => {
                            shared.publish_output_error(
                                &OutputError::FormatRejected(message).to_string(),
                            );
                            output.discard_pending_layout(layout);
                        }
                        Err(error) => shared.publish_output_error(&error.to_string()),
                    }
                    if let Some(hook) = &config.rt_event {
                        hook(RtLoopEvent::OutputOpenEnd);
                    }
                    output.events.fault(FAULT_OUTPUT, RtEvent::OutputFault);
                    if let Some(next) = output.layout_at_boundary()
                        && next != layout
                    {
                        layout = next;
                        delay = 0;
                        continue;
                    }
                    delay = if delay == 0 {
                        RECONNECT_INITIAL_MS
                    } else {
                        (delay * 2).min(RECONNECT_MAX_MS)
                    };
                }
                sink_restart_baseline = sink.restarts();
                shared.output_reopens.fetch_add(1, Ordering::Relaxed);
                output.events.clear_fault(FAULT_OUTPUT);
                output.events.push(RtEvent::OutputLayoutChanged { layout });
                continue;
            }
            OutputStep::Lost(error) => {
                shared.publish_output_error(&error.to_string());
                output.events.fault(FAULT_OUTPUT, RtEvent::OutputFault);
                drop(sink);
                output.discard_crossing_audio();
                let mut delay = RECONNECT_INITIAL_MS;
                loop {
                    if wait_for_stop(&shared, Duration::from_millis(delay)) {
                        return;
                    }
                    if output.ended() {
                        // Capture ended while no sink was open and nothing is queued:
                        // there is nothing left to play on a new one.
                        if let Some(hook) = &config.rt_event {
                            hook(RtLoopEvent::OutputFinished);
                        }
                        return;
                    }
                    if let Some(hook) = &config.rt_event {
                        hook(RtLoopEvent::OutputOpenBegin);
                    }
                    shared
                        .output_reconnect_attempts
                        .fetch_add(1, Ordering::Relaxed);
                    let opened = opener.open(output.layout);
                    match opened {
                        Ok(reopened) => {
                            let reopened_format = reopened.format();
                            let layout = output.layout;
                            if let Err(error) = configure_reopened_output(
                                &mut output,
                                &shared,
                                config.clock,
                                layout,
                                reopened_format,
                            ) {
                                shared.publish_output_error(error);
                                if let Some(hook) = &config.rt_event {
                                    hook(RtLoopEvent::OutputOpenEnd);
                                }
                                delay = (delay * 2).min(RECONNECT_MAX_MS);
                                continue;
                            }
                            if let Some(hook) = &config.rt_event {
                                hook(RtLoopEvent::OutputOpenEnd);
                            }
                            sink = reopened;
                            sink_restart_baseline = sink.restarts();
                            shared.output_reconnects.fetch_add(1, Ordering::Relaxed);
                            output.events.clear_fault(FAULT_OUTPUT);
                            output.events.push(RtEvent::OutputReconnected);
                            break;
                        }
                        Err(reopen_error) => {
                            shared.publish_output_error(&reopen_error.to_string());
                            if let Some(hook) = &config.rt_event {
                                hook(RtLoopEvent::OutputOpenEnd);
                            }
                            delay = (delay * 2).min(RECONNECT_MAX_MS);
                        }
                    }
                }
            }
            OutputStep::GrowCadence(_) => {
                drop(sink);
                let mut delay = 0;
                loop {
                    if delay != 0 && wait_for_stop(&shared, Duration::from_millis(delay)) {
                        return;
                    }
                    if output.ended() {
                        // Capture ended while no sink was open and nothing is queued:
                        // there is nothing left to play on a new one.
                        if let Some(hook) = &config.rt_event {
                            hook(RtLoopEvent::OutputFinished);
                        }
                        return;
                    }
                    if let Some(hook) = &config.rt_event {
                        hook(RtLoopEvent::OutputOpenBegin);
                    }
                    let reopened = opener.open(output.layout);
                    match reopened {
                        Ok(reopened) => {
                            let format = reopened.format();
                            let layout = output.layout;
                            match configure_reopened_output(
                                &mut output,
                                &shared,
                                config.clock,
                                layout,
                                format,
                            ) {
                                Ok(()) => {
                                    if let Some(hook) = &config.rt_event {
                                        hook(RtLoopEvent::OutputOpenEnd);
                                    }
                                    sink = reopened;
                                    sink_restart_baseline = sink.restarts();
                                    output.events.clear_fault(FAULT_OUTPUT);
                                    output.events.push(RtEvent::OutputCadenceReopen {
                                        period_ms: format.period_frames as f32 * 1_000.0
                                            / format.rate as f32,
                                    });
                                    break;
                                }
                                Err(error) => shared.publish_output_error(error),
                            }
                        }
                        Err(error) => shared.publish_output_error(&error.to_string()),
                    }
                    if let Some(hook) = &config.rt_event {
                        hook(RtLoopEvent::OutputOpenEnd);
                    }
                    output.events.fault(FAULT_OUTPUT, RtEvent::OutputFault);
                    delay = if delay == 0 {
                        RECONNECT_INITIAL_MS
                    } else {
                        (delay * 2).min(RECONNECT_MAX_MS)
                    };
                }
                continue;
            }
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
    marker_queue: VecDeque<TimingMarker>,
    layout: OutputLayout,
    channels: usize,
    period_frames: usize,
    period_ring_level_frames: usize,
    /// Ring level left after this period's frames were taken.
    period_trough_frames: usize,
    /// Set at start, after an underrun or a device restart, and when the
    /// strategy or scene rate changes: output silence until the ring holds
    /// target + one delivery + one period, then drop the exact-zero frames
    /// beyond that, so the trough starts at the target instead of at zero.
    priming: bool,
    /// Crossing-ring capacity in frames per channel.
    capacity_frames: usize,
    period_stream_start: u64,
    period_source_frames: usize,
    period_had_audio: bool,
    mute_gain: f32,
    fade_in_remaining: usize,
    last_output: [f32; MAX_OUTPUT_CHANNELS],
    have_last_output: bool,
    latency: LatencyWriter,
    cadence: CadenceGuard,
    /// Periods of the resampler's tail still owed to the current sink, by a
    /// reopen or by the end of capture, whichever writes them.
    tail_periods: usize,
    drain_real_frames: usize,
    drain_fade_frames: usize,
    drain_fade_lead_frames: usize,
    period_signal_end: usize,
    drain_frames_written: usize,
    drain_fade_prepared: bool,
    asrc_tail_pending: bool,
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
        layout: OutputLayout,
        capacity_frames: usize,
    ) -> Self {
        let channels = usize::from(layout.channels);
        let asrc = AsrcClock::new(period_frames, device_rate, channels);
        let events = EventWriter::new(event_producer, Arc::clone(&shared));
        let strategy_mode = ClockMode::from_u32(shared.clock_mode.load(Ordering::Acquire));
        let latency = LatencyWriter::new(&shared);
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
            strategy_mode,
            stream_frame: 0,
            pending_marker: None,
            marker_queue: VecDeque::with_capacity(TIMING_QUEUE_CAPACITY),
            layout,
            channels,
            period_frames,
            period_ring_level_frames: 0,
            period_trough_frames: 0,
            priming: true,
            capacity_frames,
            period_stream_start: 0,
            period_source_frames: 0,
            period_had_audio: false,
            mute_gain: 1.0,
            fade_in_remaining: boundary_fade_frames(device_rate),
            last_output: [0.0; MAX_OUTPUT_CHANNELS],
            have_last_output: false,
            latency,
            cadence: CadenceGuard::new(),
            tail_periods: 0,
            drain_real_frames: 0,
            drain_fade_frames: 0,
            drain_fade_lead_frames: 0,
            period_signal_end: 0,
            drain_frames_written: 0,
            drain_fade_prepared: false,
            asrc_tail_pending: false,
        }
    }

    fn fill(&mut self, buffer: &mut [f32]) {
        // Read once, before anything about the ring: every block capture
        // published before it finished is then visible to this period, and
        // every decision about how the period ends uses this reading. Capture
        // finishing after it is a source that was late for this period.
        let capture_done = self.shared.capture_done.load(Ordering::Acquire);
        buffer.fill(0.0);
        self.refresh_markers();
        self.target_ms = f64::from(f32::from_bits(
            self.shared.target_ms_bits.load(Ordering::Acquire),
        ));
        self.period_ring_level_frames = self.queued_frames();
        self.period_stream_start = self.stream_frame;
        self.period_source_frames = 0;
        self.discard_stale_markers();
        self.period_had_audio = false;

        let scene_rate = self.shared.scene_rate.load(Ordering::Acquire);
        let mode = ClockMode::from_u32(self.shared.clock_mode.load(Ordering::Acquire));
        if scene_rate == 0 {
            self.apply_mute(buffer);
            return;
        }
        if mode != self.strategy_mode || self.asrc.scene_rate != scene_rate {
            self.strategy_mode = mode;
            self.controller.reset();
            self.asrc.reset(scene_rate, self.device_rate);
            self.asrc_tail_pending = false;
            self.priming = true;
            self.fade_in_remaining = boundary_fade_frames(self.device_rate);
            self.latency.reset(&self.shared);
        }
        if self.priming {
            if self.shared.capture_input_ended.load(Ordering::Acquire) {
                // The source ended before the normal target was reached. Drain
                // every queued frame rather than treating the first sound as
                // disposable priming excess.
                self.priming = false;
            } else {
                // The direct clock copies the period's frames from the ring as is.
                let take = match mode {
                    ClockMode::Trim => buffer.len() / self.channels,
                    ClockMode::Asrc => self.asrc.frames_needed(),
                };
                let Some(excess) = self.priming_excess(scene_rate, take) else {
                    self.apply_mute(buffer);
                    return;
                };
                self.discard(excess);
                self.priming = false;
            }
        }
        let frames = buffer.len() / self.channels;
        let layout_frames = self.current_layout_frames();
        let marker_frames = self.queued_frames();
        let at_layout_boundary = layout_frames < marker_frames;
        self.asrc.input_limit = layout_frames;
        let seen = UnderrunSnapshot {
            ring_frames: (self.audio.slots() / self.channels) as u32,
            marker_frames: marker_frames as u32,
            layout_frames: layout_frames as u32,
            needed_frames: match mode {
                ClockMode::Trim => frames,
                ClockMode::Asrc => self.asrc.frames_needed(),
            } as u32,
            produced_frames: 0,
            since_block_ms: 0.0,
        };
        let produced = match mode {
            ClockMode::Trim => {
                let output_frames = frames.min(layout_frames);
                self.direct.render(
                    &mut self.audio,
                    &mut buffer[..output_frames * self.channels],
                    self.channels,
                    &mut self.stream_frame,
                )
            }
            ClockMode::Asrc => {
                self.asrc.ending = at_layout_boundary || capture_done;
                self.asrc.render(
                    &mut self.audio,
                    &mut buffer[..frames * self.channels],
                    scene_rate,
                    self.device_rate,
                    &mut self.stream_frame,
                )
            }
        };
        self.period_source_frames = produced.source_frames;
        self.period_had_audio = produced.output_frames != 0;
        if mode == ClockMode::Asrc && produced.source_frames != 0 {
            self.asrc_tail_pending = self.asrc.drain_periods() != 0;
        }
        // The stream ended here: nothing more is coming, so a short period is
        // its end, not a source falling behind.
        let stream_ended = capture_done && self.drained();
        let layout_ended = at_layout_boundary && produced.source_frames >= layout_frames;
        if (stream_ended || layout_ended)
            && mode == ClockMode::Asrc
            && produced.output_frames == 0
            && self.asrc_tail_pending
            && self.tail_periods == 0
        {
            // The take that emptied the stream or the old layout already went
            // out (the end was not visible yet), so the resampler's tail goes
            // in this period; a reopen writes whatever of it is left.
            self.plan_sent_period_tail();
            self.fill_tail_period(buffer);
            self.tail_periods = self.tail_periods.saturating_sub(1);
            return;
        }
        // Only a resampler that consumed input owes a tail.
        let drains_asrc = mode == ClockMode::Asrc
            && self.asrc_tail_pending
            && self.asrc.drain_periods() != 0
            && (layout_ended || stream_ended);
        if drains_asrc {
            self.prepare_asrc_drain_fade(Some((
                produced.output_frames,
                self.asrc.real_output_frames,
            )));
            self.tail_periods = self.asrc.drain_periods();
            self.asrc_tail_pending = false;
            self.apply_asrc_drain_lead_fade(buffer);
        }
        if produced.output_frames < frames {
            self.apply_fade_in(buffer, produced.output_frames);
            if !drains_asrc {
                if produced.output_frames == 0 {
                    self.apply_zero_frame_fade(buffer);
                } else {
                    self.apply_fade_out(buffer, produced.output_frames);
                }
            }
            if !layout_ended && !stream_ended {
                self.shared.underruns.fetch_add(1, Ordering::Relaxed);
                self.shared.publish_underrun(UnderrunSnapshot {
                    produced_frames: produced.output_frames as u32,
                    ..seen
                });
                self.priming = true;
                self.fade_in_remaining = boundary_fade_frames(self.device_rate);
            }
        } else if layout_ended {
            self.apply_fade_in(buffer, produced.output_frames);
            if !drains_asrc {
                self.apply_fade_out(buffer, produced.output_frames);
            }
        } else {
            self.apply_fade_in(buffer, produced.output_frames);
        }
        self.refresh_markers();
        self.period_trough_frames = self.queued_frames();
        self.apply_mute(buffer);
        if let Some(frame) = buffer.chunks_exact(self.channels).last() {
            self.last_output[..self.channels].copy_from_slice(frame);
            self.have_last_output = frame.iter().any(|sample| *sample != 0.0);
        }
    }

    fn prepare_asrc_drain_fade(&mut self, rendered: Option<(usize, usize)>) {
        if self.drain_fade_prepared {
            return;
        }
        let plan = plan_asrc_tail_fade(
            self.asrc.output_delay(),
            boundary_fade_frames(self.device_rate),
            rendered,
        );
        self.drain_real_frames = plan.drain_real_frames;
        self.drain_fade_frames = plan.drain_fade_frames;
        self.drain_fade_lead_frames = plan.lead_frames;
        self.period_signal_end = plan.period_signal_end;
        self.drain_frames_written = 0;
        self.drain_fade_prepared = true;
    }

    fn apply_asrc_drain_lead_fade(&self, buffer: &mut [f32]) {
        let total_fade_frames = self
            .drain_fade_lead_frames
            .saturating_add(self.drain_fade_frames);
        let fade_start = self
            .period_signal_end
            .saturating_sub(self.drain_fade_lead_frames);
        for (frame_index, frame) in buffer.chunks_exact_mut(self.channels).enumerate() {
            let gain = if frame_index >= self.period_signal_end {
                0.0
            } else if frame_index < fade_start {
                1.0
            } else if total_fade_frames <= 1 {
                0.0
            } else {
                let offset = frame_index - fade_start;
                (total_fade_frames - 1 - offset) as f32 / (total_fade_frames - 1) as f32
            };
            for sample in frame {
                *sample *= gain;
            }
        }
    }

    fn apply_asrc_drain_fade(&mut self, buffer: &mut [f32]) {
        let period_frames = buffer.len() / self.channels;
        for (offset, frame) in buffer.chunks_exact_mut(self.channels).enumerate() {
            let gain = asrc_drain_gain(
                self.drain_frames_written.saturating_add(offset),
                self.drain_real_frames,
                self.drain_fade_frames,
                self.drain_fade_lead_frames,
            );
            for sample in frame {
                *sample *= gain;
            }
        }
        self.drain_frames_written = self.drain_frames_written.saturating_add(period_frames);
    }

    /// `None` until the ring holds target + the next delivery + this period's
    /// take (capped one delivery below capacity, where the capture side would
    /// start dropping blocks instead); then the frames beyond target plus the
    /// delivery and the take. The next delivery is the larger of the head
    /// block and the largest recent capture step: a decoder hands over a
    /// whole sync frame (32 ms of E-AC-3) as several mixer blocks at once, and
    /// the ring must last until the next one. Only exact-zero frames may actually be discarded;
    /// audible excess remains as latency for the PI controller to reduce.
    /// The take is in scene frames and, for the ASRC, includes the filter's
    /// lookahead before its first render.
    fn priming_excess(&self, scene_rate: u32, take: usize) -> Option<usize> {
        let rate = f64::from(scene_rate);
        let block_frames = self.delivery_frames(scene_rate);
        let target_frames = self.target_ms * rate / 1_000.0;
        let take = take as f64;
        let threshold =
            (target_frames + block_frames + take).min(self.capacity_frames as f64 - block_frames);
        let level = self.period_ring_level_frames as f64;
        if level < threshold {
            return None;
        }
        let keep = (target_frames + block_frames + take).min(threshold);
        Some((level - keep).max(0.0) as usize)
    }

    /// The next delivery in scene frames: the head block, or the largest
    /// recent capture step if that is larger (a DD+ sync frame lands as
    /// several mixer blocks at once).
    fn delivery_frames(&self, scene_rate: u32) -> f64 {
        self.pending_marker
            .map_or(0.0, |marker| {
                let marker_rate = if marker.rate == 0 {
                    scene_rate
                } else {
                    marker.rate
                };
                f64::from(marker.frames) * f64::from(scene_rate) / f64::from(marker_rate)
            })
            .max(self.shared.arrival_frames.load(Ordering::Acquire) as f64)
    }

    /// The trough the controller holds, in ms (see [`trough_setpoint_frames`]).
    fn trough_setpoint_ms(&self, scene_rate: u32, take_frames: f64) -> f64 {
        let rate = f64::from(scene_rate);
        trough_setpoint_frames(
            self.target_ms * rate / 1_000.0,
            take_frames,
            self.delivery_frames(scene_rate),
            self.capacity_frames as f64,
        ) * 1_000.0
            / rate
    }

    /// Drops up to `frames` exact-zero frames from the head of the ring,
    /// keeping the stream position and latency markers in step. Inspection
    /// stops at the first audible frame.
    fn discard(&mut self, frames: usize) {
        let inspect = frames.min(self.queued_frames());
        if inspect == 0 {
            return;
        }
        let Ok(chunk) = self.audio.read_chunk(inspect * self.channels) else {
            return;
        };
        let (first, second) = chunk.as_slices();
        let silent_samples = first
            .iter()
            .chain(second)
            .take_while(|sample| **sample == 0.0)
            .count();
        let silent_frames = silent_samples / self.channels;
        chunk.commit(silent_frames * self.channels);
        self.stream_frame = self.stream_frame.saturating_add(silent_frames as u64);
        self.period_stream_start = self.stream_frame;
        self.discard_stale_markers();
    }

    fn apply_fade_in(&mut self, buffer: &mut [f32], audible_frames: usize) {
        apply_fade_in(
            buffer,
            self.channels,
            self.device_rate,
            audible_frames,
            &mut self.fade_in_remaining,
        );
    }

    /// The drain continues the resampler's output, so a fade-in still running
    /// (the take before was the first after priming, or an underrun silenced
    /// the period before) continues into it rather than jumping to full gain.
    fn apply_drain_fade_in(&mut self, buffer: &mut [f32]) {
        let frames = buffer.len() / self.channels;
        self.apply_fade_in(buffer, frames);
    }

    fn apply_zero_frame_fade(&self, buffer: &mut [f32]) {
        if !self.have_last_output {
            return;
        }
        let frames = boundary_fade_frames(self.device_rate).min(buffer.len() / self.channels);
        for (offset, frame) in buffer
            .chunks_exact_mut(self.channels)
            .take(frames)
            .enumerate()
        {
            let gain = if frames <= 1 {
                0.0
            } else {
                (frames - 1 - offset) as f32 / (frames - 1) as f32
            };
            for (channel, sample) in frame.iter_mut().enumerate() {
                *sample = self.last_output[channel] * gain;
            }
        }
    }

    fn apply_fade_out(&self, buffer: &mut [f32], audible_frames: usize) {
        let frames = boundary_fade_frames(self.device_rate).min(audible_frames);
        let start = audible_frames.saturating_sub(frames);
        for (offset, frame) in buffer
            .chunks_exact_mut(self.channels)
            .skip(start)
            .take(frames)
            .enumerate()
        {
            let gain = if frames <= 1 {
                0.0
            } else {
                (frames - 1 - offset) as f32 / (frames - 1) as f32
            };
            for sample in frame {
                *sample *= gain;
            }
        }
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
        for frame in buffer.chunks_exact_mut(self.channels) {
            if self.mute_gain < target {
                self.mute_gain = (self.mute_gain + step).min(target);
            } else if self.mute_gain > target {
                self.mute_gain = (self.mute_gain - step).max(target);
            }
            for sample in frame {
                *sample *= self.mute_gain;
            }
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
        let setpoint_ms =
            self.trough_setpoint_ms(scene_rate, period_seconds * f64::from(scene_rate));
        if let Some((trough_ms, ppm)) = self.controller.observe(
            to_ms(self.period_trough_frames),
            period_seconds,
            setpoint_ms,
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
                ClockMode::Asrc => {
                    self.shared
                        .trim_ppm_bits
                        .store(0.0_f64.to_bits(), Ordering::Release);
                    if !self.drain_fade_prepared {
                        self.asrc.set_ppm(ppm, scene_rate, self.device_rate);
                    }
                }
            }
        }
        self.consume_markers(playback_qpc, scene_rate);
    }

    fn refresh_markers(&mut self) {
        while let Ok(marker) = self.timing.pop() {
            debug_assert!(self.marker_queue.len() < TIMING_QUEUE_CAPACITY);
            self.marker_queue.push_back(marker);
        }
    }

    fn next_marker(&mut self) -> Option<TimingMarker> {
        self.marker_queue
            .pop_front()
            .or_else(|| self.timing.pop().ok())
    }

    /// Frames of `marker` the output has not taken yet. A period that takes
    /// more than one delivery leaves markers it already read in the queue
    /// until `consume_markers`; counting those whole put the trough up to a
    /// period above the ring (20 ms at a 20 ms period), so the controller held
    /// a ring that was nearly dry and the soaks underran.
    fn unconsumed_frames(&self, marker: TimingMarker) -> usize {
        marker
            .stream_frame
            .saturating_add(u64::from(marker.frames))
            .saturating_sub(marker.stream_frame.max(self.stream_frame)) as usize
    }

    fn queued_frames(&self) -> usize {
        if self.pending_marker.is_none() && self.marker_queue.is_empty() {
            return self.audio.slots() / self.channels;
        }
        self.pending_marker
            .iter()
            .chain(self.marker_queue.iter())
            .map(|marker| self.unconsumed_frames(*marker))
            .sum()
    }

    fn current_layout_frames(&self) -> usize {
        if self.pending_marker.is_none() && self.marker_queue.is_empty() {
            return self.audio.slots() / self.channels;
        }
        let pending = self.pending_marker.map_or(0, |marker| {
            if marker.layout == self.layout {
                self.unconsumed_frames(marker)
            } else {
                0
            }
        });
        pending.saturating_add(
            self.marker_queue
                .iter()
                .take_while(|marker| marker.layout == self.layout)
                .map(|marker| self.unconsumed_frames(*marker))
                .sum::<usize>(),
        )
    }

    fn layout_at_boundary(&mut self) -> Option<OutputLayout> {
        self.refresh_markers();
        self.discard_stale_markers();
        self.pending_marker
            .filter(|marker| marker.stream_frame <= self.stream_frame)
            .map(|marker| marker.layout)
    }

    fn pending_layout_change(&mut self) -> Option<OutputLayout> {
        self.layout_at_boundary()
            .filter(|layout| *layout != self.layout)
    }

    fn discard_pending_layout(&mut self, layout: OutputLayout) -> u64 {
        let frames = self.discard_buffered_audio(|marker| marker.layout == layout);
        if frames != 0 {
            self.shared
                .output_overflow_frames
                .fetch_add(frames, Ordering::Relaxed);
            self.events.push(RtEvent::OutputOverflow { frames });
        }
        frames
    }

    fn discard_buffered_audio(&mut self, mut accept: impl FnMut(TimingMarker) -> bool) -> u64 {
        // Freeze the audio side first. Markers are published before their samples,
        // so markers observed below can describe this entire sample budget, while
        // samples produced after this point cannot be discarded accidentally.
        let mut sample_budget = self.audio.slots();
        self.refresh_markers();
        if self.pending_marker.is_none() {
            self.pending_marker = self.marker_queue.pop_front();
        }
        let mut frames = 0_u64;
        while let Some(marker) = self.pending_marker {
            let marker_end = marker.stream_frame.saturating_add(u64::from(marker.frames));
            if marker_end <= self.stream_frame {
                self.pending_marker = self.marker_queue.pop_front();
                continue;
            }
            if marker.stream_frame > self.stream_frame || !accept(marker) {
                break;
            }
            let channels = usize::from(marker.layout.channels);
            if channels == 0 {
                break;
            }
            let available = marker_end.saturating_sub(self.stream_frame);
            let discard = available.min((sample_budget / channels) as u64) as usize;
            if discard == 0 {
                break;
            }
            let samples = discard.saturating_mul(channels);
            for _ in 0..samples {
                let _ = self.audio.pop();
            }
            sample_budget -= samples;
            self.stream_frame = self.stream_frame.saturating_add(discard as u64);
            frames = frames.saturating_add(discard as u64);
            if self.stream_frame != marker_end {
                break;
            }
            // Do not reach back into the producer ring here. That could consume
            // a marker produced after the frozen audio boundary above.
            self.pending_marker = self.marker_queue.pop_front();
        }
        frames
    }

    fn discard_crossing_audio(&mut self) {
        let frames = self.discard_buffered_audio(|_| true);
        self.asrc = AsrcClock::new(self.period_frames, self.device_rate, self.channels);
        let scene_rate = self.shared.scene_rate.load(Ordering::Acquire);
        self.asrc.reset(scene_rate, self.device_rate);
        self.asrc_tail_pending = false;
        self.tail_periods = 0;
        self.shared
            .output_overflow_frames
            .fetch_add(frames, Ordering::Relaxed);
        self.priming = true;
        self.fade_in_remaining = boundary_fade_frames(self.device_rate);
        self.latency.reset(&self.shared);
        self.cadence.reset();
    }

    fn discard_stale_markers(&mut self) {
        if self.pending_marker.is_none() {
            self.pending_marker = self.next_marker();
        }
        while self.pending_marker.is_some_and(|marker| {
            marker.stream_frame.saturating_add(u64::from(marker.frames)) <= self.stream_frame
        }) {
            self.pending_marker = self.next_marker();
        }
    }

    fn consume_markers(&mut self, playback_qpc: u64, scene_rate: u32) {
        if self.pending_marker.is_none() {
            self.pending_marker = self.next_marker();
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
                self.pending_marker = self.next_marker();
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
            };
            let frame_qpc = playback_qpc.saturating_add(
                (device_offset * self.qpc_frequency as f64 / f64::from(self.device_rate)).round()
                    as u64,
            );
            if marker.source_qpc != 0 && frame_qpc >= marker.source_qpc {
                let latency_ms =
                    (frame_qpc - marker.source_qpc) as f64 * 1_000.0 / self.qpc_frequency as f64;
                self.latency.push(latency_ms as f32, &self.shared);
            }
            let marker_end = marker.stream_frame.saturating_add(u64::from(marker.frames));
            if marker_end <= period_end {
                self.pending_marker = self.next_marker();
            } else {
                break;
            }
        }
    }

    fn reconfigure(
        &mut self,
        mode: ClockMode,
        layout: OutputLayout,
        device_rate: u32,
        period_frames: usize,
    ) {
        self.strategy_mode = mode;
        self.layout = layout;
        self.channels = usize::from(layout.channels);
        self.device_rate = device_rate;
        self.period_frames = period_frames;
        self.controller.reset();
        self.direct = DirectClock;
        self.asrc = AsrcClock::new(period_frames, device_rate, self.channels);
        let scene_rate = self.shared.scene_rate.load(Ordering::Acquire);
        self.asrc.reset(scene_rate, device_rate);
        self.priming = true;
        self.period_source_frames = 0;
        self.period_had_audio = false;
        self.mute_gain = 1.0;
        self.fade_in_remaining = boundary_fade_frames(device_rate);
        self.last_output.fill(0.0);
        self.have_last_output = false;
        self.latency.reset(&self.shared);
        self.cadence.reset();
        self.drain_fade_prepared = false;
        self.tail_periods = 0;
        self.asrc_tail_pending = false;
    }

    fn drained(&self) -> bool {
        self.audio.slots() == 0 && self.asrc.buffered_frames() == 0
    }

    /// Capture has ended and every frame it produced has been rendered.
    fn ended(&self) -> bool {
        self.shared.capture_started.load(Ordering::Acquire)
            && self.shared.capture_done.load(Ordering::Acquire)
            && self.drained()
    }

    /// Plans the tail of a resampler whose last rendered period already went
    /// to the sink: no fade was planned in that period, so all of it lies in
    /// the tail, and the ratio stays as it is until the tail is out.
    fn plan_sent_period_tail(&mut self) {
        self.asrc.freeze_ratio();
        self.prepare_asrc_drain_fade(None);
        self.tail_periods = self.asrc.drain_periods();
        self.asrc_tail_pending = false;
    }

    /// One period of the resampler's tail: what it still holds, faded as
    /// planned, then silence.
    fn fill_tail_period(&mut self, buffer: &mut [f32]) {
        buffer.fill(0.0);
        self.asrc.flush(buffer);
        self.apply_drain_fade_in(buffer);
        self.apply_asrc_drain_fade(buffer);
        self.apply_mute(buffer);
    }

    /// Writes what the resampler still holds once capture has ended, then
    /// nothing more. A reopen drain the end interrupted continues where it
    /// was rather than starting over.
    fn write_terminal_tail<S: OutputSink>(&mut self, sink: &mut S) -> Result<(), OutputError> {
        if self.strategy_mode != ClockMode::Asrc {
            return Ok(());
        }
        if self.asrc_tail_pending && self.tail_periods == 0 {
            self.plan_sent_period_tail();
        }
        for _ in 0..self.tail_periods {
            sink.next_period(&mut |buffer| self.fill_tail_period(buffer))?;
            self.tail_periods -= 1;
        }
        Ok(())
    }

    fn begin_reopen_tail(&mut self) -> bool {
        self.asrc.freeze_ratio();
        // Only input the resampler consumed and has not drained yet owes a
        // tail; one already written in full is not planned again.
        if self.strategy_mode == ClockMode::Asrc && self.tail_periods == 0 && self.asrc_tail_pending
        {
            self.plan_sent_period_tail();
        }
        self.tail_periods != 0
    }

    fn write_reopen_tail_period<S: OutputSink>(
        &mut self,
        sink: &mut S,
    ) -> Result<bool, OutputError> {
        debug_assert!(self.tail_periods != 0);
        sink.next_period(&mut |buffer| self.fill_tail_period(buffer))?;
        self.tail_periods = self.tail_periods.saturating_sub(1);
        Ok(self.tail_periods == 0)
    }
}

#[derive(Clone, Copy)]
struct Rendered {
    source_frames: usize,
    output_frames: usize,
}

struct DirectClock;

impl DirectClock {
    fn render(
        &mut self,
        audio: &mut Consumer<f32>,
        output: &mut [f32],
        channels: usize,
        stream_frame: &mut u64,
    ) -> Rendered {
        let available_samples = audio.slots().min(output.len());
        let samples = available_samples - available_samples % channels;
        if samples != 0 {
            let (popped, _) = audio.pop_partial_slice(&mut output[..samples]);
            debug_assert_eq!(popped.len(), samples);
        }
        let frames = samples / channels;
        *stream_frame = stream_frame.saturating_add(frames as u64);
        Rendered {
            source_frames: frames,
            output_frames: frames,
        }
    }
}

struct AsrcClock {
    resampler: Option<Async<f32>>,
    input: InterleavedOwned<f32>,
    nominal_ratio: f64,
    scene_rate: u32,
    device_rate: u32,
    period_output_frames: usize,
    channels: usize,
    input_limit: usize,
    real_output_frames: usize,
    /// The source has ended: a last take shorter than the resampler asks for
    /// is completed with silence instead of waiting for frames that will not
    /// come.
    ending: bool,
    ratio_frozen: bool,
}

impl AsrcClock {
    fn new(period_frames: usize, device_rate: u32, channels: usize) -> Self {
        // A 256-tap Blackman-Harris sinc with cubic interpolation is rubato's
        // documented high-quality starting point. Stereo uses the combined-sinc
        // optimization, making cubic suitable for a 48 kHz real-time stream.
        let parameters = SincInterpolationParameters::new(256, WindowFunction::BlackmanHarris2);
        let ratio = f64::from(device_rate) / f64::from(crate::MIX_RATE);
        let resampler = Async::new_sinc(
            ratio,
            ASRC_MAX_RELATIVE_RATIO,
            &parameters,
            period_frames,
            channels,
            FixedAsync::Output,
        )
        .ok();
        Self {
            resampler,
            input: InterleavedOwned::new(0.0, channels, MAX_ASRC_INPUT_FRAMES),
            nominal_ratio: ratio,
            scene_rate: 0,
            device_rate,
            period_output_frames: period_frames,
            channels,
            input_limit: usize::MAX,
            real_output_frames: 0,
            ending: false,
            ratio_frozen: false,
        }
    }

    fn reset(&mut self, scene_rate: u32, device_rate: u32) {
        self.ratio_frozen = false;
        debug_assert!(scene_rate == 0 || scene_rate == crate::MIX_RATE);
        if scene_rate == 0 || device_rate != self.device_rate {
            self.scene_rate = 0;
            return;
        }
        if let Some(resampler) = &mut self.resampler {
            resampler.reset();
        }
        self.nominal_ratio = f64::from(device_rate) / f64::from(crate::MIX_RATE);
        self.scene_rate = crate::MIX_RATE;
    }

    fn set_ppm(&mut self, ppm: f64, scene_rate: u32, device_rate: u32) {
        if self.ratio_frozen {
            return;
        }
        if self.scene_rate != scene_rate || self.resampler.is_none() {
            self.reset(scene_rate, device_rate);
        }
        if let Some(resampler) = &mut self.resampler {
            let ratio = self.nominal_ratio * (1.0 - ppm * 1.0e-6);
            let _ = resampler.set_resample_ratio(ratio, true);
        }
    }

    fn freeze_ratio(&mut self) {
        let Some(resampler) = &mut self.resampler else {
            self.ratio_frozen = true;
            return;
        };
        let ratio = resampler.resample_ratio();
        let _ = resampler.set_resample_ratio(ratio, false);
        self.ratio_frozen = true;
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

    fn output_delay(&self) -> usize {
        self.resampler
            .as_ref()
            .map_or(0, rubato::Resampler::output_delay)
    }

    fn drain_periods(&self) -> usize {
        self.output_delay().div_ceil(self.period_output_frames)
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
            for channel in 0..self.channels {
                let _ = self.input.write_sample(channel, frame, &0.0);
            }
        }
        let output_frames_capacity = output.len() / self.channels;
        let Ok(mut output_adapter) =
            InterleavedSlice::new_mut(output, self.channels, output_frames_capacity)
        else {
            return;
        };
        let _ = resampler.process_into_buffer(&self.input, &mut output_adapter, None);
    }
}

impl AsrcClock {
    fn render(
        &mut self,
        audio: &mut Consumer<f32>,
        output: &mut [f32],
        scene_rate: u32,
        device_rate: u32,
        stream_frame: &mut u64,
    ) -> Rendered {
        self.real_output_frames = 0;
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
        let available = (audio.slots() / self.channels).min(self.input_limit);
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
            for channel in 0..self.channels {
                let sample = if frame < real {
                    audio.pop().expect("availability was checked")
                } else {
                    0.0
                };
                let _ = self.input.write_sample(channel, frame, &sample);
            }
        }
        let output_frames_capacity = output.len() / self.channels;
        let Ok(mut output_adapter) =
            InterleavedSlice::new_mut(output, self.channels, output_frames_capacity)
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
        self.real_output_frames = if real == input_frames {
            output_frames
        } else {
            output_frames
                .saturating_mul(real)
                .saturating_add(input_frames / 2)
                / input_frames
        };
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
        let proportional = PI_KP * error_ms;
        // The integral is the clock skew. An error beyond the band is latency
        // to drain (priming keeps audible excess, ADR 0028), and integrating
        // it, or integrating further into a saturated output, winds the
        // integral up so it keeps draining past the target until the output
        // runs dry.
        let unclamped = proportional + PI_KI * self.integral;
        let saturated_this_way =
            unclamped.abs() >= PI_LIMIT_PPM && unclamped.signum() == error_ms.signum();
        if error_ms.abs() <= PI_INTEGRAL_BAND_MS && !saturated_this_way {
            self.integral = (self.integral + error_ms * seconds)
                .clamp(-PI_LIMIT_PPM / PI_KI, PI_LIMIT_PPM / PI_KI);
        }
        (proportional + PI_KI * self.integral).clamp(-PI_LIMIT_PPM, PI_LIMIT_PPM)
    }
}

/// The largest number of frames one capture step put into the crossing ring,
/// over the last [`ARRIVAL_WINDOW`] steps that put any. The mixer cuts its
/// output into `MIX_FRAMES` blocks, but a decoder delivers a whole sync frame
/// in one step, so the ring waits that long, not one block, for the next.
struct ArrivalPeak {
    recent: [u64; ARRIVAL_WINDOW],
    next: usize,
}

impl ArrivalPeak {
    fn new() -> Self {
        Self {
            recent: [0; ARRIVAL_WINDOW],
            next: 0,
        }
    }

    /// Records one step's delivery and returns the window's largest.
    fn record(&mut self, frames: u64) -> u64 {
        self.recent[self.next] = frames;
        self.next = (self.next + 1) % ARRIVAL_WINDOW;
        self.recent.iter().copied().max().unwrap_or(frames)
    }
}

/// Where the controller holds the trough, in frames: the target beyond what
/// one delivery landing just after a take instead of just before it costs
/// that take, which is the smaller of the take and the delivery. The trough
/// alone cannot show that cost while every period gets its delivery: with a
/// period at least as long as a delivery (10 ms shared on 10 ms PCM, 20 ms
/// periods) it is flat until the arrival phase crosses the period boundary,
/// so a controller holding it at the target kept the ring one late delivery
/// from empty, and the period that missed its delivery underran. Priming
/// fills to target + delivery + take, so the trough starts here. Capped where
/// the ring still takes a delivery after the take.
fn trough_setpoint_frames(target: f64, take: f64, delivery: f64, capacity: f64) -> f64 {
    (target + take.min(delivery)).min((capacity - delivery - take).max(0.0))
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
        };
        let ppm = self.pi.update(error_ms, self.seconds);
        self.trough_ms = f64::INFINITY;
        self.seconds = 0.0;
        Some((trough_ms, ppm))
    }
}

struct CadenceGuard {
    start_qpc: Option<u64>,
    start_position: Option<u64>,
    bad_seconds: f64,
    latched: bool,
    newly_faulted: bool,
}

impl CadenceGuard {
    fn new() -> Self {
        Self {
            start_qpc: None,
            start_position: None,
            bad_seconds: 0.0,
            latched: false,
            newly_faulted: false,
        }
    }

    fn reset(&mut self) {
        *self = Self::new();
    }

    fn observe(
        &mut self,
        qpc: u64,
        position: Option<u64>,
        qpc_frequency: u64,
        rate: u32,
    ) -> Option<f32> {
        self.newly_faulted = false;
        let position = position?;
        let Some(start_qpc) = self.start_qpc else {
            self.start_qpc = Some(qpc);
            self.start_position = Some(position);
            return None;
        };
        let seconds = qpc.saturating_sub(start_qpc) as f64 / qpc_frequency as f64;
        if seconds < CADENCE_WINDOW_SECONDS {
            return None;
        }
        let frames = position.saturating_sub(self.start_position.unwrap_or(position));
        let ratio = frames as f64 / (seconds * f64::from(rate));
        self.start_qpc = Some(qpc);
        self.start_position = Some(position);
        if !(0.9..=1.1).contains(&ratio) {
            self.bad_seconds += seconds;
            if self.bad_seconds >= CADENCE_FAULT_SECONDS && !self.latched {
                self.latched = true;
                self.newly_faulted = true;
            }
        } else {
            self.bad_seconds = 0.0;
            self.latched = false;
        }
        Some(ratio as f32)
    }

    fn faulted(&self) -> bool {
        self.newly_faulted
    }
}

struct LatencyWriter {
    generation: u64,
    reset_request: u64,
    next: u64,
}

impl LatencyWriter {
    fn new(shared: &Shared) -> Self {
        Self {
            generation: shared.latency_generation.load(Ordering::Acquire),
            reset_request: shared.latency_reset_requested.load(Ordering::Acquire),
            next: 0,
        }
    }

    fn reset(&mut self, shared: &Shared) {
        self.generation = shared.reset_latency();
        self.reset_request = shared.latency_reset_requested.load(Ordering::Acquire);
        self.next = 0;
    }

    fn apply_requested_reset(&mut self, shared: &Shared) {
        let request = shared.latency_reset_requested.load(Ordering::Acquire);
        if request != self.reset_request {
            self.reset(shared);
            self.reset_request = request;
        }
    }

    fn push(&mut self, value: f32, shared: &Shared) {
        if !value.is_finite() {
            return;
        }
        let generation = shared.latency_generation.load(Ordering::Acquire);
        if generation != self.generation {
            self.generation = generation;
            self.next = 0;
        }
        let sequence = self.next;
        let index = sequence as usize % LATENCY_SAMPLES;
        shared.latency_sequences[index].store(0, Ordering::Relaxed);
        std::sync::atomic::fence(Ordering::Release);
        shared.latency_values[index].store(value.to_bits(), Ordering::Relaxed);
        shared.latency_sequences[index].store(sequence.wrapping_add(1), Ordering::Release);
        self.next = sequence.wrapping_add(1);
        shared.latency_next.store(self.next, Ordering::Release);
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

fn valid_target_ms(target_ms: f32, shared: &Shared) -> bool {
    target_ms.is_finite()
        && target_ms >= 0.0
        && target_ms < f32::from_bits(shared.max_target_ms_bits.load(Ordering::Acquire))
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
        BitstreamCodec::Eac3Joc => (2, 0),
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
        2 => BitstreamCodec::Eac3Joc,
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
    use rtrb::RingBuffer;

    use super::*;

    fn output_format(rate: u32) -> OutputFormat {
        OutputFormat {
            rate,
            channels: 2,
            channel_mask: 0x3,
            period_frames: 480,
            mode: OutputMode::Exclusive,
            fell_back: false,
            mix_channels: 2,
        }
    }

    #[test]
    fn the_asrc_drain_fade_ends_on_the_last_real_frame() {
        // 44.1 kHz output: 117 real frames in the drain, an 88-frame fade.
        let (real, fade) = (117, 88);
        let gains: Vec<f32> = (0..160)
            .map(|frame| asrc_drain_gain(frame, real, fade, 0))
            .collect();
        assert!(
            gains[..real - fade].iter().all(|gain| *gain == 1.0),
            "{gains:?}"
        );
        assert!(
            gains[real - fade..real]
                .windows(2)
                .all(|pair| pair[1] < pair[0])
        );
        assert_eq!(gains[real - 1], 0.0);
        assert!(gains[real..].iter().all(|gain| *gain == 0.0));
        // A delay shorter than the fade: the drain continues the ramp the last
        // rendered period began (lead 40 of 88 frames).
        let (real, fade, lead) = (48, 48, 40);
        assert_eq!(asrc_drain_gain(0, real, fade, lead), 47.0 / 87.0);
        assert_eq!(asrc_drain_gain(real - 1, real, fade, lead), 0.0);
    }

    #[test]
    fn the_asrc_tail_fade_plan_places_the_ramp_at_the_real_signal_end() {
        assert_eq!(
            plan_asrc_tail_fade(117, 88, None),
            TailFade {
                lead_frames: 0,
                period_signal_end: 0,
                drain_real_frames: 117,
                drain_fade_frames: 88,
            }
        );
        assert_eq!(
            plan_asrc_tail_fade(59, 88, None),
            TailFade {
                lead_frames: 0,
                period_signal_end: 0,
                drain_real_frames: 59,
                drain_fade_frames: 59,
            }
        );
        assert_eq!(
            plan_asrc_tail_fade(117, 88, Some((441, 400))),
            TailFade {
                lead_frames: 12,
                period_signal_end: 441,
                drain_real_frames: 76,
                drain_fade_frames: 76,
            }
        );
        assert_eq!(
            plan_asrc_tail_fade(117, 88, Some((441, 200))),
            TailFade {
                lead_frames: 88,
                period_signal_end: 317,
                drain_real_frames: 0,
                drain_fade_frames: 0,
            }
        );
    }

    #[test]
    fn a_planned_asrc_tail_freezes_the_pending_ratio_through_drain() {
        let scene_rate = 48_000;
        let device_rate = 44_100;
        let period_frames = 441;
        let source_frames = 960;
        let (mut audio_in, mut audio) = RingBuffer::<f32>::new(source_frames * 2);
        for frame in 0..source_frames {
            let sample =
                (2.0 * std::f32::consts::PI * 997.0 * frame as f32 / scene_rate as f32).sin();
            audio_in.push(sample).unwrap();
            audio_in.push(sample).unwrap();
        }
        let mut asrc = AsrcClock::new(period_frames, device_rate, 2);
        asrc.reset(scene_rate, device_rate);
        let mut rendered = vec![0.0; period_frames * 2];
        let mut stream_frame = 0;
        let result = asrc.render(
            &mut audio,
            &mut rendered,
            scene_rate,
            device_rate,
            &mut stream_frame,
        );
        assert_eq!(result.output_frames, period_frames);
        let planned_end = period_frames + asrc.output_delay();

        asrc.freeze_ratio();
        asrc.set_ppm(400_000.0, scene_rate, device_rate);
        let frozen_end = period_frames + asrc.output_delay();
        let mut drain = vec![0.0; period_frames * 2];
        asrc.flush(&mut drain);
        let planned_delay = planned_end - period_frames;
        for (frame_index, frame) in drain.as_chunks_mut::<2>().0.iter_mut().enumerate() {
            let gain = asrc_drain_gain(
                frame_index,
                planned_delay,
                boundary_fade_frames(device_rate),
                0,
            );
            frame[0] *= gain;
            frame[1] *= gain;
        }
        let last_nonzero = drain
            .as_chunks::<2>()
            .0
            .iter()
            .rposition(|frame| frame[0] != 0.0 || frame[1] != 0.0)
            .map(|frame| period_frames + frame + 1)
            .expect("the frozen ASRC emitted no delayed signal");

        assert_eq!(frozen_end, planned_end);
        assert!(
            last_nonzero.abs_diff(planned_end) <= 1,
            "drained end {last_nonzero} moved away from frozen plan {planned_end}"
        );
        assert!(
            drain[planned_delay * 2..]
                .iter()
                .all(|sample| *sample == 0.0),
            "frozen drain continued past planned end {planned_end}"
        );
    }

    #[test]
    fn leaving_trim_clears_a_sub_deadband_applied_request_exactly() {
        struct TrimCapture(Vec<f64>);

        impl CaptureSource for TrimCapture {
            fn read(&mut self, _buffer: &mut [u8], _wait: Duration) -> Result<usize, CaptureError> {
                Ok(0)
            }

            fn set_rate_trim(&mut self, ppm: f64) -> Result<(), ssr_types::TrimUnsupported> {
                self.0.push(ppm);
                Ok(())
            }
        }

        let shared = Shared::new();
        shared
            .clock_mode
            .store(ClockMode::Trim.as_u32(), Ordering::Release);
        shared
            .trim_ppm_bits
            .store(0.06_f64.to_bits(), Ordering::Release);
        let mut capture = TrimCapture(Vec::new());
        let mut applied = f64::NAN;
        apply_requested_trim(&mut capture, true, &shared, &mut applied);
        shared
            .clock_mode
            .store(ClockMode::Asrc.as_u32(), Ordering::Release);
        apply_requested_trim(&mut capture, true, &shared, &mut applied);

        assert_eq!(capture.0, [0.06, 0.0]);
        assert_eq!(applied.to_bits(), 0.0_f64.to_bits());
    }

    #[test]
    fn output_clock_selection_rechecks_every_opened_format() {
        assert_eq!(
            select_output_clock(
                ClockPreference::Auto,
                true,
                output_format(crate::MIX_RATE),
                OutputLayout::STEREO,
            ),
            Ok(ClockMode::Trim)
        );
        assert_eq!(
            select_output_clock(
                ClockPreference::Auto,
                true,
                output_format(44_100),
                OutputLayout::STEREO,
            ),
            Ok(ClockMode::Asrc)
        );
        assert_eq!(
            select_output_clock(
                ClockPreference::Auto,
                true,
                output_format(crate::MIX_RATE),
                OutputLayout::STEREO,
            ),
            Ok(ClockMode::Trim)
        );
        assert!(
            select_output_clock(
                ClockPreference::Trim,
                true,
                output_format(44_100),
                OutputLayout::STEREO,
            )
            .is_err()
        );
        assert!(
            select_output_clock(
                ClockPreference::Asrc,
                false,
                output_format(MIN_DEVICE_RATE - 1),
                OutputLayout::STEREO,
            )
            .is_err()
        );
        assert_eq!(
            select_output_clock(
                ClockPreference::Asrc,
                false,
                output_format(MIN_DEVICE_RATE),
                OutputLayout::STEREO,
            ),
            Ok(ClockMode::Asrc)
        );
    }

    #[test]
    fn only_errors_without_an_endpoint_use_unattributed_counters() {
        let shared = Shared::new();
        shared.note_chain_error(None, ChainErrorKind::TruncatedRecord);
        shared.note_chain_error(Some(1), ChainErrorKind::CorruptPayload);
        shared.note_chain_error(
            Some(ENGINE_ENDPOINTS as u16),
            ChainErrorKind::BadRecordLength,
        );

        assert_eq!(
            shared
                .read_unattributed_chain_errors()
                .get(ChainErrorKind::TruncatedRecord),
            1
        );
        assert_eq!(shared.read_unattributed_chain_errors().total(), 1);
        assert_eq!(
            shared.endpoints[1]
                .read_chain_errors()
                .get(ChainErrorKind::CorruptPayload),
            1
        );
        assert_eq!(
            shared
                .endpoints
                .iter()
                .map(|endpoint| endpoint.read_chain_errors().total())
                .sum::<u64>(),
            1
        );
    }

    #[test]
    fn cadence_guard_accepts_normal_rejects_sustained_slow_and_ignores_a_blip() {
        fn feed(ratios: &[f64]) -> (Vec<f32>, usize) {
            let mut guard = CadenceGuard::new();
            let mut qpc = 0_u64;
            let mut position = 0_u64;
            let mut observed = Vec::new();
            let mut faults = 0;
            let _ = guard.observe(qpc, Some(position), 1_000, 1_000);
            for ratio in ratios {
                qpc += 2_000;
                position += (2_000.0 * ratio).round() as u64;
                if let Some(value) = guard.observe(qpc, Some(position), 1_000, 1_000) {
                    observed.push(value);
                    faults += usize::from(guard.faulted());
                }
            }
            (observed, faults)
        }

        assert_eq!(feed(&[1.0, 1.0, 1.0]).1, 0);
        let (slow, faults) = feed(&[0.65, 0.65, 0.65]);
        assert_eq!(slow, [0.65, 0.65, 0.65]);
        assert_eq!(faults, 1);
        assert_eq!(feed(&[0.65, 1.0, 1.0]).1, 0);
    }

    struct ClockRun {
        underruns: u32,
        clamped: u32,
        /// Windows at the ppm limit over the second half.
        late_clamped: u32,
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
    /// keeps every audible frame beyond it (ADR 0028: only exact silence is
    /// discarded), and the controller reads the level left after each period.
    fn simulate_clock(
        skew_ppm: f64,
        block: usize,
        period: usize,
        trim: bool,
        seconds: f64,
    ) -> ClockRun {
        simulate_clock_from(skew_ppm, block, period, trim, seconds, 0.0)
    }

    /// As [`simulate_clock`], with `backlog_ms` of audible source already
    /// queued when the output starts (a dump or driver that delivers a burst).
    fn simulate_clock_from(
        skew_ppm: f64,
        block: usize,
        period: usize,
        trim: bool,
        seconds: f64,
        backlog_ms: f64,
    ) -> ClockRun {
        simulate_clock_with(skew_ppm, block, period, trim, seconds, backlog_ms, 0.0)
    }

    /// The trough the simulation's controller holds: the runtime's setpoint
    /// for a 10 ms target, in ms.
    fn simulated_setpoint_ms(block: usize, period: usize) -> f64 {
        trough_setpoint_frames(480.0, period as f64, block as f64, f64::INFINITY) / 48.0
    }

    /// As [`simulate_clock_from`], with a share `late` of the blocks landing
    /// one period late (a capture step that misses the take it was due
    /// before), drawn from a fixed seed.
    fn simulate_clock_with(
        skew_ppm: f64,
        block: usize,
        period: usize,
        trim: bool,
        seconds: f64,
        backlog_ms: f64,
        late: f64,
    ) -> ClockRun {
        let rate = 48_000.0;
        let target = 10.0;
        let to_ms = |frames: f64| frames * 1_000.0 / rate;
        let setpoint = simulated_setpoint_ms(block, period);
        let mut seed = 0x9E37_79B9_7F4A_7C15_u64;
        let mut draw = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed >> 11) as f64 / (1_u64 << 53) as f64
        };
        let mut held = 0.0_f64;
        let mode = if trim {
            ClockMode::Trim
        } else {
            ClockMode::Asrc
        };
        let period_seconds = period as f64 / rate;
        let periods = (seconds / period_seconds) as usize;
        let mut controller = TroughController::new();
        let (mut arriving, mut correction) = (0.0_f64, 0.0_f64);
        let mut level = (backlog_ms * rate / 1_000.0).round();
        let mut priming = true;
        let mut run = ClockRun {
            underruns: 0,
            clamped: 0,
            late_clamped: 0,
            trough: (f64::INFINITY, f64::NEG_INFINITY),
            ppm: (f64::INFINITY, f64::NEG_INFINITY),
            final_ppm: 0.0,
        };
        for index in 0..periods {
            let source_ppm = skew_ppm + if trim { correction } else { 0.0 };
            arriving += period as f64 * (1.0 + source_ppm * 1.0e-6);
            level += held;
            held = 0.0;
            while arriving >= block as f64 {
                arriving -= block as f64;
                if late > 0.0 && draw() < late {
                    held += block as f64;
                } else {
                    level += block as f64;
                }
            }
            if priming {
                if to_ms(level) < target + to_ms(block as f64) + to_ms(period as f64) {
                    continue;
                }
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
                controller.observe(to_ms(level), period_seconds, setpoint, mode)
            else {
                continue;
            };
            correction = ppm;
            if ppm.abs() >= PI_LIMIT_PPM {
                run.clamped += 1;
                if index * 2 >= periods {
                    run.late_clamped += 1;
                }
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
    fn target_change_is_observed_at_the_next_output_period() {
        let shared = Arc::new(Shared::new());
        shared.scene_rate.store(48_000, Ordering::Release);
        shared
            .target_ms_bits
            .store(10.0_f32.to_bits(), Ordering::Release);
        let (_audio_in, audio) = RingBuffer::<f32>::new(2);
        let (_timing_in, timing) = RingBuffer::<TimingMarker>::new(TIMING_QUEUE_CAPACITY);
        let (events, _events_out) = RingBuffer::<RtEvent>::new(EVENT_QUEUE_CAPACITY);
        let mut output = OutputState::new(
            audio,
            timing,
            48_000,
            48,
            10.0,
            DEFAULT_QPC_FREQUENCY,
            events,
            Arc::clone(&shared),
            OutputLayout::STEREO,
            80 * 48,
        );
        let mut period = vec![0.0; 48 * 2];
        output.fill(&mut period);
        assert_eq!(output.target_ms, 10.0);
        shared
            .target_ms_bits
            .store(25.0_f32.to_bits(), Ordering::Release);
        assert_eq!(output.target_ms, 10.0);
        output.fill(&mut period);
        assert_eq!(output.target_ms, 25.0);
    }

    #[test]
    fn target_change_rejects_values_outside_crossing_capacity() {
        let (changes, _consumer) = RingBuffer::new(8);
        let (_capture_events_in, capture_events) = RingBuffer::new(1);
        let (_output_events_in, output_events) = RingBuffer::new(1);
        let (_retired_in, retired) = RingBuffer::new(1);
        let shared = Arc::new(Shared::new());
        shared
            .max_target_ms_bits
            .store(50.0_f32.to_bits(), Ordering::Release);
        let engine = Engine {
            shared,
            changes: RefCell::new(changes),
            capture_events,
            output_events,
            retired,
            capture_thread: None,
            output_thread: None,
        };

        assert!(engine.apply(LiveChange::TargetMs(0.0)).is_ok());
        assert!(engine.apply(LiveChange::TargetMs(49.999)).is_ok());
        for invalid in [-0.1, 50.0, f32::NEG_INFINITY, f32::INFINITY, f32::NAN] {
            assert_eq!(engine.apply(LiveChange::TargetMs(invalid)), Err(Rejected));
        }
    }

    #[test]
    fn lfe_gain_change_rejects_values_outside_the_supported_range() {
        let (changes, _consumer) = RingBuffer::new(4);
        let (_capture_events_in, capture_events) = RingBuffer::new(1);
        let (_output_events_in, output_events) = RingBuffer::new(1);
        let (_retired_in, retired) = RingBuffer::new(1);
        let engine = Engine {
            shared: Arc::new(Shared::new()),
            changes: RefCell::new(changes),
            capture_events,
            output_events,
            retired,
            capture_thread: None,
            output_thread: None,
        };

        assert!(engine.apply(LiveChange::LfeGain(-40.0)).is_ok());
        assert!(engine.apply(LiveChange::LfeGain(20.0)).is_ok());
        for invalid in [-40.1, 20.1, f32::NEG_INFINITY, f32::INFINITY, f32::NAN] {
            assert_eq!(engine.apply(LiveChange::LfeGain(invalid)), Err(Rejected));
        }
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
                    let setpoint = simulated_setpoint_ms(block, period);
                    assert_eq!(run.underruns, 0, "{label}");
                    assert_eq!(run.clamped, 0, "{label}");
                    assert!(
                        run.trough.0 >= setpoint - 2.0 && run.trough.1 <= setpoint + 2.0,
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
    fn audible_priming_excess_drains_without_winding_up_the_clock() {
        // Priming keeps every audible frame (ADR 0028), so a source that
        // starts with a backlog leaves the trough tens of milliseconds above
        // target. The controller drains that latency at up to its limit; if
        // the integral learned it as clock skew, it kept draining past the
        // target, the output ran dry, primed again with the excess, and the
        // cycle repeated every ~20 s (the CI soaks' underruns).
        // Periods shorter than a block, at any skew the simulation covers; and
        // every period at no skew (the CI soaks: a QPC-paced dump into a
        // QPC-clocked virtual cable). Longer periods under skew starve now
        // and then for another reason (clock_with_a_long_period_stays_bounded).
        let short =
            [(480, 144), (1_536, 144), (1_536, 480)].map(|case| (case, [-300.0, 0.0, 300.0]));
        let long = [(480, 480), (480, 960), (1_536, 960)].map(|case| (case, [0.0; 3]));
        for ((block, period), skews) in short.into_iter().chain(long) {
            for skew in skews {
                for backlog in [30.0, 60.0] {
                    for trim in [false, true] {
                        let run = simulate_clock_from(skew, block, period, trim, 300.0, backlog);
                        let label = format!(
                            "block {block} period {period} skew {skew} backlog {backlog} trim {trim}"
                        );
                        let setpoint = simulated_setpoint_ms(block, period);
                        assert_eq!(run.underruns, 0, "{label}");
                        assert_eq!(run.late_clamped, 0, "{label}: still at the limit");
                        assert!(
                            run.trough.0 >= setpoint - 3.0 && run.trough.1 <= setpoint + 3.0,
                            "{label}: trough {:?} ms",
                            run.trough
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn a_period_as_long_as_a_block_survives_phase_slips_and_late_blocks() {
        // A 10 ms shared period with 10 ms PCM blocks, and a 20 ms exclusive
        // period with PCM or E-AC-3. While every period gets its block the
        // trough is flat; as the arrival phase drifts past the period boundary
        // one period gets none, and so does a period whose block lands late.
        // Held at the bare target the ring was one block from empty there and
        // starved about once per phase slip (33 s at 300 ppm for 10 ms
        // blocks); the setpoint keeps that block as margin.
        for (block, period) in [(480, 480), (480, 960), (1_536, 960)] {
            for skew in [-300.0, -100.0, 0.0, 100.0, 300.0] {
                for late in [0.0, 0.02] {
                    for trim in [false, true] {
                        let run = simulate_clock_with(skew, block, period, trim, 300.0, 0.0, late);
                        let label = format!(
                            "block {block} period {period} skew {skew} late {late} trim {trim}"
                        );
                        assert_eq!(run.underruns, 0, "{label}");
                        assert_eq!(run.late_clamped, 0, "{label}: still at the limit");
                    }
                }
            }
        }
    }

    #[test]
    fn trough_setpoint_keeps_one_late_delivery_beyond_the_target() {
        // The smaller of the take and the delivery: a 3 ms period loses one
        // take to a late 10 ms block, a 20 ms period loses one 10 ms block.
        assert_eq!(trough_setpoint_frames(480.0, 144.0, 480.0, 9_600.0), 624.0);
        assert_eq!(trough_setpoint_frames(480.0, 960.0, 480.0, 9_600.0), 960.0);
        assert_eq!(
            trough_setpoint_frames(480.0, 480.0, 1_536.0, 9_600.0),
            960.0
        );
        // Never above what leaves room for a delivery after the take.
        assert_eq!(
            trough_setpoint_frames(480.0, 960.0, 1_536.0, 2_976.0),
            480.0
        );
        assert_eq!(trough_setpoint_frames(480.0, 960.0, 1_536.0, 1_000.0), 0.0);
    }

    #[test]
    fn the_default_ring_never_caps_the_setpoint_for_a_dd_plus_delivery() {
        // A DD+ sync frame lands as up to four mixer blocks (40 ms). The
        // setpoint keeps one late delivery's worth above the target only while
        // the ring has room for that delivery after the take; at 80 ms the
        // 20 ms period was capped to a 20 ms trough and underran.
        let config = EngineConfig::default();
        let capacity = capacity_frames(config.capacity_ms, crate::MIX_RATE) as f64;
        let target = f64::from(config.target_ms) * 48.0;
        let delivery = 4.0 * crate::MIX_FRAMES as f64;
        for take in [144.0, 480.0, 960.0, 1_920.0, 4_096.0] {
            assert_eq!(
                trough_setpoint_frames(target, take, delivery, capacity),
                target + f64::min(take, delivery),
                "{take}-frame period"
            );
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
            OutputLayout::STEREO,
            80 * 48,
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
                    layout: OutputLayout::STEREO,
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
            OutputLayout::STEREO,
            80 * 48,
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
                    layout: OutputLayout::STEREO,
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
    fn asrc_keeps_channels_apart_with_twelve_channels() {
        let channels = 12;
        let frames = 960;
        let (mut producer, mut audio) = RingBuffer::<f32>::new(frames * channels);
        for _ in 0..frames {
            for channel in 0..channels {
                producer.push((channel + 1) as f32).unwrap();
            }
        }
        let mut asrc = AsrcClock::new(441, 44_100, channels);
        asrc.reset(48_000, 44_100);
        asrc.input_limit = frames;
        let mut output = vec![0.0; 441 * channels];
        let mut stream_frame = 0;
        let rendered = asrc.render(&mut audio, &mut output, 48_000, 44_100, &mut stream_frame);
        assert_eq!(rendered.output_frames, 441);
        for frame in output.chunks_exact(channels) {
            let reference = frame[0];
            for (channel, sample) in frame.iter().enumerate() {
                assert!(
                    (*sample - reference * (channel + 1) as f32).abs() < 1.0e-4,
                    "channel {channel} contained {sample} for reference {reference}"
                );
            }
        }
    }

    #[test]
    fn multichannel_blocks_enter_the_crossing_ring_with_their_layout() {
        let shared = Arc::new(Shared::new());
        shared.dropped_frames.store(7, Ordering::Release);
        let (mut audio, mut audio_out) = RingBuffer::<f32>::new(64);
        let (mut timing, mut timing_out) = RingBuffer::<TimingMarker>::new(4);
        let (event_producer, _event_out) = RingBuffer::<RtEvent>::new(4);
        let mut events = EventWriter::new(event_producer, Arc::clone(&shared));
        let mut stream_frame = 11;
        assert_eq!(shared.scene_rate.load(Ordering::Acquire), 0);
        let samples = [
            1.0, 2.0, 3.0, // channel 0
            11.0, 12.0, 13.0, // channel 1
            21.0, 22.0, 23.0, // channel 2
            31.0, 32.0, 33.0, // channel 3
        ];

        handle_chain_event(
            ChainEvent::Block(crate::BlockOut {
                epoch: 1,
                rate: 48_000,
                source_qpc: 23,
                channels: 4,
                channel_mask: 0x33,
                frames: 3,
                samples: &samples,
                silent: false,
                idle: false,
            }),
            &mut audio,
            &mut timing,
            &mut stream_frame,
            &mut events,
            &shared,
        );

        let interleaved = (0..12)
            .map(|_| audio_out.pop().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            interleaved,
            vec![
                1.0, 11.0, 21.0, 31.0, 2.0, 12.0, 22.0, 32.0, 3.0, 13.0, 23.0, 33.0
            ]
        );
        let marker = timing_out.pop().unwrap();
        assert_eq!(marker.stream_frame, 11);
        assert_eq!(marker.frames, 3);
        assert_eq!(marker.layout.channels, 4);
        assert_eq!(marker.layout.channel_mask, 0x33);
        assert_eq!(stream_frame, 14);
        assert_eq!(shared.scene_rate.load(Ordering::Acquire), crate::MIX_RATE);
        assert_eq!(shared.blocks.load(Ordering::Acquire), 1);
        assert_eq!(shared.status().dropped_frames, 7);
    }

    #[test]
    fn idle_blocks_are_dropped_silently_when_the_ring_is_full() {
        let shared = Shared::new();
        let (mut audio, _audio_out) = RingBuffer::<f32>::new(1);
        audio.push(1.0).unwrap();
        let (mut timing, _timing_out) = RingBuffer::<TimingMarker>::new(1);
        let (events, _events_out) = RingBuffer::<RtEvent>::new(1);
        let mut events = EventWriter::new(events, Arc::new(Shared::new()));
        let mut stream_frame = 0;
        push_block(
            crate::chain::BlockOut {
                epoch: 1,
                rate: 48_000,
                source_qpc: 1,
                channels: 2,
                channel_mask: 0x3,
                frames: 1,
                samples: &[0.0, 0.0],
                silent: true,
                idle: true,
            },
            &mut audio,
            &mut timing,
            &mut stream_frame,
            &mut events,
            &shared,
        );
        assert_eq!(shared.output_overflow_frames.load(Ordering::Acquire), 0);
        assert_eq!(shared.idle_blocks.load(Ordering::Acquire), 0);
        assert_eq!(stream_frame, 0);
    }

    #[test]
    fn a_full_ring_counts_overflowed_frames() {
        let shared = Shared::new();
        let (mut audio, _audio_out) = RingBuffer::<f32>::new(1);
        audio.push(1.0).unwrap();
        let (mut timing, _timing_out) = RingBuffer::<TimingMarker>::new(1);
        let (events, mut events_out) = RingBuffer::<RtEvent>::new(1);
        let event_shared = Arc::new(Shared::new());
        let mut events = EventWriter::new(events, event_shared);
        let mut stream_frame = 0;
        push_block(
            crate::chain::BlockOut {
                epoch: 1,
                rate: 48_000,
                source_qpc: 1,
                channels: 2,
                channel_mask: 0x3,
                frames: 2,
                samples: &[0.25; 4],
                silent: false,
                idle: false,
            },
            &mut audio,
            &mut timing,
            &mut stream_frame,
            &mut events,
            &shared,
        );
        assert_eq!(shared.output_overflow_frames.load(Ordering::Acquire), 2);
        assert!(matches!(
            events_out.pop(),
            Ok(RtEvent::OutputOverflow { frames: 2 })
        ));
        assert_eq!(stream_frame, 0);
    }

    #[test]
    fn an_overflow_burst_reports_its_frames_without_flooding_the_event_queue() {
        // A stalled output makes every delivery overflow. The burst reports
        // its first block at once and the rest when a block fits again, so a
        // thousand overflowing blocks neither fill an 8-slot queue nor lose a
        // frame from the events' total.
        let shared = Shared::new();
        let (mut audio, mut audio_out) = RingBuffer::<f32>::new(4);
        let (mut timing, mut timing_out) = RingBuffer::<TimingMarker>::new(4);
        let (events, mut events_out) = RingBuffer::<RtEvent>::new(8);
        let event_shared = Arc::new(Shared::new());
        let mut events = EventWriter::new(events, Arc::clone(&event_shared));
        let mut stream_frame = 0;
        let block = |samples: &'static [f32]| crate::chain::BlockOut {
            epoch: 1,
            rate: 48_000,
            source_qpc: 1,
            channels: 2,
            channel_mask: 0x3,
            frames: 2,
            samples,
            silent: false,
            idle: false,
        };
        let mut push = |audio: &mut Producer<f32>,
                        timing: &mut Producer<TimingMarker>,
                        events: &mut EventWriter| {
            push_block(
                block(&[0.25; 4]),
                audio,
                timing,
                &mut stream_frame,
                events,
                &shared,
            );
        };
        // Fill the ring, then overflow a thousand blocks.
        push(&mut audio, &mut timing, &mut events);
        for _ in 0..1_000 {
            push(&mut audio, &mut timing, &mut events);
        }
        // The output takes the block; the next one fits and ends the burst.
        for _ in 0..4 {
            audio_out.pop().unwrap();
        }
        timing_out.pop().unwrap();
        push(&mut audio, &mut timing, &mut events);

        let mut reported = Vec::new();
        while let Ok(RtEvent::OutputOverflow { frames }) = events_out.pop() {
            reported.push(frames);
        }
        // The first block at once, the rest at the end (and one more only if
        // the loop took over a second).
        assert_eq!(event_shared.dropped_events.load(Ordering::Acquire), 0);
        assert_eq!(reported.first(), Some(&2), "{reported:?}");
        assert!(reported.len() <= 3, "{reported:?}");
        assert_eq!(reported.iter().sum::<u64>(), 2_000);
        assert_eq!(shared.output_overflow_frames.load(Ordering::Acquire), 2_000);
    }

    #[test]
    fn priming_never_discards_sound() {
        fn discarded(samples: &[f32], frames: usize) -> (u64, Vec<f32>) {
            let shared = Arc::new(Shared::new());
            shared.scene_rate.store(48_000, Ordering::Release);
            let (mut producer, audio) = RingBuffer::<f32>::new(samples.len().max(2));
            for &sample in samples {
                producer.push(sample).unwrap();
            }
            let (_timing_in, timing) = RingBuffer::<TimingMarker>::new(TIMING_QUEUE_CAPACITY);
            let (events, _events_out) = RingBuffer::<RtEvent>::new(EVENT_QUEUE_CAPACITY);
            let mut output = OutputState::new(
                audio,
                timing,
                48_000,
                48,
                10.0,
                DEFAULT_QPC_FREQUENCY,
                events,
                shared,
                OutputLayout::STEREO,
                samples.len() / 2,
            );
            output.discard(frames);
            let remaining = output.audio.slots();
            let mut samples = vec![0.0; remaining];
            let _ = output.audio.pop_partial_slice(&mut samples);
            (output.stream_frame, samples)
        }

        let sound = [0.25, -0.25, 0.5, -0.5];
        let (frames, remaining) = discarded(&sound, 2);
        assert_eq!(frames, 0);
        assert_eq!(remaining, sound);

        let samples = [0.0, 0.0, 0.0, 0.0, 0.25, -0.25, 0.5, -0.5];
        let (frames, remaining) = discarded(&samples, 4);
        assert_eq!(frames, 2);
        assert_eq!(remaining, samples[4..]);
        assert!(
            samples[..frames as usize * 2]
                .iter()
                .all(|sample| *sample == 0.0)
        );
    }

    #[test]
    fn latency_writer_matches_previous_percentiles_and_resets_by_generation() {
        let shared = Shared::new();
        let mut writer = LatencyWriter::new(&shared);
        for value in 1..=100 {
            writer.push(value as f32, &shared);
        }
        let status = shared.status();
        assert_eq!(status.latency.samples, 100);
        assert_eq!(status.latency.p50_ms, 51.0);
        assert_eq!(status.latency.p99_ms, 99.0);

        writer.reset(&shared);
        writer.push(7.0, &shared);
        assert_eq!(
            shared.status().latency,
            LatencyStats {
                p50_ms: 7.0,
                p99_ms: 7.0,
                samples: 1,
            }
        );
    }

    #[test]
    fn latency_slot_sequences_do_not_alias_across_the_u32_wrap() {
        let shared = Shared::new();
        let mut writer = LatencyWriter::new(&shared);
        writer.next = u64::from(u32::MAX) - 511;
        shared.latency_next.store(writer.next, Ordering::Release);
        for _ in 0..LATENCY_SAMPLES {
            writer.push(7.0, &shared);
        }

        assert_eq!(shared.latency().samples, LATENCY_SAMPLES as u32);
        assert_eq!(shared.latency().p50_ms, 7.0);
        assert_eq!(shared.latency().p99_ms, 7.0);
        let sequence = u64::from(u32::MAX) + 511;
        let index = sequence as usize % LATENCY_SAMPLES;
        assert_eq!(
            shared.latency_sequences[index].load(Ordering::Acquire),
            sequence.wrapping_add(1)
        );
    }

    #[test]
    fn latency_status_never_observes_a_slot_while_its_value_is_being_published() {
        let shared = Arc::new(Shared::new());
        let writer_shared = Arc::clone(&shared);
        let writer = std::thread::spawn(move || {
            let mut writer = LatencyWriter::new(&writer_shared);
            for value in 1..=100_000_u32 {
                writer.push(value as f32, &writer_shared);
            }
        });
        while !writer.is_finished() {
            let latency = shared.latency();
            if latency.samples != 0 {
                assert!(latency.p50_ms >= 1.0, "{latency:?}");
                assert!(latency.p99_ms >= latency.p50_ms, "{latency:?}");
            }
        }
        writer.join().unwrap();
        assert_eq!(shared.latency().samples, LATENCY_SAMPLES as u32);
    }

    #[test]
    fn latency_status_never_mixes_samples_across_concurrent_resets() {
        let shared = Arc::new(Shared::new());
        let writer_shared = Arc::clone(&shared);
        let writer = std::thread::spawn(move || {
            let mut writer = LatencyWriter::new(&writer_shared);
            for generation in 1..=2_000_u32 {
                writer.reset(&writer_shared);
                let value = generation as f32;
                for _ in 0..32 {
                    writer.push(value, &writer_shared);
                }
            }
        });
        while !writer.is_finished() {
            let latency = shared.latency();
            if latency.samples != 0 {
                assert_eq!(latency.p50_ms, latency.p99_ms, "{latency:?}");
            }
        }
        writer.join().unwrap();
    }

    #[test]
    fn a_short_terminal_asrc_take_fades_inside_the_rendered_period_and_drains_silently() {
        let shared = Arc::new(Shared::new());
        shared.scene_rate.store(48_000, Ordering::Release);
        shared
            .clock_mode
            .store(ClockMode::Asrc.as_u32(), Ordering::Release);
        shared.capture_started.store(true, Ordering::Release);
        let period_frames = 441;
        let real_frames = 8;
        let new_layout = OutputLayout {
            channels: 12,
            channel_mask: 0,
        };
        let (mut audio_in, audio) = RingBuffer::<f32>::new(real_frames * 2 + 12);
        for frame in 0..real_frames {
            let sample = (2.0 * std::f32::consts::PI * 997.0 * frame as f32 / 48_000.0).sin();
            audio_in.push(sample).unwrap();
            audio_in.push(sample).unwrap();
        }
        for _ in 0..12 {
            audio_in.push(0.0).unwrap();
        }
        let (mut timing_in, timing) = RingBuffer::<TimingMarker>::new(TIMING_QUEUE_CAPACITY);
        timing_in
            .push(TimingMarker {
                stream_frame: 0,
                frames: real_frames as u32,
                source_qpc: 0,
                rate: 48_000,
                layout: OutputLayout::STEREO,
            })
            .unwrap();
        timing_in
            .push(TimingMarker {
                stream_frame: real_frames as u64,
                frames: 1,
                source_qpc: real_frames as u64,
                rate: 48_000,
                layout: new_layout,
            })
            .unwrap();
        let (events, _events_out) = RingBuffer::<RtEvent>::new(EVENT_QUEUE_CAPACITY);
        let mut output = OutputState::new(
            audio,
            timing,
            44_100,
            period_frames,
            0.0,
            DEFAULT_QPC_FREQUENCY,
            events,
            shared,
            OutputLayout::STEREO,
            period_frames,
        );
        output.asrc.reset(48_000, 44_100);
        output.priming = false;
        output.fade_in_remaining = 0;
        let mut period = vec![0.0; period_frames * 2];
        output.fill(&mut period);

        let predicted_signal_end = 7 + output.asrc.output_delay();
        assert!(predicted_signal_end < period_frames);
        assert_eq!(output.period_signal_end, predicted_signal_end);
        let left: Vec<_> = period
            .as_chunks::<2>()
            .0
            .iter()
            .map(|frame| frame[0])
            .collect();
        let first_zero = left[..predicted_signal_end]
            .iter()
            .rposition(|sample| *sample == 0.0)
            .expect("the in-period fade did not reach exact zero");
        assert!(first_zero >= predicted_signal_end.saturating_sub(2));
        assert!(
            left[predicted_signal_end..]
                .iter()
                .all(|sample| *sample == 0.0)
        );
        let tone_step = 2.0 * (std::f32::consts::PI * 997.0 / 44_100.0).sin();
        let max_step = left[..predicted_signal_end]
            .windows(2)
            .map(|pair| (pair[1] - pair[0]).abs())
            .fold(0.0_f32, f32::max);
        assert!(
            max_step <= tone_step + 0.05,
            "short terminal fade step {max_step} exceeded tone bound {}",
            tone_step + 0.05
        );
        assert_eq!(output.pending_layout_change(), Some(new_layout));

        let mut drain = vec![1.0; period_frames * 2];
        output.asrc.flush(&mut drain);
        output.apply_asrc_drain_fade(&mut drain);
        assert!(drain.iter().all(|sample| *sample == 0.0));
    }

    /// Records every period the output writes after the test's own renders.
    struct TailSink {
        periods: Vec<Vec<f32>>,
        period_frames: usize,
    }

    impl OutputSink for TailSink {
        fn format(&self) -> ssr_types::OutputFormat {
            ssr_types::OutputFormat::exclusive(44_100, 2, 0, self.period_frames)
        }

        fn next_period(&mut self, fill: &mut dyn FnMut(&mut [f32])) -> Result<u64, OutputError> {
            let mut period = vec![0.0; self.period_frames * 2];
            fill(&mut period);
            self.periods.push(period);
            Ok(0)
        }
    }

    /// A 48 kHz scene into a 44.1 kHz ASRC whose ring holds exactly one take
    /// of a 997 Hz tone, rendered with capture still running.
    fn asrc_after_one_whole_take() -> (OutputState, Arc<Shared>, Vec<f32>) {
        asrc_after_one_whole_take_with_period(441)
    }

    fn asrc_after_one_whole_take_with_period(
        period_frames: usize,
    ) -> (OutputState, Arc<Shared>, Vec<f32>) {
        let (output, shared, period, _, _) = asrc_after_one_whole_take_keeping_input(period_frames);
        (output, shared, period)
    }

    /// As above, with the ring's producers kept so more input can follow.
    fn asrc_after_one_whole_take_keeping_input(
        period_frames: usize,
    ) -> (
        OutputState,
        Arc<Shared>,
        Vec<f32>,
        rtrb::Producer<f32>,
        rtrb::Producer<TimingMarker>,
    ) {
        let shared = Arc::new(Shared::new());
        shared.scene_rate.store(48_000, Ordering::Release);
        shared
            .clock_mode
            .store(ClockMode::Asrc.as_u32(), Ordering::Release);
        shared.capture_started.store(true, Ordering::Release);
        let (mut audio_in, audio) = RingBuffer::<f32>::new(4_096 * 2);
        let (mut timing_in, timing) = RingBuffer::<TimingMarker>::new(TIMING_QUEUE_CAPACITY);
        let (events, _events_out) = RingBuffer::<RtEvent>::new(EVENT_QUEUE_CAPACITY);
        let mut output = OutputState::new(
            audio,
            timing,
            44_100,
            period_frames,
            0.0,
            DEFAULT_QPC_FREQUENCY,
            events,
            Arc::clone(&shared),
            OutputLayout::STEREO,
            4_096,
        );
        output.asrc.reset(48_000, 44_100);
        output.priming = false;
        output.fade_in_remaining = 0;
        let take = output.asrc.frames_needed();
        for frame in 0..take {
            let sample = (2.0 * std::f32::consts::PI * 997.0 * frame as f32 / 48_000.0).sin();
            audio_in.push(sample).unwrap();
            audio_in.push(sample).unwrap();
        }
        timing_in
            .push(TimingMarker {
                stream_frame: 0,
                frames: take as u32,
                source_qpc: 0,
                rate: 48_000,
                layout: OutputLayout::STEREO,
            })
            .unwrap();
        let mut period = vec![0.0; period_frames * 2];
        output.fill(&mut period);
        assert_eq!(output.audio.slots(), 0, "the take did not empty the ring");
        assert_eq!(shared.underruns.load(Ordering::Acquire), 0);
        assert_eq!(output.tail_periods, 0, "capture had not ended yet");
        (output, shared, period, audio_in, timing_in)
    }

    fn left(samples: &[f32]) -> Vec<f32> {
        samples
            .as_chunks::<2>()
            .0
            .iter()
            .map(|frame| frame[0])
            .collect()
    }

    fn max_step(frames: &[f32]) -> f32 {
        frames
            .windows(2)
            .map(|pair| (pair[1] - pair[0]).abs())
            .fold(0.0_f32, f32::max)
    }

    #[test]
    fn the_end_of_capture_seen_after_the_last_take_drains_the_tail_right_after_it() {
        let (mut output, shared, period) = asrc_after_one_whole_take();
        let delay = output.asrc.output_delay();
        assert_eq!(delay, 117);
        assert_eq!(boundary_fade_frames(44_100), 88);

        // The end of capture becomes visible only now, after the take that
        // emptied the ring went out as an ordinary period.
        shared.capture_done.store(true, Ordering::Release);
        assert!(output.ended());
        let mut sink = TailSink {
            periods: Vec::new(),
            period_frames: 441,
        };
        output.write_terminal_tail(&mut sink).unwrap();

        // One drain period follows the rendered one: no empty period, no
        // underrun, and the tail is written once.
        assert_eq!(sink.periods.len(), 1);
        assert_eq!(shared.underruns.load(Ordering::Acquire), 0);
        assert_eq!(output.tail_periods, 0);
        assert!(output.write_terminal_tail(&mut sink).is_ok());
        assert_eq!(sink.periods.len(), 1, "the tail was written twice");

        // The whole take was real input, so the real signal runs `delay`
        // frames into the drain: full level up to the fade, which takes the
        // last 88 of them to exact zero.
        let mut frames = left(&period);
        frames.extend(left(&sink.periods[0]));
        let signal_end = 441 + delay;
        let fade_start = signal_end - 88;
        let pre_fade_peak = frames[441..fade_start]
            .iter()
            .fold(0.0_f32, |peak, sample| peak.max(sample.abs()));
        assert!(
            pre_fade_peak > 0.5,
            "the drain lost the tail before its fade: peak {pre_fade_peak}"
        );
        assert_eq!(frames[signal_end - 1], 0.0);
        assert!(frames[signal_end - 2] != 0.0);
        assert!(frames[signal_end..].iter().all(|sample| *sample == 0.0));
        let tone_step = 2.0 * (std::f32::consts::PI * 997.0 / 44_100.0).sin();
        let step = max_step(&frames[220..signal_end]);
        assert!(
            step <= tone_step + 0.05,
            "the tail step {step} exceeded the tone bound {}",
            tone_step + 0.05
        );
    }

    #[test]
    fn the_end_of_capture_seen_while_the_device_asks_for_a_period_puts_the_tail_in_it() {
        let (mut output, shared, period) = asrc_after_one_whole_take();
        let delay = output.asrc.output_delay();

        // The output thread is waiting inside the sink when capture ends, so
        // the end is first seen by the period the device asks for next.
        shared.capture_done.store(true, Ordering::Release);
        let mut next = vec![0.0; 441 * 2];
        output.fill(&mut next);

        assert_eq!(shared.underruns.load(Ordering::Acquire), 0);
        assert_eq!(output.tail_periods, 0);
        assert!(output.ended());
        let mut sink = TailSink {
            periods: Vec::new(),
            period_frames: 441,
        };
        output.write_terminal_tail(&mut sink).unwrap();
        assert!(sink.periods.is_empty(), "the tail was written twice");

        let mut frames = left(&period);
        frames.extend(left(&next));
        let signal_end = 441 + delay;
        let pre_fade_peak = frames[441..signal_end - 88]
            .iter()
            .fold(0.0_f32, |peak, sample| peak.max(sample.abs()));
        assert!(
            pre_fade_peak > 0.5,
            "the period lost the tail before its fade: peak {pre_fade_peak}"
        );
        assert_eq!(frames[signal_end - 1], 0.0);
        assert!(frames[signal_end..].iter().all(|sample| *sample == 0.0));
        let tone_step = 2.0 * (std::f32::consts::PI * 997.0 / 44_100.0).sin();
        let step = max_step(&frames[220..signal_end]);
        assert!(
            step <= tone_step + 0.05,
            "the tail step {step} exceeded the tone bound {}",
            tone_step + 0.05
        );
    }

    #[test]
    fn a_layout_change_seen_while_the_device_asks_for_a_period_puts_the_old_tail_in_it() {
        let (mut output, shared, period, mut audio_in, mut timing_in) =
            asrc_after_one_whole_take_keeping_input(441);
        let delay = output.asrc.output_delay();
        let new_layout = OutputLayout {
            channels: 12,
            channel_mask: 0,
        };
        // While the device waits, capture delivers a block in a new layout.
        timing_in
            .push(TimingMarker {
                stream_frame: output.stream_frame,
                frames: 1,
                source_qpc: output.stream_frame,
                rate: 48_000,
                layout: new_layout,
            })
            .unwrap();
        for _ in 0..12 {
            audio_in.push(0.25).unwrap();
        }
        let mut next = vec![0.0; 441 * 2];
        output.fill(&mut next);

        assert_eq!(shared.underruns.load(Ordering::Acquire), 0);
        assert_eq!(output.pending_layout_change(), Some(new_layout));
        assert_eq!(output.tail_periods, 0);
        assert!(!output.begin_reopen_tail(), "the reopen owes a second tail");
        let mut frames = left(&period);
        frames.extend(left(&next));
        let signal_end = 441 + delay;
        let pre_fade_peak = frames[441..signal_end - 88]
            .iter()
            .fold(0.0_f32, |peak, sample| peak.max(sample.abs()));
        assert!(
            pre_fade_peak > 0.5,
            "the old layout's tail was not in the period: peak {pre_fade_peak}"
        );
        assert_eq!(frames[signal_end - 1], 0.0);
        assert!(frames[signal_end..].iter().all(|sample| *sample == 0.0));
        let tone_step = 2.0 * (std::f32::consts::PI * 997.0 / 44_100.0).sin();
        let step = max_step(&frames[220..signal_end]);
        assert!(
            step <= tone_step + 0.05,
            "the tail step {step} exceeded the tone bound {}",
            tone_step + 0.05
        );
    }

    #[test]
    fn the_end_of_capture_during_a_reopen_drain_writes_only_the_periods_left() {
        let (mut output, shared, period) = asrc_after_one_whole_take_with_period(44);
        let delay = output.asrc.output_delay();
        assert_eq!(output.asrc.drain_periods(), 3);
        let mut sink = TailSink {
            periods: Vec::new(),
            period_frames: 44,
        };
        assert!(output.begin_reopen_tail());
        assert!(!output.write_reopen_tail_period(&mut sink).unwrap());

        shared.capture_done.store(true, Ordering::Release);
        assert!(output.ended());
        output.write_terminal_tail(&mut sink).unwrap();
        assert_eq!(sink.periods.len(), 3, "the end restarted the reopen drain");

        let mut frames = left(&period);
        for tail in &sink.periods {
            frames.extend(left(tail));
        }
        let signal_end = 44 + delay;
        assert_eq!(frames[signal_end - 1], 0.0);
        assert!(frames[signal_end - 2] != 0.0);
        assert!(frames[signal_end..].iter().all(|sample| *sample == 0.0));
        let tone_step = 2.0 * (std::f32::consts::PI * 997.0 / 44_100.0).sin();
        let step = max_step(&frames[..signal_end]);
        assert!(
            step <= tone_step + 0.05,
            "the interrupted tail stepped {step}, above the tone bound {}",
            tone_step + 0.05
        );
    }

    fn ended_output(mode: ClockMode, frames: usize) -> (OutputState, Arc<Shared>) {
        let shared = Arc::new(Shared::new());
        shared.scene_rate.store(48_000, Ordering::Release);
        shared.clock_mode.store(mode.as_u32(), Ordering::Release);
        shared.capture_started.store(true, Ordering::Release);
        shared.capture_input_ended.store(true, Ordering::Release);
        shared.capture_done.store(true, Ordering::Release);
        let (mut audio_in, audio) = RingBuffer::<f32>::new(4_096 * 2);
        let (mut timing_in, timing) = RingBuffer::<TimingMarker>::new(TIMING_QUEUE_CAPACITY);
        let (events, _events_out) = RingBuffer::<RtEvent>::new(EVENT_QUEUE_CAPACITY);
        let device_rate = if mode == ClockMode::Trim {
            48_000
        } else {
            44_100
        };
        let mut output = OutputState::new(
            audio,
            timing,
            device_rate,
            480,
            0.0,
            DEFAULT_QPC_FREQUENCY,
            events,
            Arc::clone(&shared),
            OutputLayout::STEREO,
            4_096,
        );
        output.asrc.reset(48_000, device_rate);
        output.fade_in_remaining = 0;
        for frame in 0..frames {
            let sample = (2.0 * std::f32::consts::PI * 997.0 * frame as f32 / 48_000.0).sin();
            audio_in.push(sample).unwrap();
            audio_in.push(sample).unwrap();
        }
        if frames != 0 {
            timing_in
                .push(TimingMarker {
                    stream_frame: 0,
                    frames: frames as u32,
                    source_qpc: 0,
                    rate: 48_000,
                    layout: OutputLayout::STEREO,
                })
                .unwrap();
        }
        (output, shared)
    }

    #[test]
    fn a_stream_that_ends_before_any_audio_owes_no_tail_and_no_underrun() {
        let (mut output, shared) = ended_output(ClockMode::Asrc, 0);
        assert!(output.asrc.drain_periods() != 0);
        let mut period = vec![1.0; 480 * 2];
        output.fill(&mut period);
        assert!(period.iter().all(|sample| *sample == 0.0));
        assert_eq!(shared.underruns.load(Ordering::Acquire), 0);
        assert_eq!(output.tail_periods, 0);
        assert!(!output.begin_reopen_tail(), "a reopen invented a tail");
        let mut sink = TailSink {
            periods: Vec::new(),
            period_frames: 480,
        };
        output.write_terminal_tail(&mut sink).unwrap();
        assert!(sink.periods.is_empty(), "the end invented a tail");
    }

    #[test]
    fn a_trim_stream_that_ends_inside_a_period_plays_its_last_frames_without_an_underrun() {
        let (mut output, shared) = ended_output(ClockMode::Trim, 100);
        let mut period = vec![0.0; 480 * 2];
        output.fill(&mut period);
        assert_eq!(shared.underruns.load(Ordering::Acquire), 0);
        assert!(output.drained());
        // Every one of the 100 frames plays, the last 88 under the end fade.
        let fade = boundary_fade_frames(48_000);
        for (frame, played) in left(&period[..100 * 2]).into_iter().enumerate() {
            let source = (2.0 * std::f32::consts::PI * 997.0 * frame as f32 / 48_000.0).sin();
            let gain = match frame.checked_sub(100 - fade) {
                None => 1.0,
                Some(offset) => (fade - 1 - offset) as f32 / (fade - 1) as f32,
            };
            assert!(
                (played - source * gain).abs() <= 1.0e-6,
                "frame {frame} played {played}, expected {}",
                source * gain
            );
        }
        assert!(period[100 * 2..].iter().all(|sample| *sample == 0.0));
    }

    #[test]
    fn a_reopen_gives_way_to_finishing_once_everything_is_rendered() {
        let layout = OutputLayout {
            channels: 12,
            channel_mask: 0,
        };
        assert!(matches!(
            end_before_reopen(OutputStep::ChangeLayout(layout), true),
            OutputStep::Continue
        ));
        assert!(matches!(
            end_before_reopen(OutputStep::GrowCadence(Duration::from_millis(20)), true),
            OutputStep::Continue
        ));
        assert!(matches!(
            end_before_reopen(OutputStep::Lost(OutputError::Removed), true),
            OutputStep::Lost(_)
        ));
        assert!(matches!(
            end_before_reopen(OutputStep::ChangeLayout(layout), false),
            OutputStep::ChangeLayout(_)
        ));
        assert!(matches!(
            end_before_reopen(OutputStep::GrowCadence(Duration::from_millis(20)), false),
            OutputStep::GrowCadence(_)
        ));
    }

    #[test]
    fn an_underrun_before_the_end_of_capture_fades_the_tail_back_in() {
        let (mut output, shared, _) = asrc_after_one_whole_take();
        let delay = output.asrc.output_delay();
        let mut underrun = vec![0.0; 441 * 2];
        output.fill(&mut underrun);
        assert_eq!(shared.underruns.load(Ordering::Acquire), 1);
        let underrun = left(&underrun);
        assert_eq!(*underrun.last().unwrap(), 0.0);

        shared.capture_done.store(true, Ordering::Release);
        assert!(output.ended());
        let mut sink = TailSink {
            periods: Vec::new(),
            period_frames: 441,
        };
        output.write_terminal_tail(&mut sink).unwrap();
        assert_eq!(sink.periods.len(), 1);
        assert_eq!(shared.underruns.load(Ordering::Acquire), 1);

        // The tail comes back after silence, so it starts from zero and ramps
        // up instead of jumping to the level the take ended at.
        let drain = left(&sink.periods[0]);
        assert_eq!(drain[0], 0.0);
        let peak = drain
            .iter()
            .fold(0.0_f32, |peak, sample| peak.max(sample.abs()));
        assert!(peak > 0.1, "the tail after the underrun was dropped");
        assert!(drain[delay..].iter().all(|sample| *sample == 0.0));
        let mut frames = underrun;
        frames.extend(drain);
        let tone_step = 2.0 * (std::f32::consts::PI * 997.0 / 44_100.0).sin();
        let step = max_step(&frames);
        assert!(
            step <= tone_step + 0.05,
            "the tail after the underrun stepped {step}, above the tone bound {}",
            tone_step + 0.05
        );
    }

    #[test]
    fn layout_boundary_fades_out_the_old_sink_without_dropping_its_remainder() {
        let shared = Arc::new(Shared::new());
        shared.scene_rate.store(48_000, Ordering::Release);
        shared
            .clock_mode
            .store(ClockMode::Trim.as_u32(), Ordering::Release);
        shared.capture_started.store(true, Ordering::Release);
        let period_frames = 480;
        let old_frames = 48;
        let old_samples = old_frames * 2;
        let new_layout = OutputLayout {
            channels: 12,
            channel_mask: 0,
        };
        let (mut audio_in, audio) = RingBuffer::<f32>::new(old_samples + 12);
        for _ in 0..old_samples {
            audio_in.push(1.0).unwrap();
        }
        for _ in 0..12 {
            audio_in.push(0.5).unwrap();
        }
        let (mut timing_in, timing) = RingBuffer::<TimingMarker>::new(TIMING_QUEUE_CAPACITY);
        timing_in
            .push(TimingMarker {
                stream_frame: 0,
                frames: old_frames as u32,
                source_qpc: 0,
                rate: 48_000,
                layout: OutputLayout::STEREO,
            })
            .unwrap();
        timing_in
            .push(TimingMarker {
                stream_frame: old_frames as u64,
                frames: 1,
                source_qpc: old_frames as u64,
                rate: 48_000,
                layout: new_layout,
            })
            .unwrap();
        let (events, _events_out) = RingBuffer::<RtEvent>::new(EVENT_QUEUE_CAPACITY);
        let mut output = OutputState::new(
            audio,
            timing,
            48_000,
            period_frames,
            0.0,
            DEFAULT_QPC_FREQUENCY,
            events,
            shared,
            OutputLayout::STEREO,
            period_frames,
        );
        output.asrc.reset(48_000, 48_000);
        output.priming = false;
        output.fade_in_remaining = 0;
        let mut period = vec![0.0; period_frames * 2];
        output.fill(&mut period);

        assert!(
            period[old_samples..period_frames * 2]
                .iter()
                .all(|sample| *sample == 0.0)
        );
        let audible = &period[..old_samples];
        assert_eq!(audible[0], 1.0);
        assert_eq!(audible[1], 1.0);
        assert_eq!(audible[old_samples - 2], 0.0);
        assert_eq!(audible[old_samples - 1], 0.0);
        assert_eq!(output.pending_layout_change(), Some(new_layout));
        assert_eq!(output.audio.slots(), 12);
    }

    #[test]
    fn retry_observes_a_return_to_the_current_layout() {
        let shared = Arc::new(Shared::new());
        let rejected = OutputLayout {
            channels: 6,
            channel_mask: 0x3f,
        };
        let (mut audio_in, audio) = RingBuffer::<f32>::new(32);
        for _ in 0..16 {
            audio_in.push(1.0).unwrap();
        }
        let (mut timing_in, timing) = RingBuffer::<TimingMarker>::new(TIMING_QUEUE_CAPACITY);
        timing_in
            .push(TimingMarker {
                stream_frame: 0,
                frames: 2,
                source_qpc: 1,
                rate: 48_000,
                layout: rejected,
            })
            .unwrap();
        timing_in
            .push(TimingMarker {
                stream_frame: 2,
                frames: 2,
                source_qpc: 2,
                rate: 48_000,
                layout: OutputLayout::STEREO,
            })
            .unwrap();
        let (events, _events_out) = RingBuffer::<RtEvent>::new(EVENT_QUEUE_CAPACITY);
        let mut output = OutputState::new(
            audio,
            timing,
            48_000,
            480,
            0.0,
            DEFAULT_QPC_FREQUENCY,
            events,
            shared,
            OutputLayout::STEREO,
            480,
        );

        assert_eq!(output.pending_layout_change(), Some(rejected));
        assert_eq!(output.discard_pending_layout(rejected), 2);
        assert_eq!(output.layout_at_boundary(), Some(OutputLayout::STEREO));
        assert_eq!(output.pending_layout_change(), None);
    }

    #[test]
    fn partially_consumed_marker_is_discarded_from_its_remaining_frame() {
        let shared = Arc::new(Shared::new());
        shared.scene_rate.store(48_000, Ordering::Release);
        shared
            .clock_mode
            .store(ClockMode::Trim.as_u32(), Ordering::Release);
        let rejected = OutputLayout::STEREO;
        let newer = OutputLayout {
            channels: 6,
            channel_mask: 0x3f,
        };
        let (mut audio_in, audio) = RingBuffer::<f32>::new(2_000);
        for _ in 0..(480 * 2 + 6) {
            audio_in.push(1.0).unwrap();
        }
        let (mut timing_in, timing) = RingBuffer::<TimingMarker>::new(TIMING_QUEUE_CAPACITY);
        timing_in
            .push(TimingMarker {
                stream_frame: 0,
                frames: 480,
                source_qpc: 1,
                rate: 48_000,
                layout: rejected,
            })
            .unwrap();
        timing_in
            .push(TimingMarker {
                stream_frame: 480,
                frames: 1,
                source_qpc: 2,
                rate: 48_000,
                layout: newer,
            })
            .unwrap();
        let (events, _events_out) = RingBuffer::<RtEvent>::new(EVENT_QUEUE_CAPACITY);
        let mut output = OutputState::new(
            audio,
            timing,
            48_000,
            144,
            0.0,
            DEFAULT_QPC_FREQUENCY,
            events,
            shared,
            rejected,
            480,
        );
        output.strategy_mode = ClockMode::Trim;
        output.asrc.reset(48_000, 48_000);
        output.priming = false;
        output.fade_in_remaining = 0;
        let mut period = vec![0.0; 144 * 2];
        output.fill(&mut period);
        assert_eq!(output.stream_frame, 144);
        output.finish_period(0);

        assert_eq!(output.discard_pending_layout(rejected), 336);
        assert_eq!(output.stream_frame, 480);
        assert_eq!(output.pending_layout_change(), Some(newer));
        assert_eq!(output.audio.slots(), 6);
    }

    #[test]
    fn rejected_layout_discards_only_that_layout_and_counts_overflow() {
        let shared = Arc::new(Shared::new());
        shared.scene_rate.store(48_000, Ordering::Release);
        let rejected = OutputLayout {
            channels: 2,
            channel_mask: 0x3,
        };
        let newer = OutputLayout {
            channels: 2,
            channel_mask: 0,
        };
        let (mut audio_in, audio) = RingBuffer::<f32>::new(16);
        for sample in 0..12 {
            audio_in.push(sample as f32).unwrap();
        }
        let (mut timing_in, timing) = RingBuffer::<TimingMarker>::new(TIMING_QUEUE_CAPACITY);
        timing_in
            .push(TimingMarker {
                stream_frame: 0,
                frames: 4,
                source_qpc: 1,
                rate: 48_000,
                layout: rejected,
            })
            .unwrap();
        timing_in
            .push(TimingMarker {
                stream_frame: 4,
                frames: 2,
                source_qpc: 2,
                rate: 48_000,
                layout: newer,
            })
            .unwrap();
        let (events, mut events_out) = RingBuffer::<RtEvent>::new(EVENT_QUEUE_CAPACITY);
        let mut output = OutputState::new(
            audio,
            timing,
            48_000,
            480,
            0.0,
            DEFAULT_QPC_FREQUENCY,
            events,
            Arc::clone(&shared),
            OutputLayout {
                channels: 6,
                channel_mask: 0x3f,
            },
            480,
        );

        assert_eq!(output.pending_layout_change(), Some(rejected));
        assert_eq!(output.discard_pending_layout(rejected), 4);
        assert_eq!(output.stream_frame, 4);
        assert_eq!(output.audio.slots(), 4);
        assert_eq!(output.pending_layout_change(), Some(newer));
        assert_eq!(shared.output_overflow_frames.load(Ordering::Acquire), 4);
        assert!(matches!(
            events_out.pop(),
            Ok(RtEvent::OutputOverflow { frames: 4 })
        ));
    }

    #[test]
    fn zero_frame_starvation_fades_from_the_previous_output_frame() {
        let shared = Arc::new(Shared::new());
        shared.scene_rate.store(48_000, Ordering::Release);
        shared
            .clock_mode
            .store(ClockMode::Trim.as_u32(), Ordering::Release);
        let (mut audio_in, audio) = RingBuffer::<f32>::new(960);
        for _ in 0..960 {
            audio_in.push(0.75).unwrap();
        }
        let (mut timing_in, timing) = RingBuffer::<TimingMarker>::new(TIMING_QUEUE_CAPACITY);
        timing_in
            .push(TimingMarker {
                stream_frame: 0,
                frames: 480,
                source_qpc: 1,
                rate: 48_000,
                layout: OutputLayout::STEREO,
            })
            .unwrap();
        let (events, _events_out) = RingBuffer::<RtEvent>::new(EVENT_QUEUE_CAPACITY);
        let mut output = OutputState::new(
            audio,
            timing,
            48_000,
            480,
            0.0,
            DEFAULT_QPC_FREQUENCY,
            events,
            shared,
            OutputLayout::STEREO,
            480,
        );
        output.strategy_mode = ClockMode::Trim;
        output.asrc.reset(48_000, 48_000);
        output.priming = false;
        output.fade_in_remaining = 0;
        let mut period = vec![0.0; 960];
        output.fill(&mut period);
        assert_eq!(&period[958..], &[0.75, 0.75]);

        output.fill(&mut period);

        let fade_frames = boundary_fade_frames(48_000);
        assert_eq!(&period[..2], &[0.75, 0.75]);
        assert_eq!(&period[(fade_frames - 1) * 2..fade_frames * 2], &[0.0, 0.0]);
        assert!(
            period[fade_frames * 2..]
                .iter()
                .all(|sample| *sample == 0.0)
        );
    }

    #[test]
    fn trough_is_the_ring_left_after_the_period() {
        // A period that takes several deliveries reads past markers that stay
        // queued until finish_period. The trough sampled after the render must
        // be what the ring holds, not those markers counted whole: the soaks
        // ran at 20 ms periods on 10 ms deliveries, saw "20 ms" while the ring
        // held 0-10 ms, and underran.
        for (period_frames, block_frames) in [(480, 480), (960, 480), (1_440, 480), (960, 1_536)] {
            let label = format!("period {period_frames}, block {block_frames}");
            let shared = Arc::new(Shared::new());
            shared.scene_rate.store(48_000, Ordering::Release);
            shared
                .clock_mode
                .store(ClockMode::Asrc.as_u32(), Ordering::Release);
            let (mut audio_in, audio) = RingBuffer::<f32>::new(48_000 * 2);
            let (mut timing_in, timing) = RingBuffer::<TimingMarker>::new(TIMING_QUEUE_CAPACITY);
            let (events, _events_out) = RingBuffer::<RtEvent>::new(EVENT_QUEUE_CAPACITY);
            let mut output = OutputState::new(
                audio,
                timing,
                48_000,
                period_frames,
                10.0,
                DEFAULT_QPC_FREQUENCY,
                events,
                Arc::clone(&shared),
                OutputLayout::STEREO,
                48_000,
            );
            let mut stream = 0_u64;
            let mut delivered = 0_usize;
            let mut deliver = |audio_in: &mut Producer<f32>,
                               timing_in: &mut Producer<TimingMarker>,
                               delivered: &mut usize| {
                timing_in
                    .push(TimingMarker {
                        stream_frame: stream,
                        frames: block_frames as u32,
                        source_qpc: 1,
                        rate: 48_000,
                        layout: OutputLayout::STEREO,
                    })
                    .unwrap();
                for _ in 0..block_frames * 2 {
                    audio_in.push(0.25).unwrap();
                }
                stream += block_frames as u64;
                *delivered += block_frames;
            };
            let mut period = vec![0.0; period_frames * 2];
            let mut taken = 0_usize;
            for n in 0..150_u64 {
                // Deliver at the period's average rate, with the source a
                // little fast so the controller acts on a moving level.
                while delivered < taken + period_frames * 4 + period_frames * n as usize / 1_000 {
                    deliver(&mut audio_in, &mut timing_in, &mut delivered);
                }
                output.fill(&mut period);
                let ring = output.audio.slots() / 2;
                assert_eq!(output.period_trough_frames, ring, "{label}, period {n}");
                taken += output.period_source_frames;
                output.finish_period(n * period_frames as u64);
            }
            assert_eq!(shared.underruns.load(Ordering::Acquire), 0, "{label}");
        }
    }

    #[test]
    fn starvation_counts_an_underrun_and_installs_a_resume_fade() {
        let shared = Arc::new(Shared::new());
        shared.scene_rate.store(48_000, Ordering::Release);
        shared
            .clock_mode
            .store(ClockMode::Trim.as_u32(), Ordering::Release);
        let (_audio_in, audio) = RingBuffer::<f32>::new(960);
        let (_timing_in, timing) = RingBuffer::<TimingMarker>::new(TIMING_QUEUE_CAPACITY);
        let (events, _events_out) = RingBuffer::<RtEvent>::new(EVENT_QUEUE_CAPACITY);
        let mut output = OutputState::new(
            audio,
            timing,
            48_000,
            480,
            0.0,
            DEFAULT_QPC_FREQUENCY,
            events,
            Arc::clone(&shared),
            OutputLayout::STEREO,
            480,
        );
        output.strategy_mode = ClockMode::Trim;
        output.asrc.reset(48_000, 48_000);
        output.priming = false;
        output.fade_in_remaining = 0;
        let mut period = vec![1.0; 960];

        assert_eq!(shared.status().last_underrun, None);
        output.fill(&mut period);

        assert_eq!(shared.underruns.load(Ordering::Acquire), 1);
        assert!(output.priming);
        assert_eq!(output.fade_in_remaining, boundary_fade_frames(48_000));
        assert!(period.iter().all(|sample| *sample == 0.0));
        // The status says what the period saw: an empty ring, not a limit.
        let seen = shared
            .status()
            .last_underrun
            .expect("the underrun is published");
        assert_eq!(
            (
                seen.ring_frames,
                seen.marker_frames,
                seen.layout_frames,
                seen.needed_frames,
                seen.produced_frames
            ),
            (0, 0, 0, 480, 0)
        );
        assert!(seen.since_block_ms >= 0.0);
    }

    #[test]
    fn partial_period_consumes_existing_fade_before_installing_resume_fade() {
        let shared = Arc::new(Shared::new());
        shared.scene_rate.store(48_000, Ordering::Release);
        shared
            .clock_mode
            .store(ClockMode::Trim.as_u32(), Ordering::Release);
        let (mut audio_in, audio) = RingBuffer::<f32>::new(200);
        for _ in 0..200 {
            audio_in.push(1.0).unwrap();
        }
        let (mut timing_in, timing) = RingBuffer::<TimingMarker>::new(TIMING_QUEUE_CAPACITY);
        timing_in
            .push(TimingMarker {
                stream_frame: 0,
                frames: 100,
                source_qpc: 1,
                rate: 48_000,
                layout: OutputLayout::STEREO,
            })
            .unwrap();
        let (events, _events_out) = RingBuffer::<RtEvent>::new(EVENT_QUEUE_CAPACITY);
        let mut output = OutputState::new(
            audio,
            timing,
            48_000,
            480,
            0.0,
            DEFAULT_QPC_FREQUENCY,
            events,
            Arc::clone(&shared),
            OutputLayout::STEREO,
            480,
        );
        output.strategy_mode = ClockMode::Trim;
        output.asrc.reset(48_000, 48_000);
        output.priming = false;
        let mut period = vec![0.0; 960];

        output.fill(&mut period);

        assert_eq!(shared.underruns.load(Ordering::Acquire), 1);
        assert!(period[0].abs() <= f32::EPSILON);
        assert!(period[2] > period[0]);
        assert_eq!(period[198], 0.0);
        assert_eq!(period[199], 0.0);
        assert_eq!(output.fade_in_remaining, boundary_fade_frames(48_000));
    }

    #[test]
    fn boundary_fades_are_frame_common_end_at_zero_and_leave_normal_flow_exact() {
        let mut stereo = vec![1.0; 100 * 2];
        let mut remaining = boundary_fade_frames(48_000);
        apply_fade_in(&mut stereo, 2, 48_000, 100, &mut remaining);
        assert_eq!(stereo[0..2], [0.0, 0.0]);
        assert_eq!(stereo[2], stereo[3]);
        assert_eq!(remaining, 0);
        assert_eq!(stereo[96 * 2], 1.0);
        assert_eq!(stereo[99 * 2], 1.0);

        let shared = Arc::new(Shared::new());
        shared.scene_rate.store(48_000, Ordering::Release);
        let (_audio_in, audio) = RingBuffer::<f32>::new(2);
        let (_timing_in, timing) = RingBuffer::<TimingMarker>::new(TIMING_QUEUE_CAPACITY);
        let (events, _events_out) = RingBuffer::<RtEvent>::new(EVENT_QUEUE_CAPACITY);
        let output = OutputState::new(
            audio,
            timing,
            48_000,
            100,
            10.0,
            DEFAULT_QPC_FREQUENCY,
            events,
            shared,
            OutputLayout::STEREO,
            2,
        );
        let mut suffix = vec![1.0; 100 * 2];
        output.apply_fade_out(&mut suffix, 100);
        assert_eq!(suffix[(100 - 96) * 2 - 1], 1.0);
        assert_eq!(suffix[99 * 2], 0.0);
        assert_eq!(suffix[99 * 2 + 1], 0.0);

        let original = vec![0.25; 100 * 2];
        let mut unchanged = original.clone();
        let mut done = 0;
        apply_fade_in(&mut unchanged, 2, 48_000, 100, &mut done);
        assert_eq!(unchanged, original);
    }

    #[test]
    fn fault_transitions_emit_once_and_clear_only_the_matching_fault() {
        let shared = Arc::new(Shared::new());
        let (producer, mut consumer) = RingBuffer::new(8);
        let mut events = EventWriter::new(producer, Arc::clone(&shared));

        events.fault(FAULT_OUTPUT, RtEvent::OutputFault);
        events.fault(FAULT_OUTPUT, RtEvent::OutputFault);
        events.fault(FAULT_CAPTURE, RtEvent::CaptureFault);
        assert_eq!(
            shared.fault.load(Ordering::Acquire),
            FAULT_OUTPUT | FAULT_CAPTURE
        );
        assert!(matches!(consumer.pop(), Ok(RtEvent::OutputFault)));
        assert!(matches!(consumer.pop(), Ok(RtEvent::CaptureFault)));
        assert!(consumer.pop().is_err());

        events.clear_fault(FAULT_CAPTURE);
        assert_eq!(shared.fault.load(Ordering::Acquire), FAULT_OUTPUT);
        assert!(consumer.pop().is_err());
        events.clear_fault(FAULT_OUTPUT);
        assert_eq!(shared.fault.load(Ordering::Acquire), FAULT_NONE);
        assert!(matches!(consumer.pop(), Ok(RtEvent::FaultCleared)));
        assert!(consumer.pop().is_err());
    }

    #[test]
    fn zero_qpc_tail_marker_does_not_enter_latency_statistics() {
        let shared = Arc::new(Shared::new());
        shared.scene_rate.store(48_000, Ordering::Release);
        shared
            .clock_mode
            .store(ClockMode::Trim.as_u32(), Ordering::Release);
        let (mut audio_in, audio) = RingBuffer::<f32>::new(2);
        audio_in.push(0.5).unwrap();
        audio_in.push(0.5).unwrap();
        let (mut timing_in, timing) = RingBuffer::<TimingMarker>::new(TIMING_QUEUE_CAPACITY);
        timing_in
            .push(TimingMarker {
                stream_frame: 0,
                frames: 1,
                source_qpc: 0,
                rate: 48_000,
                layout: OutputLayout::STEREO,
            })
            .unwrap();
        let (events, _events_out) = RingBuffer::<RtEvent>::new(EVENT_QUEUE_CAPACITY);
        let mut output = OutputState::new(
            audio,
            timing,
            48_000,
            1,
            0.0,
            DEFAULT_QPC_FREQUENCY,
            events,
            Arc::clone(&shared),
            OutputLayout::STEREO,
            1,
        );
        output.strategy_mode = ClockMode::Trim;
        output.priming = false;
        output.fade_in_remaining = 0;
        let mut period = [0.0; 2];
        output.fill(&mut period);
        output.finish_period(DEFAULT_QPC_FREQUENCY);

        assert_eq!(shared.latency().samples, 0);
    }

    #[test]
    fn any_endpoint_epoch_requests_an_output_owned_latency_reset() {
        let shared = Arc::new(Shared::new());
        let (audio, _audio_out) = RingBuffer::<f32>::new(2);
        let (timing, _timing_out) = RingBuffer::<TimingMarker>::new(2);
        let (event_producer, _event_consumer) = RingBuffer::<RtEvent>::new(8);
        let mut events = EventWriter::new(event_producer, Arc::clone(&shared));
        let mut stream_frame = 0;
        let format = SourceFormat::Pcm(PcmFormat {
            channels: 2,
            sample_rate: 48_000,
            bits_per_sample: 32,
            valid_bits: 32,
            sample_type: SampleType::Float,
            channel_mask: 3,
        });
        let mut latency = LatencyWriter::new(&shared);
        latency.push(12.0, &shared);

        handle_chain_event(
            ChainEvent::FormatChanged {
                endpoint: 1,
                epoch: 1,
                format,
                support: crate::chain::FormatSupport::Supported,
            },
            &mut { audio },
            &mut { timing },
            &mut stream_frame,
            &mut events,
            &shared,
        );
        assert_eq!(shared.latency().samples, 1);
        assert_eq!(shared.latency_reset_requested.load(Ordering::Acquire), 1);
        latency.apply_requested_reset(&shared);
        assert_eq!(shared.latency().samples, 0);

        latency.push(13.0, &shared);
        let (mut audio, _audio_out) = RingBuffer::<f32>::new(2);
        let (mut timing, _timing_out) = RingBuffer::<TimingMarker>::new(2);
        handle_chain_event(
            ChainEvent::FormatChanged {
                endpoint: 0,
                epoch: 1,
                format,
                support: crate::chain::FormatSupport::Supported,
            },
            &mut audio,
            &mut timing,
            &mut stream_frame,
            &mut events,
            &shared,
        );
        assert_eq!(shared.latency().samples, 1);
        assert_eq!(shared.latency_reset_requested.load(Ordering::Acquire), 2);
        latency.apply_requested_reset(&shared);
        assert_eq!(shared.latency().samples, 0);
    }

    #[test]
    fn requested_reset_racing_with_output_reset_never_republishes_old_samples() {
        let shared = Shared::new();
        let mut writer = LatencyWriter::new(&shared);
        writer.push(91.0, &shared);
        shared.request_latency_reset();
        writer.reset(&shared);
        shared.request_latency_reset();
        writer.apply_requested_reset(&shared);
        writer.push(7.0, &shared);

        assert_eq!(
            shared.latency(),
            LatencyStats {
                p50_ms: 7.0,
                p99_ms: 7.0,
                samples: 1,
            }
        );
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
                codec: BitstreamCodec::Eac3Joc,
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
            let endpoint = epoch % ENGINE_ENDPOINTS;
            shared.publish_format(endpoint as u16, epoch as u32, format, UNSUPPORTED_NONE);
            assert_eq!(
                shared.endpoints[endpoint].read(),
                (Some(format), epoch as u32, UNSUPPORTED_NONE, 0.0, false)
            );
            assert_eq!(shared.status().endpoints[endpoint].format, Some(format));
        }
    }

    #[test]
    fn endpoint_format_support_and_volume_share_one_seqlock_generation() {
        let shared = Arc::new(Shared::new());
        let formats = [
            SourceFormat::Pcm(PcmFormat {
                channels: 2,
                sample_rate: 44_100,
                bits_per_sample: 32,
                valid_bits: 24,
                sample_type: SampleType::Float,
                channel_mask: 3,
            }),
            SourceFormat::Bitstream(BitstreamFormat {
                codec: BitstreamCodec::Other(0x9a7b_6c5d),
                container_rate: 192_000,
                container_channels: 8,
                encoded_rate: 96_000,
                encoded_channels: 0xfedc_ba98,
            }),
        ];
        shared.publish_format(2, 0, formats[0], UNSUPPORTED_NONE);
        let writer = Arc::clone(&shared);
        let handle = std::thread::spawn(move || {
            let mut state = 0x1234_5678_u32;
            for _ in 0..100_000 {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                let endpoint = &writer.endpoints[2];
                endpoint.format_seq.fetch_add(1, Ordering::AcqRel);
                let index = (state & 1) as usize;
                let (tag, a, b, c) = encode_format(formats[index]);
                endpoint.format_tag.store(tag, Ordering::Relaxed);
                endpoint.format_a.store(a, Ordering::Relaxed);
                endpoint.format_b.store(b, Ordering::Relaxed);
                endpoint.format_c.store(c, Ordering::Relaxed);
                endpoint.epoch.store(index as u32, Ordering::Relaxed);
                endpoint.unsupported.store(
                    if index == 0 {
                        UNSUPPORTED_NONE
                    } else {
                        UNSUPPORTED_FORMAT
                    },
                    Ordering::Relaxed,
                );
                endpoint.volume_bits.store(
                    if index == 0 { 0.0_f32 } else { -6.0_f32 }.to_bits(),
                    Ordering::Relaxed,
                );
                endpoint.muted.store(index != 0, Ordering::Relaxed);
                endpoint.format_seq.fetch_add(1, Ordering::Release);
            }
        });
        let mut state = 0x8765_4321_u32;
        for _ in 0..100_000 {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            if state & 7 == 0 {
                std::hint::spin_loop();
            }
            let observed = shared.endpoints[2].read();
            assert!(
                observed == (Some(formats[0]), 0, UNSUPPORTED_NONE, 0.0, false)
                    || observed == (Some(formats[1]), 1, UNSUPPORTED_FORMAT, -6.0, true,),
                "torn endpoint state: {observed:?}"
            );
        }
        handle.join().unwrap();
    }
}

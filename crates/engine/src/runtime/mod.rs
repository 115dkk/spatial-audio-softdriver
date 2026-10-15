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

mod clock;
mod crossing;
mod output;

use clock::*;
use crossing::{CrossingConsumer, CrossingProducer};
use output::*;

use std::cell::RefCell;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use rtrb::{Consumer, Producer, PushError, RingBuffer};
use ssr_types::{
    BitstreamCodec, BitstreamFormat, CaptureError, CaptureSource, ChainErrorCounts, ChainErrorKind,
    ClockStatus, ENGINE_ENDPOINTS, EndpointStatus, EngineFault, EngineStatus, LatencyStats,
    OutputError, OutputFormat, OutputLayout, OutputMode, OutputOpener, OutputSink, PcmFormat,
    SampleType, SourceFormat, UnderrunSnapshot, UnsupportedReason,
};

use crate::Chain;
use crate::chain::{ChainConfig, ChainEvent, Prepared};
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
const MUTE_RAMP_MS: f64 = 5.0;
const BOUNDARY_FADE_MS: f64 = 2.0;
const RECONNECT_INITIAL_MS: u64 = 250;
const RECONNECT_MAX_MS: u64 = 5_000;
// Deliveries land in the crossing ring whole (10 ms of PCM or MAT, 32 ms of
// E-AC-3 as several mixer blocks) while the output takes one period at a
// time, so the level is a sawtooth whose trough, just before the next
// delivery lands, is how close the output came to running dry. The controller
// holds that trough at target_ms plus what one late delivery costs a take
// (trough_setpoint_frames): it reads the lowest level left after each period
// over a window, and acts once per window, against a reference that moves from
// where priming left the trough to that setpoint (TroughController). The
// error is in milliseconds; P turns one millisecond into
// 60 ppm and I adds 3 ppm per millisecond-second. Larger gains chase the
// sawtooth itself (2,000 and 200 swung the ratio between the ±2,000 ppm limits
// on hardware). The clock tests, driving the real output with the mixer's
// 10 ms blocks, hold the trough within ±2 ms of the setpoint at ±300 ppm skew
// without reaching the limit when the period is shorter than a block; at a
// period of a block or longer the level moves in whole blocks and the tests
// require no underrun and no late saturation. A QPC-paced source against a
// USB DAC crystal is within about ±100 ppm, and the limit only bounds
// transients.
// The integral learns only within this many milliseconds of the setpoint; P
// alone drains larger errors (see PiController::update). With a period at
// least as long as a delivery the trough is flat between phase slips and
// steps by a whole delivery at each, so the skew shows only as those steps:
// the band holds one 10 ms block (PCM, MAT and the mixer's blocks) and 2 ms
// of jitter. At 6 ms the integral never learned a slip, P alone took about
// 20 s to climb back each time, and a late block in that time emptied the
// ring. At 16 ms it learned the E-AC-3 sawtooth at a 3 ms period as skew.
// An overflow burst reports at most one event per this many nanoseconds.
const OVERFLOW_REPORT_NS: u64 = 1_000_000_000;
// Priming waits for the largest delivery among this many recent capture steps
// that delivered audio (see ArrivalPeak). A DD+ sync frame is 3.2 mixer blocks,
// so deliveries run three blocks four times and then four; eight steps always
// hold a four-block one, and a one-off backlog is forgotten soon after.
const FAULT_NONE: u32 = 0;
const FAULT_CAPTURE: u32 = 1;
const FAULT_OUTPUT: u32 = 2;

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
        unsupported: Option<UnsupportedReason>,
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
    ring_capacity_frames
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
    let (crossing_producer, crossing_consumer) =
        crossing::channel(ring_capacity_frames, MAX_OUTPUT_CHANNELS);
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
                crossing_consumer,
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
                crossing_producer,
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
            unsupported: AtomicU32::new(0),
            volume_bits: AtomicU32::new(0.0_f32.to_bits()),
            muted: AtomicBool::new(false),
            chain_errors: std::array::from_fn(|_| AtomicU64::new(0)),
        }
    }

    fn publish(&self, epoch: u32, format: SourceFormat, unsupported: Option<UnsupportedReason>) {
        self.format_seq.fetch_add(1, Ordering::AcqRel);
        let (tag, a, b, c) = encode_format(format);
        self.format_tag.store(tag, Ordering::Relaxed);
        self.format_a.store(a, Ordering::Relaxed);
        self.format_b.store(b, Ordering::Relaxed);
        self.format_c.store(c, Ordering::Relaxed);
        self.epoch.store(epoch, Ordering::Relaxed);
        self.unsupported.store(
            unsupported.map_or(0, UnsupportedReason::code),
            Ordering::Relaxed,
        );
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

    fn read(
        &self,
    ) -> (
        Option<SourceFormat>,
        u32,
        Option<UnsupportedReason>,
        f32,
        bool,
    ) {
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
                    UnsupportedReason::from_code(unsupported),
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
    skipped_records: AtomicU64,
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
            skipped_records: AtomicU64::new(0),
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

    fn publish_format(
        &self,
        endpoint: u16,
        epoch: u32,
        format: SourceFormat,
        unsupported: Option<UnsupportedReason>,
    ) {
        if let Some(status) = self.endpoints.get(endpoint as usize) {
            status.publish(epoch, format, unsupported);
        }
    }

    fn publish_unsupported(&self, endpoint: u16, reason: UnsupportedReason) {
        if let Some(status) = self.endpoints.get(endpoint as usize) {
            status.format_seq.fetch_add(1, Ordering::AcqRel);
            status.unsupported.store(reason.code(), Ordering::Relaxed);
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
        let (format, epoch, unsupported, volume_db, muted) = status.read();
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
            skipped_records: self.skipped_records.load(Ordering::Acquire),
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
        unsupported: Option<UnsupportedReason>,
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
                unsupported,
            } => EngineEvent::FormatChanged {
                endpoint,
                epoch,
                format,
                unsupported,
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

fn requested_trim(shared: &Shared) -> f64 {
    if ClockMode::from_u32(shared.clock_mode.load(Ordering::Acquire)) == ClockMode::Trim {
        f64::from_bits(shared.trim_ppm_bits.load(Ordering::Acquire))
    } else {
        0.0
    }
}

fn apply_requested_trim<C: CaptureSource>(
    capture: &mut C,
    trim_supported: bool,
    shared: &Shared,
    applied_trim: &mut f64,
) {
    let requested_trim = requested_trim(shared);
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
    crossing: &mut CrossingProducer,
    shared: &Shared,
    chain: &mut Chain,
    events: &mut EventWriter,
    read_buffer: &mut [u8],
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
        let block_budget = crossing
            .audio_slots()
            .checked_div(block_samples)
            .unwrap_or(0)
            .min(crossing.timing_slots());
        return if chain.finish_step(block_budget, &mut |event| {
            handle_chain_event(event, crossing, events, shared);
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
        handle_chain_event(event, crossing, events, shared);
    });
    if let Some(qpc) = capture.now_qpc() {
        chain.idle(qpc, &mut |event| {
            handle_chain_event(event, crossing, events, shared);
        });
    }
    shared
        .lane_underrun_frames
        .store(chain.lane_underrun_frames(), Ordering::Release);
    shared
        .dropped_frames
        .store(chain.dropped_frames(), Ordering::Release);
    shared
        .skipped_records
        .store(chain.skipped_records(), Ordering::Release);
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
    mut crossing: CrossingProducer,
    shared: Arc<Shared>,
    mut chain: Chain,
) -> Option<Box<Prepared>> {
    if let Some(hook) = &config.thread_hook {
        hook(RealtimeThread::Capture);
    }
    let mut read_buffer = vec![0_u8; config.read_buffer_bytes];
    let mut events = EventWriter::new(event_producer, Arc::clone(&shared));
    let mut applied_trim = f64::NAN;
    let mut pending_retired: Option<Box<Prepared>> = None;
    let mut arrivals = ArrivalPeak::new();
    shared.capture_started.store(true, Ordering::Release);
    let mut finishing = false;
    while !shared.stop.load(Ordering::Acquire) {
        let mut step = None;
        let step_start = crossing.stream_frame();
        let mut work = || {
            step = Some(capture_rt_step(
                &mut capture,
                config.read_wait,
                trim_supported,
                &mut changes,
                &mut retired,
                &mut crossing,
                &shared,
                &mut chain,
                &mut events,
                &mut read_buffer,
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
        let delivered = crossing.stream_frame().saturating_sub(step_start);
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
        .skipped_records
        .store(chain.skipped_records(), Ordering::Release);
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
    crossing: &mut CrossingProducer,
    events: &mut EventWriter,
    shared: &Shared,
) {
    match event {
        ChainEvent::Block(block) => {
            crossing.push(block, events, shared);
        }
        ChainEvent::OutputChannels { .. } => {}
        ChainEvent::FormatChanged {
            endpoint,
            epoch,
            format,
            unsupported,
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
            shared.publish_format(endpoint, epoch, format, unsupported);
            events.push(RtEvent::FormatChanged {
                endpoint,
                epoch,
                format,
                unsupported,
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
        ChainEvent::Error {
            endpoint,
            error,
            repeat: _,
        } => {
            let kind = crate::chain::chain_error_kind(&error);
            shared.note_chain_error(endpoint, kind);
            if let (Some(endpoint), Some(reason)) = (endpoint, kind.unsupported_reason()) {
                shared.publish_unsupported(endpoint, reason);
            }
        }
    }
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
        output.reprime(Reprime::SinkRestarted);
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
    crossing: CrossingConsumer,
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
        crossing,
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
mod tests;

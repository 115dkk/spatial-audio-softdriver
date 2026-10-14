#![forbid(unsafe_code)]

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use brir::PreparedBrir;
use convolver::FilterBank;
use engine::{
    ClockPreference, EngineConfig, EngineEvent, LiveChange, Prepared, StartError, StartOptions,
    start_with, start_with_clock,
};
use speakers::{PreparedSpeaker, PreparedSpeakers};
use ssr_abi::{ENDPOINT_PRIMARY, RecordWriter, wave_format_bytes};
use ssr_types::{
    BitstreamCodec, BitstreamFormat, CaptureError, CaptureSource, ClockStatus, Direction,
    EngineFault, Layout, OutputError, OutputFormat, OutputLayout, OutputOpener, OutputSink,
    PcmFormat, SampleType, SourceFormat, TrimUnsupported,
};

const RATE: u32 = 48_000;
const BLOCK: usize = 480;
const QPC_FREQUENCY: u64 = 48_000;

fn pcm_format(rate: u32) -> SourceFormat {
    SourceFormat::Pcm(PcmFormat {
        channels: 2,
        sample_rate: rate,
        bits_per_sample: 32,
        valid_bits: 32,
        sample_type: SampleType::Float,
        channel_mask: 3,
    })
}

fn format_record_for(endpoint: u16, rate: u32) -> Vec<u8> {
    let mut record = vec![0; 256];
    let mut writer = RecordWriter::new(&mut record);
    writer.set_endpoint(endpoint);
    assert!(writer.format_change(1, 0, &wave_format_bytes(&pcm_format(rate))));
    let len = writer.len();
    record.truncate(len);
    record
}

fn format_record(rate: u32) -> Vec<u8> {
    format_record_for(ENDPOINT_PRIMARY, rate)
}

fn data_record(qpc: u64, value: f32) -> Vec<u8> {
    data_record_for(ENDPOINT_PRIMARY, qpc, value, -value, BLOCK)
}

fn tone_record(qpc: u64, start_frame: u64, frequency: f32) -> Vec<u8> {
    tone_record_frames_at_rate(qpc, start_frame, frequency, RATE, BLOCK)
}

fn tone_record_frames_at_rate(
    qpc: u64,
    start_frame: u64,
    frequency: f32,
    rate: u32,
    frames: usize,
) -> Vec<u8> {
    let mut pcm = vec![0_u8; frames * 2 * 4];
    for (offset, frame) in pcm.as_chunks_mut::<8>().0.iter_mut().enumerate() {
        let phase = 2.0 * std::f32::consts::PI * frequency * (start_frame + offset as u64) as f32
            / rate as f32;
        let sample = phase.sin();
        frame[..4].copy_from_slice(&sample.to_le_bytes());
        frame[4..].copy_from_slice(&sample.to_le_bytes());
    }
    bytes_record_for(ENDPOINT_PRIMARY, qpc, &pcm)
}

fn data_record_for(endpoint: u16, qpc: u64, left: f32, right: f32, frames: usize) -> Vec<u8> {
    let mut pcm = vec![0_u8; frames * 2 * 4];
    for frame in pcm.as_chunks_mut::<8>().0 {
        frame[..4].copy_from_slice(&left.to_le_bytes());
        frame[4..].copy_from_slice(&right.to_le_bytes());
    }
    bytes_record_for(endpoint, qpc, &pcm)
}

fn period_energy(samples: &[f32]) -> f64 {
    samples
        .iter()
        .map(|sample| f64::from(*sample) * f64::from(*sample))
        .sum()
}

fn boundary_left_frames(periods: &[&[f32]]) -> Vec<f32> {
    periods
        .iter()
        .flat_map(|period| period.as_chunks::<2>().0.iter().map(|frame| frame[0]))
        .collect()
}

fn assert_boundary_tail_is_continuous_and_silent_after_zero(periods: &[&[f32]], frequency: f32) {
    assert_boundary_frames_are_continuous_and_silent_after_zero(
        &boundary_left_frames(periods),
        frequency,
    );
}

fn assert_boundary_frames_are_continuous_and_silent_after_zero(frames: &[f32], frequency: f32) {
    let first_audible = frames
        .iter()
        .position(|sample| *sample != 0.0)
        .expect("old sink tail had no audible samples");
    let first_zero = frames[first_audible + 1..]
        .iter()
        .position(|sample| *sample == 0.0)
        .map(|offset| first_audible + 1 + offset)
        .expect("fade did not end at exact zero");
    assert!(
        frames[first_zero..].iter().all(|sample| *sample == 0.0),
        "old sink drain became audible again after reaching zero"
    );
    assert_tone_step_bound(frames, frequency);
}

fn assert_tone_step_bound(frames: &[f32], frequency: f32) {
    let tone_step = 2.0 * (std::f32::consts::PI * frequency / 44_100.0).sin();
    // The 256-tap ASRC can overshoot a full-scale sine slightly at the finite
    // input boundary; leave a small filter-ringing allowance while still being
    // far below the pre-fix zero-to-full-scale restart.
    let margin = 0.05;
    let max_step = frames
        .windows(2)
        .map(|pair| (pair[1] - pair[0]).abs())
        .fold(0.0_f32, f32::max);
    assert!(
        max_step <= tone_step + margin,
        "old sink discontinuity {max_step} exceeded tone bound {}",
        tone_step + margin
    );
}

fn bytes_record_for(endpoint: u16, qpc: u64, bytes: &[u8]) -> Vec<u8> {
    let mut record = vec![0; ssr_abi::record_size(bytes.len())];
    let mut writer = RecordWriter::new(&mut record);
    writer.set_endpoint(endpoint);
    assert!(writer.data(1, qpc * 8, qpc, bytes));
    let len = writer.len();
    record.truncate(len);
    record
}

fn stream_start_record(endpoint: u16) -> Vec<u8> {
    let mut record = vec![0; ssr_abi::record_size(0)];
    let mut writer = RecordWriter::new(&mut record);
    writer.set_endpoint(endpoint);
    assert!(writer.stream_start(1, 0));
    let len = writer.len();
    record.truncate(len);
    record
}

fn stream_stop_record(endpoint: u16) -> Vec<u8> {
    let mut record = vec![0; ssr_abi::record_size(0)];
    let mut writer = RecordWriter::new(&mut record);
    writer.set_endpoint(endpoint);
    assert!(writer.stream_stop(1, 0));
    let len = writer.len();
    record.truncate(len);
    record
}

fn ddp_format_record() -> Vec<u8> {
    let format = SourceFormat::Bitstream(BitstreamFormat {
        codec: BitstreamCodec::Eac3,
        container_rate: 192_000,
        container_channels: 2,
        encoded_rate: 48_000,
        encoded_channels: 6,
    });
    let mut record = vec![0; 256];
    let mut writer = RecordWriter::new(&mut record);
    assert!(writer.format_change(1, 0, &wave_format_bytes(&format)));
    let len = writer.len();
    record.truncate(len);
    record
}

fn speaker_prepared(channels: usize, channel_mask: u32) -> Box<Prepared> {
    let directions = (0..channels)
        .map(|channel| Direction::new(format!("ch{channel}"), channel as f32 * 10.0, 0.0))
        .collect();
    let speakers = (0..channels)
        .map(|channel| PreparedSpeaker {
            name: format!("ch{channel}"),
            device_channel: channel,
            small: false,
            crossover: None,
            delay_frames: 0,
            gain: 1.0,
            fir: None,
        })
        .collect();
    Box::new(
        Prepared::speakers(
            Arc::new(PreparedSpeakers {
                layout: Layout::new(directions).unwrap(),
                rate: RATE,
                partition: BLOCK,
                device_channels: channels,
                channel_mask,
                speakers,
                subwoofers: Vec::new(),
                bass_to: Vec::new(),
                lfe_gain: 10.0_f32.sqrt(),
                gain: 1.0,
                limiter_ceiling: 1.0,
            }),
            BLOCK,
        )
        .unwrap(),
    )
}

fn prepared(rate: u32) -> Box<Prepared> {
    prepared_with_ir(rate, &[1.0])
}

fn prepared_with_ir(rate: u32, ir: &[f32]) -> Box<Prepared> {
    let filters = [[ir, ir]];
    let brir = Arc::new(PreparedBrir {
        layout: Layout::new(vec![Direction::new("front", 0.0, 0.0)]).unwrap(),
        bank: Arc::new(FilterBank::new(&filters, BLOCK).unwrap()),
        rate,
        partition: BLOCK,
        lfe: None,
        gain: 1.0,
        limiter_ceiling: 1.0,
        ir_frames: ir.len(),
        omitted_silent_directions: Vec::new(),
        trimmed_leading_frames: 0,
    });
    Box::new(Prepared::new(brir, BLOCK).unwrap())
}

struct RecordsCapture {
    records: Vec<Vec<u8>>,
    next: usize,
    trim_supported: bool,
    trims: Option<Arc<Mutex<Vec<f64>>>>,
    gate: Option<Arc<AtomicBool>>,
    /// Without it every record arrives at once, the output's priming drops
    /// what lies beyond target + one delivery, and QPCs written in advance no
    /// longer match when the remaining blocks play.
    paced_by: Option<Pacing>,
    lockstep: Option<Arc<Lockstep>>,
    /// Return an empty live read instead of ending after the fixture records.
    stay_open: bool,
}

/// Hands out records like a live source, at most `lead` records ahead of the
/// sink's periods, and stamps each data record with the QPC of its last byte,
/// as the ABI requires (these tests count QPC in frames, `QPC_FREQUENCY` = `RATE`).
struct Pacing {
    state: Arc<SinkState>,
    lead: u64,
    scene_rate: u32,
}

#[derive(Debug, Default)]
struct Steps {
    read_allowed: u64,
    read_started: u64,
    read_completed: u64,
    period_allowed: u64,
    period_started: u64,
    period_completed: u64,
    capture_phases_completed: u64,
    capture_finished: bool,
    qpc: u64,
    released: bool,
}

#[derive(Default)]
struct Lockstep {
    steps: Mutex<Steps>,
    changed: Condvar,
}

impl Lockstep {
    fn step_read(&self, count: u64) {
        let mut steps = self.steps.lock().unwrap();
        let target = steps.read_completed + count;
        steps.read_allowed += count;
        self.changed.notify_all();
        while steps.read_completed < target {
            let (next, timeout) = self
                .changed
                .wait_timeout(steps, Duration::from_secs(5))
                .unwrap();
            steps = next;
            if timeout.timed_out() {
                let message = format!("capture did not complete read {target}: {steps:?}");
                steps.released = true;
                self.changed.notify_all();
                drop(steps);
                panic!("{message}");
            }
        }
    }

    fn start_read(&self, count: u64) {
        let mut steps = self.steps.lock().unwrap();
        let target = steps.read_started + count;
        steps.read_allowed += count;
        self.changed.notify_all();
        while steps.read_started < target {
            let (next, timeout) = self
                .changed
                .wait_timeout(steps, Duration::from_secs(5))
                .unwrap();
            steps = next;
            if timeout.timed_out() {
                let message = format!("capture did not start read {target}: {steps:?}");
                steps.released = true;
                self.changed.notify_all();
                drop(steps);
                panic!("{message}");
            }
        }
    }

    fn step_period(&self, count: u64) {
        let mut steps = self.steps.lock().unwrap();
        let target = steps.period_completed + count;
        steps.period_allowed += count;
        self.changed.notify_all();
        while steps.period_completed < target {
            let (next, timeout) = self
                .changed
                .wait_timeout(steps, Duration::from_secs(5))
                .unwrap();
            steps = next;
            if timeout.timed_out() {
                let message = format!("output did not complete period {target}: {steps:?}");
                steps.released = true;
                self.changed.notify_all();
                drop(steps);
                panic!("{message}");
            }
        }
    }

    fn start_period(&self, count: u64) {
        let mut steps = self.steps.lock().unwrap();
        let target = steps.period_started + count;
        steps.period_allowed += count;
        self.changed.notify_all();
        while steps.period_started < target {
            let (next, timeout) = self
                .changed
                .wait_timeout(steps, Duration::from_secs(5))
                .unwrap();
            steps = next;
            if timeout.timed_out() {
                let message = format!("output did not start period {target}: {steps:?}");
                steps.released = true;
                self.changed.notify_all();
                drop(steps);
                panic!("{message}");
            }
        }
    }

    /// Completes the preceding read at the next `read` entry, after the runtime
    /// has consumed its records, then waits for permission to begin one read.
    fn enter_read(&self) {
        let mut steps = self.steps.lock().unwrap();
        if steps.read_completed < steps.read_started {
            steps.read_completed += 1;
            self.changed.notify_all();
        }
        while !steps.released && steps.read_started == steps.read_allowed {
            steps = self.changed.wait(steps).unwrap();
        }
        steps.read_started += 1;
        self.changed.notify_all();
    }

    fn advance_read_clock(&self, frames: u64) {
        self.steps.lock().unwrap().qpc += frames;
    }

    fn enter_period(&self, frames: u64) -> u64 {
        let mut steps = self.steps.lock().unwrap();
        while !steps.released && steps.period_started == steps.period_allowed {
            steps = self.changed.wait(steps).unwrap();
        }
        let qpc = steps.qpc;
        steps.period_started += 1;
        steps.qpc += frames;
        qpc
    }

    fn complete_period(&self) {
        let mut steps = self.steps.lock().unwrap();
        steps.period_completed += 1;
        self.changed.notify_all();
    }

    fn complete_capture_phase(&self) {
        let mut steps = self.steps.lock().unwrap();
        steps.capture_phases_completed += 1;
        self.changed.notify_all();
    }

    fn finish_capture(&self) {
        let mut steps = self.steps.lock().unwrap();
        steps.capture_finished = true;
        self.changed.notify_all();
    }

    /// Waits until the capture thread has stopped, so everything it will put
    /// into the crossing ring is already there.
    fn wait_capture_finished(&self) {
        let mut steps = self.steps.lock().unwrap();
        while !steps.capture_finished {
            let (next, timeout) = self
                .changed
                .wait_timeout(steps, Duration::from_secs(5))
                .unwrap();
            steps = next;
            if timeout.timed_out() {
                let message = format!("capture did not finish: {steps:?}");
                steps.released = true;
                self.changed.notify_all();
                drop(steps);
                panic!("{message}");
            }
        }
    }

    fn release(&self) {
        let mut steps = self.steps.lock().unwrap();
        steps.released = true;
        self.changed.notify_all();
    }

    fn signal_output_failure(&self, observed: &AtomicBool) {
        let _steps = self.steps.lock().unwrap();
        observed.store(true, Ordering::Release);
        self.changed.notify_all();
    }

    fn wait_output_failure(&self, observed: &AtomicBool) {
        let mut steps = self.steps.lock().unwrap();
        while !observed.load(Ordering::Acquire) {
            let (next, timeout) = self
                .changed
                .wait_timeout(steps, Duration::from_secs(5))
                .unwrap();
            steps = next;
            if timeout.timed_out() {
                let message = format!("output did not report its injected failure: {steps:?}");
                steps.released = true;
                self.changed.notify_all();
                drop(steps);
                panic!("{message}");
            }
        }
    }

    fn now_qpc(&self) -> u64 {
        self.steps.lock().unwrap().qpc
    }
}

/// Waits for a thread to leave on its own (here: the output thread stopping
/// its sink once EOF is drained). Thread exit is not driven by the lockstep
/// clock, so this one wait is on wall time; audio progress never is.
fn wait_for_thread_exit(timeout: Duration, mut exited: impl FnMut() -> bool) {
    let start = Instant::now();
    while !exited() {
        assert!(start.elapsed() < timeout, "thread did not exit");
        std::thread::yield_now();
    }
}

const RECORD_QPC: usize = std::mem::offset_of!(ssr_abi::RecordHeader, qpc);
const RECORD_KIND: usize = std::mem::offset_of!(ssr_abi::RecordHeader, kind);
const RECORD_LEN: usize = std::mem::offset_of!(ssr_abi::RecordHeader, len);

struct ReconnectingCapture {
    inner: RecordsCapture,
    fail_at: usize,
    lost: bool,
    reconnects: Arc<AtomicU64>,
}

impl CaptureSource for ReconnectingCapture {
    fn read(&mut self, buffer: &mut [u8], wait: Duration) -> Result<usize, CaptureError> {
        if !self.lost && self.inner.next == self.fail_at {
            self.lost = true;
            return Err(CaptureError::NotPresent);
        }
        self.inner.read(buffer, wait)
    }

    fn now_qpc(&self) -> Option<u64> {
        self.inner.now_qpc()
    }

    fn set_rate_trim(&mut self, ppm: f64) -> Result<(), TrimUnsupported> {
        self.inner.set_rate_trim(ppm)
    }

    fn reconnect(&mut self) -> Result<(), CaptureError> {
        self.reconnects.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
}

impl CaptureSource for RecordsCapture {
    fn read(&mut self, buffer: &mut [u8], _wait: Duration) -> Result<usize, CaptureError> {
        if let Some(lockstep) = &self.lockstep {
            lockstep.enter_read();
        }
        if self
            .gate
            .as_ref()
            .is_some_and(|gate| !gate.load(Ordering::Acquire))
        {
            std::thread::yield_now();
            return Ok(0);
        }
        if self.paced_by.as_ref().is_some_and(|pacing| {
            self.next as u64 > pacing.state.periods.load(Ordering::Acquire) + pacing.lead
        }) {
            std::thread::yield_now();
            return Ok(0);
        }
        let Some(record) = self.records.get(self.next) else {
            if self.stay_open {
                return Ok(0);
            }
            return Err(CaptureError::Ended);
        };
        assert!(record.len() <= buffer.len());
        buffer[..record.len()].copy_from_slice(record);
        if let Some(pacing) = &self.paced_by {
            let now = pacing.state.frames.load(Ordering::Acquire);
            let kind = u16::from_le_bytes(buffer[RECORD_KIND..RECORD_KIND + 2].try_into().unwrap());
            let bytes = u32::from_le_bytes(buffer[RECORD_LEN..RECORD_LEN + 4].try_into().unwrap());
            // Data records in this harness use stereo f32 PCM at the pacing
            // scene rate or a 192 kHz stereo i16 IEC carrier. Convert both to
            // this synthetic clock's 48 kHz QPC units.
            let record_frames = match kind {
                ssr_abi::RECORD_KIND_DATA if bytes % 8 == 0 && bytes <= (BLOCK * 16) as u32 => {
                    u64::from(bytes / 8) * u64::from(RATE) / u64::from(pacing.scene_rate)
                }
                ssr_abi::RECORD_KIND_DATA => u64::from(bytes) * RATE as u64 / 768_000,
                _ => 0,
            };
            let qpc = now.saturating_add(record_frames);
            buffer[RECORD_QPC..RECORD_QPC + 8].copy_from_slice(&qpc.to_le_bytes());
        }
        let frames = if record.len() >= RECORD_LEN + 4 {
            let kind = u16::from_le_bytes(record[RECORD_KIND..RECORD_KIND + 2].try_into().unwrap());
            let bytes = u32::from_le_bytes(record[RECORD_LEN..RECORD_LEN + 4].try_into().unwrap());
            if kind == ssr_abi::RECORD_KIND_DATA {
                self.paced_by.as_ref().map_or_else(
                    || u64::from(bytes / 8),
                    |pacing| u64::from(bytes / 8) * u64::from(RATE) / u64::from(pacing.scene_rate),
                )
            } else {
                0
            }
        } else {
            0
        };
        self.next += 1;
        if let Some(lockstep) = &self.lockstep {
            lockstep.advance_read_clock(frames);
        }
        Ok(record.len())
    }

    fn now_qpc(&self) -> Option<u64> {
        self.lockstep.as_ref().map(|lockstep| lockstep.now_qpc())
    }

    fn set_rate_trim(&mut self, ppm: f64) -> Result<(), TrimUnsupported> {
        if self.trim_supported {
            if let Some(trims) = &self.trims {
                trims.lock().unwrap().push(ppm);
            }
            Ok(())
        } else {
            Err(TrimUnsupported)
        }
    }
}

#[derive(Clone)]
struct TestOpener {
    state: Arc<SinkState>,
    format: OutputFormat,
    reopen_format: Option<OutputFormat>,
    fail: bool,
    fail_on_open: Option<usize>,
    fail_after_first: bool,
    opens: usize,
    honor_layout: bool,
}

struct SinkState {
    lockstep: Option<Arc<Lockstep>>,
    frames: AtomicU64,
    periods: AtomicU64,
    samples: Mutex<Vec<f32>>,
    periods_by_layout: Mutex<Vec<(OutputLayout, Vec<f32>)>>,
    opened_layouts: Mutex<Vec<OutputLayout>>,
    opened_formats: Mutex<Vec<OutputFormat>>,
    requested_periods: Mutex<Vec<Duration>>,
    inject_cadence_tail: AtomicBool,
    inject_cadence_tail_enabled: AtomicBool,
    stopped: AtomicBool,
    delay_frames: u64,
    max_periods: AtomicU64,
    fail_from_period: AtomicU64,
    failure_observed: AtomicBool,
    played_numerator: AtomicU64,
    played_denominator: AtomicU64,
}

impl SinkState {
    fn new(delay_frames: u64) -> Arc<Self> {
        Self::with_lockstep(delay_frames, None)
    }

    fn lockstep(delay_frames: u64, lockstep: Arc<Lockstep>) -> Arc<Self> {
        Self::with_lockstep(delay_frames, Some(lockstep))
    }

    fn with_lockstep(delay_frames: u64, lockstep: Option<Arc<Lockstep>>) -> Arc<Self> {
        Arc::new(Self {
            lockstep,
            frames: AtomicU64::new(0),
            periods: AtomicU64::new(0),
            samples: Mutex::new(Vec::new()),
            periods_by_layout: Mutex::new(Vec::new()),
            opened_layouts: Mutex::new(Vec::new()),
            opened_formats: Mutex::new(Vec::new()),
            requested_periods: Mutex::new(Vec::new()),
            inject_cadence_tail: AtomicBool::new(false),
            inject_cadence_tail_enabled: AtomicBool::new(false),
            stopped: AtomicBool::new(false),
            delay_frames,
            max_periods: AtomicU64::new(u64::MAX),
            fail_from_period: AtomicU64::new(u64::MAX),
            failure_observed: AtomicBool::new(false),
            played_numerator: AtomicU64::new(1),
            played_denominator: AtomicU64::new(0),
        })
    }
}

impl OutputOpener for TestOpener {
    type Sink = TestSink;

    fn open(&mut self, layout: OutputLayout) -> Result<Self::Sink, OutputError> {
        self.opens += 1;
        self.state.opened_layouts.lock().unwrap().push(layout);
        if self.fail
            || self.fail_on_open == Some(self.opens)
            || (self.fail_after_first && self.opens > 1)
        {
            return Err(OutputError::NotFound);
        }
        let selected = if self.opens > 1 {
            self.reopen_format.unwrap_or(self.format)
        } else {
            self.format
        };
        let format = if self.honor_layout {
            OutputFormat {
                channels: layout.channels,
                channel_mask: layout.channel_mask,
                ..selected
            }
        } else {
            selected
        };
        self.state.opened_formats.lock().unwrap().push(format);
        Ok(TestSink {
            state: Arc::clone(&self.state),
            format,
            buffer: vec![0.0; format.period_frames * usize::from(format.channels)],
        })
    }

    fn set_period(&mut self, period: Duration) -> bool {
        self.state.requested_periods.lock().unwrap().push(period);
        if self
            .state
            .inject_cadence_tail_enabled
            .load(Ordering::Acquire)
        {
            self.state
                .inject_cadence_tail
                .store(true, Ordering::Release);
        }
        self.format.period_frames =
            (period.as_secs_f64() * f64::from(self.format.rate)).round() as usize;
        true
    }
}

struct TestSink {
    state: Arc<SinkState>,
    format: OutputFormat,
    buffer: Vec<f32>,
}

impl OutputSink for TestSink {
    fn format(&self) -> OutputFormat {
        self.format
    }

    fn next_period(&mut self, fill: &mut dyn FnMut(&mut [f32])) -> Result<u64, OutputError> {
        let periods = self.state.periods.load(Ordering::Acquire);
        if periods >= self.state.fail_from_period.load(Ordering::Acquire) {
            if let Some(lockstep) = &self.state.lockstep {
                let _ = lockstep.enter_period(self.format.period_frames as u64);
                lockstep.signal_output_failure(&self.state.failure_observed);
            } else {
                self.state.failure_observed.store(true, Ordering::Release);
            }
            return Err(OutputError::Removed);
        }
        if periods >= self.state.max_periods.load(Ordering::Acquire) {
            return Err(OutputError::Removed);
        }
        let playback_qpc = self.state.lockstep.as_ref().map_or_else(
            || self.state.frames.load(Ordering::Acquire),
            |lockstep| lockstep.enter_period(self.format.period_frames as u64),
        );
        fill(&mut self.buffer);
        if self.state.inject_cadence_tail.swap(false, Ordering::AcqRel) {
            let frames = self.buffer.len() / usize::from(self.format.channels);
            for (index, frame) in self
                .buffer
                .chunks_exact_mut(usize::from(self.format.channels))
                .enumerate()
            {
                let sample = (2.0 * std::f32::consts::PI * 997.0 * index as f32
                    / self.format.rate as f32)
                    .sin();
                for channel in frame {
                    *channel = sample;
                }
            }
            debug_assert_eq!(frames, self.format.period_frames);
        }
        self.state
            .samples
            .lock()
            .unwrap()
            .extend_from_slice(&self.buffer);
        self.state.periods_by_layout.lock().unwrap().push((
            OutputLayout {
                channels: self.format.channels,
                channel_mask: self.format.channel_mask,
            },
            self.buffer.clone(),
        ));
        self.state
            .frames
            .fetch_add(self.format.period_frames as u64, Ordering::AcqRel);
        self.state.periods.fetch_add(1, Ordering::Release);
        if self.state.lockstep.is_none() {
            std::thread::yield_now();
        }
        Ok(playback_qpc + self.state.delay_frames)
    }

    fn played_frames(&self) -> Option<u64> {
        let denominator = self.state.played_denominator.load(Ordering::Acquire);
        if denominator == 0 {
            return None;
        }
        let frames = self.state.frames.load(Ordering::Acquire);
        let numerator = self.state.played_numerator.load(Ordering::Acquire);
        Some(frames.saturating_mul(numerator) / denominator)
    }
}

impl Drop for TestSink {
    fn drop(&mut self) {
        self.state.stopped.store(true, Ordering::Release);
    }
}

fn config(clock: ClockPreference) -> EngineConfig {
    EngineConfig {
        clock,
        target_ms: 10.0,
        capacity_ms: 80.0,
        read_wait: Duration::ZERO,
        ..EngineConfig::default()
    }
}

fn lockstep_config(clock: ClockPreference, lockstep: Arc<Lockstep>) -> EngineConfig {
    EngineConfig {
        rt_event: Some(Arc::new(move |event| match event {
            engine::RtLoopEvent::CaptureEnd => lockstep.complete_capture_phase(),
            engine::RtLoopEvent::OutputEnd => lockstep.complete_period(),
            engine::RtLoopEvent::CaptureFinished => lockstep.finish_capture(),
            _ => {}
        })),
        ..config(clock)
    }
}

fn opener(state: Arc<SinkState>, rate: u32) -> TestOpener {
    opener_with_period(state, rate, BLOCK)
}

fn opener_with_period(state: Arc<SinkState>, rate: u32, period_frames: usize) -> TestOpener {
    TestOpener {
        state,
        format: OutputFormat {
            rate,
            channels: 2,
            channel_mask: 0x3,
            period_frames,
            mode: ssr_types::OutputMode::Exclusive,
            fell_back: false,
            mix_channels: 2,
        },
        reopen_format: None,
        fail: false,
        fail_on_open: None,
        fail_after_first: false,
        opens: 0,
        honor_layout: true,
    }
}

#[test]
fn output_open_failure_is_returned_from_start() {
    let state = SinkState::new(0);
    let capture = RecordsCapture {
        records: Vec::new(),
        next: 0,
        trim_supported: false,
        trims: None,
        gate: None,
        paced_by: None,
        lockstep: None,
        stay_open: false,
    };
    let mut output = opener(state, RATE);
    output.fail = true;
    assert!(matches!(
        start_with_clock(
            config(ClockPreference::Asrc),
            capture,
            output,
            QPC_FREQUENCY
        ),
        Err(StartError::Output(OutputError::NotFound))
    ));
}

#[test]
fn default_start_options_use_the_default_qpc_frequency() {
    let state = SinkState::new(0);
    let capture = RecordsCapture {
        records: Vec::new(),
        next: 0,
        trim_supported: false,
        trims: None,
        gate: None,
        paced_by: None,
        lockstep: None,
        stay_open: false,
    };
    let engine = start_with(
        config(ClockPreference::Asrc),
        capture,
        opener(state, RATE),
        StartOptions::default(),
    )
    .unwrap();
    let _ = engine.stop();
}

#[test]
fn auto_selects_trim_only_when_capture_and_output_support_it() {
    for (trim_supported, output_rate, expected) in [
        (true, RATE, ClockStatus::Trim),
        (false, RATE, ClockStatus::Asrc),
        (true, 44_100, ClockStatus::Asrc),
    ] {
        let state = SinkState::new(0);
        let capture = RecordsCapture {
            records: Vec::new(),
            next: 0,
            trim_supported,
            trims: None,
            gate: None,
            paced_by: None,
            lockstep: None,
            stay_open: true,
        };
        let engine = start_with_clock(
            config(ClockPreference::Auto),
            capture,
            opener(state, output_rate),
            QPC_FREQUENCY,
        )
        .unwrap();
        assert_eq!(engine.status().clock, expected);
        assert_eq!(engine.status().clock, engine.stop().final_status.clock);
    }
}

#[test]
fn format_change_does_not_change_the_startup_clock_or_mix_rate() {
    let lockstep = Arc::new(Lockstep::default());
    let state = SinkState::lockstep(0, Arc::clone(&lockstep));
    let capture = RecordsCapture {
        records: vec![format_record(96_000)],
        next: 0,
        trim_supported: true,
        trims: None,
        gate: None,
        paced_by: None,
        lockstep: Some(Arc::clone(&lockstep)),
        stay_open: true,
    };
    let engine = start_with_clock(
        lockstep_config(ClockPreference::Auto, Arc::clone(&lockstep)),
        capture,
        opener(state, RATE),
        QPC_FREQUENCY,
    )
    .unwrap();
    lockstep.step_read(2);
    let status = engine.status();
    assert_eq!(status.clock, ClockStatus::Trim);
    assert_eq!(status.endpoints[0].format, Some(pcm_format(96_000)));
    lockstep.release();
    let _ = engine.stop();
}

#[test]
fn required_trim_rejects_an_unsupported_capture() {
    let state = SinkState::new(0);
    let capture = RecordsCapture {
        records: Vec::new(),
        next: 0,
        trim_supported: false,
        trims: None,
        gate: None,
        paced_by: None,
        lockstep: None,
        stay_open: false,
    };
    assert!(matches!(
        start_with_clock(
            config(ClockPreference::Trim),
            capture,
            opener(state, RATE),
            QPC_FREQUENCY
        ),
        Err(StartError::TrimUnsupported)
    ));
}

#[test]
fn required_trim_rejects_a_non_mix_rate_output_before_any_endpoint_data() {
    let state = SinkState::new(0);
    let capture = RecordsCapture {
        records: Vec::new(),
        next: 0,
        trim_supported: true,
        trims: None,
        gate: None,
        paced_by: None,
        lockstep: None,
        stay_open: false,
    };
    assert!(matches!(
        start_with_clock(
            config(ClockPreference::Trim),
            capture,
            opener(state, 96_000),
            QPC_FREQUENCY
        ),
        Err(StartError::Config(_))
    ));
}

#[test]
fn format_and_needs_brir_events_reach_poll() {
    let lockstep = Arc::new(Lockstep::default());
    let state = SinkState::lockstep(0, Arc::clone(&lockstep));
    let capture = RecordsCapture {
        records: vec![
            format_record(RATE),
            data_record(0, 0.25),
            data_record(BLOCK as u64, 0.25),
            data_record((2 * BLOCK) as u64, 0.25),
        ],
        next: 0,
        trim_supported: false,
        trims: None,
        gate: None,
        paced_by: None,
        lockstep: Some(Arc::clone(&lockstep)),
        stay_open: false,
    };
    let mut engine = start_with_clock(
        lockstep_config(ClockPreference::Asrc, Arc::clone(&lockstep)),
        capture,
        opener(state, RATE),
        QPC_FREQUENCY,
    )
    .unwrap();
    lockstep.step_read(2);
    let events = engine.poll();
    assert!(events.iter().any(|event| matches!(
        event,
        EngineEvent::FormatChanged {
            endpoint: ENDPOINT_PRIMARY,
            epoch: 1,
            format,
        } if *format == pcm_format(RATE)
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        EngineEvent::NeedsBrir {
            rate: RATE,
            block: BLOCK
        }
    )));
    lockstep.release();
    let report = engine.stop();
    assert_eq!(report.blocks, 0);
}

#[test]
fn mixer_statistics_are_visible_before_capture_stops() {
    let lockstep = Arc::new(Lockstep::default());
    let state = SinkState::lockstep(0, Arc::clone(&lockstep));
    let capture = RecordsCapture {
        records: vec![
            format_record(RATE),
            format_record_for(1, RATE),
            stream_start_record(ENDPOINT_PRIMARY),
            stream_start_record(1),
            data_record_for(ENDPOINT_PRIMARY, BLOCK as u64, 0.25, -0.25, BLOCK),
            data_record_for(1, BLOCK as u64, 0.5, -0.5, BLOCK),
            data_record_for(ENDPOINT_PRIMARY, 2 * BLOCK as u64, 0.25, -0.25, BLOCK),
            data_record_for(ENDPOINT_PRIMARY, 3 * BLOCK as u64, 0.25, -0.25, BLOCK),
        ],
        next: 0,
        trim_supported: false,
        trims: None,
        gate: None,
        paced_by: None,
        lockstep: Some(Arc::clone(&lockstep)),
        stay_open: true,
    };
    let engine = start_with(
        lockstep_config(ClockPreference::Asrc, Arc::clone(&lockstep)),
        capture,
        opener(state, RATE),
        StartOptions {
            qpc_frequency: QPC_FREQUENCY,
            prepared: Some(prepared(RATE)),
        },
    )
    .unwrap();
    lockstep.step_read(9);
    for _ in 0..6 {
        lockstep.step_read(1);
        lockstep.step_period(1);
        if engine.status().lane_underrun_frames >= BLOCK as u64 {
            break;
        }
    }
    let status = engine.status();
    assert!(status.lane_underrun_frames >= BLOCK as u64);
    assert!(status.fault.is_none());
    lockstep.release();
    let _ = engine.stop();
}

#[test]
fn unsupported_format_is_counted_without_changing_endpoint_fault_semantics() {
    let lockstep = Arc::new(Lockstep::default());
    let state = SinkState::lockstep(0, Arc::clone(&lockstep));
    let capture = RecordsCapture {
        records: vec![format_record(RATE), format_record_for(1, 32_000)],
        next: 0,
        trim_supported: false,
        trims: None,
        gate: None,
        paced_by: None,
        lockstep: Some(Arc::clone(&lockstep)),
        stay_open: true,
    };
    let mut engine = start_with_clock(
        lockstep_config(ClockPreference::Asrc, Arc::clone(&lockstep)),
        capture,
        opener(state, RATE),
        QPC_FREQUENCY,
    )
    .unwrap();
    lockstep.step_read(3);
    let status = engine.status();
    assert!(status.fault.is_none());
    assert!(status.endpoints[1].unsupported.is_some());
    assert_eq!(
        status.endpoints[1]
            .chain_errors
            .get(ssr_types::ChainErrorKind::UnsupportedSceneRate),
        1
    );
    let events = engine.poll();
    assert!(events.iter().any(|event| matches!(
        event,
        EngineEvent::FormatChanged {
            endpoint: 1,
            epoch: 1,
            format,
        } if *format == pcm_format(32_000)
    )));
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, EngineEvent::Fault(_)))
    );
    lockstep.release();
    let report = engine.stop();
    assert_eq!(
        report.final_status.endpoints[1]
            .chain_errors
            .get(ssr_types::ChainErrorKind::UnsupportedSceneRate),
        1
    );
    assert_eq!(
        report.final_status.endpoints[1]
            .chain_errors
            .get(ssr_types::ChainErrorKind::UnsupportedSceneRate),
        1
    );
    assert!(report.final_status.endpoints[1].unsupported.is_some());
}

#[test]
fn malformed_records_are_counted_by_endpoint_and_without_attribution() {
    let lockstep = Arc::new(Lockstep::default());
    let state = SinkState::lockstep(0, Arc::clone(&lockstep));
    let mut bad_length = vec![0; ssr_abi::record_size(1)];
    let mut writer = RecordWriter::new(&mut bad_length);
    writer.set_endpoint(1);
    assert!(writer.stream_stop(1, 0));
    let length = writer.len();
    bad_length.resize(length + 1, 0);
    bad_length[32..36].copy_from_slice(&1_u32.to_le_bytes());
    let truncated_header = vec![0; std::mem::size_of::<ssr_abi::RecordHeader>() - 1];
    let capture = RecordsCapture {
        records: vec![bad_length, truncated_header],
        next: 0,
        trim_supported: false,
        trims: None,
        gate: None,
        paced_by: None,
        lockstep: Some(Arc::clone(&lockstep)),
        stay_open: true,
    };
    let engine = start_with_clock(
        lockstep_config(ClockPreference::Asrc, Arc::clone(&lockstep)),
        capture,
        opener(state, RATE),
        QPC_FREQUENCY,
    )
    .unwrap();

    lockstep.step_read(3);
    let status = engine.status();
    assert_eq!(
        status.endpoints[1]
            .chain_errors
            .get(ssr_types::ChainErrorKind::BadRecordLength),
        1
    );
    assert_eq!(
        status
            .unattributed_chain_errors
            .get(ssr_types::ChainErrorKind::TruncatedRecord),
        1
    );
    assert_eq!(status.endpoints[1].unsupported, None);
    assert!(status.fault.is_none());
    lockstep.release();
    let report = engine.stop();
    assert_eq!(
        report.final_status.endpoints[1]
            .chain_errors
            .get(ssr_types::ChainErrorKind::BadRecordLength),
        1
    );
    assert_eq!(
        report
            .final_status
            .unattributed_chain_errors
            .get(ssr_types::ChainErrorKind::TruncatedRecord),
        1
    );
}

#[test]
fn corrupt_payload_is_counted_without_marking_the_endpoint_unsupported() {
    let lockstep = Arc::new(Lockstep::default());
    let state = SinkState::lockstep(0, Arc::clone(&lockstep));
    let mut corrupt = Vec::new();
    bitstream::frame_burst(bitstream::DataType::Eac3, &[0; 7], 24_576, &mut corrupt);
    let capture = RecordsCapture {
        records: vec![
            ddp_format_record(),
            bytes_record_for(ENDPOINT_PRIMARY, 0, &corrupt),
        ],
        next: 0,
        trim_supported: false,
        trims: None,
        gate: None,
        paced_by: None,
        lockstep: Some(Arc::clone(&lockstep)),
        stay_open: true,
    };
    let engine = start_with_clock(
        lockstep_config(ClockPreference::Asrc, Arc::clone(&lockstep)),
        capture,
        opener(state, RATE),
        QPC_FREQUENCY,
    )
    .unwrap();

    lockstep.step_read(3);
    let status = engine.status();
    assert_eq!(
        status.endpoints[0]
            .chain_errors
            .get(ssr_types::ChainErrorKind::CorruptPayload),
        1
    );
    assert_eq!(status.endpoints[0].unsupported, None);
    assert!(status.fault.is_none());
    lockstep.release();
    let report = engine.stop();
    assert_eq!(
        report.final_status.endpoints[0]
            .chain_errors
            .get(ssr_types::ChainErrorKind::CorruptPayload),
        1
    );
}

#[test]
fn zero_mask_pcm_without_a_direct_layout_reports_that_reason() {
    let lockstep = Arc::new(Lockstep::default());
    let state = SinkState::lockstep(0, Arc::clone(&lockstep));
    let mut record = vec![0; 256];
    let mut writer = RecordWriter::new(&mut record);
    let eleven = SourceFormat::Pcm(PcmFormat {
        channels: 11,
        sample_rate: RATE,
        bits_per_sample: 32,
        valid_bits: 32,
        sample_type: SampleType::Float,
        channel_mask: 0,
    });
    assert!(writer.format_change(1, 0, &wave_format_bytes(&eleven)));
    let len = writer.len();
    record.truncate(len);
    let capture = RecordsCapture {
        records: vec![record],
        next: 0,
        trim_supported: false,
        trims: None,
        gate: None,
        paced_by: None,
        lockstep: Some(Arc::clone(&lockstep)),
        stay_open: true,
    };
    let engine = start_with(
        config(ClockPreference::Asrc),
        capture,
        opener(Arc::clone(&state), RATE),
        StartOptions {
            qpc_frequency: QPC_FREQUENCY,
            prepared: Some(prepared(RATE)),
        },
    )
    .unwrap();
    // The second read begins only after the first read's records were consumed.
    lockstep.step_read(2);
    let status = engine.status();
    lockstep.release();
    drop(engine);
    assert!(status.fault.is_none());
    let reason = status.endpoints[0]
        .unsupported
        .as_deref()
        .unwrap_or_default();
    assert!(reason.contains("no direct layout"), "{reason:?}");
}

#[test]
fn a_supported_format_clears_only_its_endpoints_unsupported_state() {
    let lockstep = Arc::new(Lockstep::default());
    let state = SinkState::lockstep(0, Arc::clone(&lockstep));
    let capture = RecordsCapture {
        records: vec![
            format_record_for(1, 32_000),
            format_record_for(2, 32_000),
            format_record_for(1, RATE),
        ],
        next: 0,
        trim_supported: false,
        trims: None,
        gate: None,
        paced_by: None,
        lockstep: Some(Arc::clone(&lockstep)),
        stay_open: true,
    };
    let engine = start_with_clock(
        lockstep_config(ClockPreference::Asrc, Arc::clone(&lockstep)),
        capture,
        opener(state, RATE),
        QPC_FREQUENCY,
    )
    .unwrap();
    lockstep.step_read(4);
    let status = engine.status();
    assert_eq!(status.endpoints[1].unsupported, None);
    assert!(status.endpoints[2].unsupported.is_some());
    assert!(status.fault.is_none());
    lockstep.release();
    let _ = engine.stop();
}

#[test]
fn a_speaker_profile_opens_its_layout_at_start() {
    let state = SinkState::new(0);
    let capture = RecordsCapture {
        records: vec![format_record(RATE)],
        next: 0,
        trim_supported: true,
        trims: None,
        gate: None,
        paced_by: None,
        lockstep: None,
        stay_open: true,
    };
    let engine = start_with(
        config(ClockPreference::Trim),
        capture,
        opener(Arc::clone(&state), RATE),
        StartOptions {
            qpc_frequency: QPC_FREQUENCY,
            prepared: Some(speaker_prepared(6, 0x3f)),
        },
    )
    .unwrap();
    assert_eq!(
        state.opened_layouts.lock().unwrap().as_slice(),
        [OutputLayout {
            channels: 6,
            channel_mask: 0x3f,
        }]
    );
    assert_eq!(engine.status().output_channels, 6);
    assert_eq!(engine.status().output_channel_mask, 0x3f);
    let _ = engine.stop();
}

#[test]
fn start_rejects_a_sink_with_another_channel_count() {
    let state = SinkState::new(0);
    let capture = RecordsCapture {
        records: Vec::new(),
        next: 0,
        trim_supported: false,
        trims: None,
        gate: None,
        paced_by: None,
        lockstep: None,
        stay_open: false,
    };
    let mut output = opener(state, RATE);
    output.honor_layout = false;
    assert!(matches!(
        start_with(
            config(ClockPreference::Asrc),
            capture,
            output,
            StartOptions {
                qpc_frequency: QPC_FREQUENCY,
                prepared: Some(speaker_prepared(6, 0x3f)),
            },
        ),
        Err(StartError::Config(_))
    ));
}

#[test]
fn first_sound_after_start_is_not_dropped() {
    let lockstep = Arc::new(Lockstep::default());
    let state = SinkState::lockstep(0, Arc::clone(&lockstep));
    let capture = RecordsCapture {
        records: vec![
            format_record(RATE),
            data_record_for(ENDPOINT_PRIMARY, 0, 0.25, 0.25, BLOCK),
            data_record_for(ENDPOINT_PRIMARY, BLOCK as u64, 0.25, 0.25, BLOCK),
        ],
        next: 0,
        trim_supported: true,
        trims: None,
        gate: None,
        paced_by: None,
        lockstep: Some(Arc::clone(&lockstep)),
        stay_open: false,
    };
    let engine = start_with(
        lockstep_config(ClockPreference::Trim, Arc::clone(&lockstep)),
        capture,
        opener(Arc::clone(&state), RATE),
        StartOptions {
            qpc_frequency: QPC_FREQUENCY,
            prepared: Some(prepared(RATE)),
        },
    )
    .unwrap();
    lockstep.step_read(3);
    lockstep.start_read(1);
    lockstep.wait_capture_finished();
    for _ in 0..24 {
        lockstep.step_period(1);
        if state
            .samples
            .lock()
            .unwrap()
            .iter()
            .any(|sample| *sample != 0.0)
        {
            break;
        }
    }
    lockstep.release();
    let report = engine.stop();
    assert_eq!(report.dropped_frames, 0);
    assert!(
        state
            .samples
            .lock()
            .unwrap()
            .iter()
            .any(|sample| *sample != 0.0),
        "first sound was lost: {report:?}"
    );
}

/// A DD+ decoder hands the mixer a whole 1536-frame sync frame every 32 ms,
/// which leaves one capture step as three or four mixer blocks. Priming must
/// cover that gap rather than one block: with silence first, the exact zeros
/// beyond the threshold are discarded, so a threshold of one block ran the
/// ring dry before the next sync frame whenever one arrived just before a
/// period (CI loopback, DD+ shared at 10 ms periods). Every arrival phase is
/// tried, in fifths of a period.
#[test]
fn priming_covers_a_decoder_sized_delivery() {
    const SYNC_FRAME: usize = 1536;
    const FRAMES: u64 = 40;
    let mut underruns = Vec::new();
    for phase in (0..BLOCK as u64).step_by(BLOCK / 5) {
        let lockstep = Arc::new(Lockstep::default());
        let state = SinkState::lockstep(0, Arc::clone(&lockstep));
        let mut records = vec![format_record(RATE)];
        records.extend((0..FRAMES).map(|frame| {
            let value = if frame < 4 { 0.0 } else { 0.25 };
            data_record_for(
                ENDPOINT_PRIMARY,
                frame * SYNC_FRAME as u64,
                value,
                value,
                SYNC_FRAME,
            )
        }));
        let capture = RecordsCapture {
            records,
            next: 0,
            trim_supported: false,
            trims: None,
            gate: None,
            paced_by: None,
            lockstep: Some(Arc::clone(&lockstep)),
            stay_open: true,
        };
        let engine = start_with(
            lockstep_config(ClockPreference::Asrc, Arc::clone(&lockstep)),
            capture,
            opener(Arc::clone(&state), RATE),
            StartOptions {
                qpc_frequency: QPC_FREQUENCY,
                prepared: Some(prepared(RATE)),
            },
        )
        .unwrap();
        lockstep.step_read(1);
        // A period every BLOCK frames; sync frame k arrives at
        // k * SYNC_FRAME + phase and is read before the next period.
        let mut delivered = 0_u64;
        for period in 0..(FRAMES * SYNC_FRAME as u64 / BLOCK as u64 - 8) {
            while delivered < FRAMES
                && delivered * SYNC_FRAME as u64 + phase <= period * BLOCK as u64
            {
                lockstep.step_read(1);
                delivered += 1;
            }
            lockstep.step_period(1);
        }
        // Count the stepped periods only: after release the output runs free
        // on a capture that delivers nothing more until stop lands, and how
        // many of those periods underrun depends on the scheduler.
        let stepped_underruns = engine.status().underruns;
        lockstep.release();
        let report = engine.stop();
        assert_eq!(report.dropped_frames, 0, "phase {phase}: {report:?}");
        assert!(
            state
                .samples
                .lock()
                .unwrap()
                .iter()
                .any(|sample| *sample != 0.0),
            "phase {phase}: no sound reached the output: {report:?}"
        );
        underruns.push((phase, stepped_underruns));
    }
    assert!(
        underruns.iter().all(|(_, count)| *count == 0),
        "underruns by arrival phase: {underruns:?}"
    );
}

#[test]
fn prepared_swap_is_retired_through_poll() {
    let lockstep = Arc::new(Lockstep::default());
    let state = SinkState::lockstep(0, Arc::clone(&lockstep));
    let mut records = vec![format_record(RATE)];
    for block in 0..8_u64 {
        records.push(data_record(block * BLOCK as u64, 0.25));
    }
    let capture = RecordsCapture {
        records,
        next: 0,
        trim_supported: true,
        trims: None,
        gate: None,
        paced_by: None,
        lockstep: Some(Arc::clone(&lockstep)),
        stay_open: true,
    };
    let mut engine = start_with_clock(
        lockstep_config(ClockPreference::Trim, Arc::clone(&lockstep)),
        capture,
        opener(state, RATE),
        QPC_FREQUENCY,
    )
    .unwrap();
    engine.apply(LiveChange::Prepared(prepared(RATE))).unwrap();
    lockstep.step_read(2);
    engine.apply(LiveChange::Prepared(prepared(RATE))).unwrap();
    lockstep.step_read(1);
    lockstep.step_period(1);
    let _ = engine.poll();
    lockstep.release();
    let _ = engine.stop();
}

#[test]
fn capture_loss_reconnects_and_clears_the_fault() {
    let lockstep = Arc::new(Lockstep::default());
    let state = SinkState::lockstep(0, Arc::clone(&lockstep));
    let reconnects = Arc::new(AtomicU64::new(0));
    let mut records = vec![format_record(RATE)];
    records.extend((0..12_u64).map(|block| data_record(block * BLOCK as u64, 0.25)));
    let capture = ReconnectingCapture {
        inner: RecordsCapture {
            records,
            next: 0,
            trim_supported: true,
            trims: None,
            gate: None,
            paced_by: None,
            lockstep: Some(Arc::clone(&lockstep)),
            stay_open: true,
        },
        fail_at: 3,
        lost: false,
        reconnects: Arc::clone(&reconnects),
    };
    let mut engine = start_with(
        lockstep_config(ClockPreference::Trim, Arc::clone(&lockstep)),
        capture,
        opener(Arc::clone(&state), RATE),
        StartOptions {
            qpc_frequency: QPC_FREQUENCY,
            prepared: Some(prepared(RATE)),
        },
    )
    .unwrap();
    lockstep.step_read(4);
    for _ in 0..4 {
        lockstep.step_read(1);
        lockstep.step_period(1);
    }
    assert_eq!(reconnects.load(Ordering::Acquire), 1);
    assert_eq!(engine.status().fault, None);
    let events = engine.poll();
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, EngineEvent::Fault(EngineFault::CaptureLost(_))))
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| **event == EngineEvent::FaultCleared)
            .count(),
        1
    );
    lockstep.release();
    let _ = engine.stop();
}

#[test]
fn latency_uses_playback_qpc_and_source_qpc() {
    let delay = 960_u64;
    let lockstep = Arc::new(Lockstep::default());
    let state = SinkState::lockstep(delay, Arc::clone(&lockstep));
    let mut records = vec![format_record(RATE)];
    for block in 0..20_u64 {
        records.push(data_record(block * BLOCK as u64, 0.25));
    }
    let capture = RecordsCapture {
        records,
        next: 0,
        trim_supported: true,
        trims: None,
        gate: None,
        paced_by: None,
        lockstep: Some(Arc::clone(&lockstep)),
        stay_open: true,
    };
    let engine = start_with(
        lockstep_config(ClockPreference::Trim, Arc::clone(&lockstep)),
        capture,
        opener(Arc::clone(&state), RATE),
        StartOptions {
            qpc_frequency: QPC_FREQUENCY,
            prepared: Some(prepared(RATE)),
        },
    )
    .unwrap();
    lockstep.step_read(1);
    for _ in 0..12 {
        lockstep.step_read(1);
        lockstep.step_period(1);
    }
    let status = engine.status();
    lockstep.release();
    drop(engine);
    assert!(
        status.latency.samples >= 5,
        "latency was {:?}",
        status.latency
    );
    // The fifth of eight observed markers has 120 ms of deterministic queued
    // capture/output progress; the synthetic sink adds exactly 960 / 48,000 =
    // 20 ms, so the median is 140 ms.
    assert_eq!(
        status.latency.p50_ms, 140.0,
        "latency was {:?}",
        status.latency
    );
}

#[test]
fn ddp_latency_uses_playback_qpc_and_source_qpc() {
    let delay = 960_u64;
    let lockstep = Arc::new(Lockstep::default());
    let state = SinkState::lockstep(delay, Arc::clone(&lockstep));
    let fixture = include_bytes!("../../decode/tests/fixtures/ddp/tones-5.1.eac3");
    let mut records = vec![ddp_format_record()];
    for block in 0..20_u64 {
        let start = block as usize % 32 * 2560;
        let mut burst = Vec::new();
        bitstream::frame_burst(
            bitstream::DataType::Eac3,
            &fixture[start..start + 2560],
            24_576,
            &mut burst,
        );
        records.push(bytes_record_for(ENDPOINT_PRIMARY, block * 1536, &burst));
    }
    let capture = RecordsCapture {
        records,
        next: 0,
        trim_supported: true,
        trims: None,
        gate: None,
        paced_by: None,
        lockstep: Some(Arc::clone(&lockstep)),
        stay_open: true,
    };
    let engine = start_with(
        lockstep_config(ClockPreference::Trim, Arc::clone(&lockstep)),
        capture,
        opener(Arc::clone(&state), RATE),
        StartOptions {
            qpc_frequency: QPC_FREQUENCY,
            prepared: Some(prepared(RATE)),
        },
    )
    .unwrap();
    lockstep.step_read(1);
    for _ in 0..12 {
        lockstep.step_read(1);
        lockstep.step_period(3);
    }
    let status = engine.status();
    if std::env::var_os("SSR_PRINT_LATENCY").is_some() {
        eprintln!("ddp latency was {:?}", status.latency);
    }
    assert!(
        status.latency.samples >= 5,
        "latency was {:?}",
        status.latency
    );
    lockstep.release();
    let _ = engine.stop();
}

#[test]
fn mute_changes_samples_by_at_most_one_ramp_increment() {
    let lockstep = Arc::new(Lockstep::default());
    let state = SinkState::lockstep(0, Arc::clone(&lockstep));
    let mut records = vec![format_record(RATE)];
    for block in 0..80_u64 {
        records.push(data_record(block * BLOCK as u64, 0.5));
    }
    let capture = RecordsCapture {
        records,
        next: 0,
        trim_supported: true,
        trims: None,
        gate: None,
        paced_by: None,
        lockstep: Some(Arc::clone(&lockstep)),
        stay_open: true,
    };
    let engine = start_with(
        lockstep_config(ClockPreference::Trim, Arc::clone(&lockstep)),
        capture,
        opener(Arc::clone(&state), RATE),
        StartOptions {
            qpc_frequency: QPC_FREQUENCY,
            prepared: Some(prepared(RATE)),
        },
    )
    .unwrap();
    lockstep.step_read(1);
    for _ in 0..3 {
        lockstep.step_read(1);
        lockstep.step_period(1);
    }
    engine.apply(LiveChange::Mute(true)).unwrap();
    for _ in 0..5 {
        lockstep.step_read(1);
        lockstep.step_period(1);
    }
    lockstep.release();
    let _ = engine.stop();
    let samples = state.samples.lock().unwrap();
    let left: Vec<f32> = samples
        .as_chunks::<2>()
        .0
        .iter()
        .map(|frame| frame[0])
        .collect();
    let max_step = left
        .windows(2)
        .map(|pair| (pair[1] - pair[0]).abs())
        .fold(0.0_f32, f32::max);
    assert!(max_step <= 0.5 / 240.0 + 1.0e-5, "step was {max_step}");
}

#[test]
fn asrc_converts_48_khz_to_44_1_khz_over_ten_seconds() {
    let lockstep = Arc::new(Lockstep::default());
    let state = SinkState::lockstep(0, Arc::clone(&lockstep));
    state.max_periods.store(1_000, Ordering::Release);
    let mut records = vec![format_record(RATE)];
    // A paced source a little longer than the ten seconds measured: an
    // unpaced one is dropped by the full ring within the first second, and
    // the output ends once the source has ended and the ring has drained.
    for block in 0..1_100_u64 {
        records.push(data_record(block * BLOCK as u64, 0.25));
    }
    let capture = RecordsCapture {
        records,
        next: 0,
        trim_supported: false,
        trims: None,
        gate: None,
        paced_by: None,
        lockstep: Some(Arc::clone(&lockstep)),
        stay_open: true,
    };
    let engine = start_with_clock(
        EngineConfig {
            clock: ClockPreference::Asrc,
            target_ms: 40.0,
            capacity_ms: 120.0,
            read_wait: Duration::ZERO,
            rt_event: Some({
                let lockstep = Arc::clone(&lockstep);
                Arc::new(move |event| {
                    if event == engine::RtLoopEvent::OutputEnd {
                        lockstep.complete_period();
                    }
                })
            }),
            ..EngineConfig::default()
        },
        capture,
        opener_with_period(Arc::clone(&state), 44_100, 441),
        QPC_FREQUENCY,
    )
    .unwrap();
    engine.apply(LiveChange::Prepared(prepared(RATE))).unwrap();
    lockstep.step_read(1);
    for _ in 0..999 {
        lockstep.step_read(1);
        lockstep.step_period(1);
    }
    lockstep.step_read(1);
    lockstep.step_period(1);
    lockstep.release();
    let _ = engine.stop();
    let produced = state.frames.load(Ordering::Acquire);
    let expected = 441_000_u64;
    let error = produced.abs_diff(expected) as f64 / expected as f64;
    assert!(error <= 0.001, "produced {produced}, expected {expected}");
}

#[test]
fn target_change_moves_the_ring_controller_toward_the_new_target() {
    let lockstep = Arc::new(Lockstep::default());
    let state = SinkState::lockstep(0, Arc::clone(&lockstep));
    let mut records = vec![format_record(RATE)];
    records.extend((0..900_u64).map(|block| data_record(block * BLOCK as u64, 0.25)));
    let capture = RecordsCapture {
        records,
        next: 0,
        trim_supported: false,
        trims: None,
        gate: None,
        paced_by: None,
        lockstep: Some(Arc::clone(&lockstep)),
        stay_open: true,
    };
    let engine = start_with(
        EngineConfig {
            target_ms: 10.0,
            capacity_ms: 80.0,
            rt_event: Some({
                let lockstep = Arc::clone(&lockstep);
                Arc::new(move |event| match event {
                    engine::RtLoopEvent::CaptureEnd => lockstep.complete_capture_phase(),
                    engine::RtLoopEvent::OutputEnd => lockstep.complete_period(),
                    engine::RtLoopEvent::CaptureFinished => lockstep.finish_capture(),
                    _ => {}
                })
            }),
            ..config(ClockPreference::Asrc)
        },
        capture,
        opener(Arc::clone(&state), RATE),
        StartOptions {
            qpc_frequency: QPC_FREQUENCY,
            prepared: Some(prepared(RATE)),
        },
    )
    .unwrap();

    for _ in 0..300 {
        lockstep.step_read(1);
        lockstep.step_period(1);
    }
    let old = engine.status();
    engine.apply(LiveChange::TargetMs(30.0)).unwrap();
    for _ in 0..400 {
        lockstep.step_read(1);
        lockstep.step_period(1);
    }
    let new = engine.status();
    lockstep.release();
    let _ = engine.stop();

    let old_distance = (old.ring_level_ms - 10.0).abs();
    let new_distance = (new.ring_level_ms - 30.0).abs();
    assert!(new.clock_ppm < old.clock_ppm, "old={old:?} new={new:?}");
    assert!(
        new_distance < (new.ring_level_ms - 10.0).abs(),
        "old={old:?} new={new:?}"
    );
    assert!(
        new_distance <= old_distance + 10.0,
        "old={old:?} new={new:?}"
    );
}

#[test]
fn a_layout_reopen_switches_trim_to_asrc_clears_trim_and_keeps_playing() {
    let lockstep = Arc::new(Lockstep::default());
    let state = SinkState::lockstep(0, Arc::clone(&lockstep));
    let trims = Arc::new(Mutex::new(Vec::new()));
    let mut records = vec![format_record(RATE), stream_start_record(ENDPOINT_PRIMARY)];
    let frequency = 997.0;
    records.extend(
        (0..96_u64).map(|block| tone_record(block * BLOCK as u64, block * BLOCK as u64, frequency)),
    );
    let capture = RecordsCapture {
        records,
        next: 0,
        trim_supported: true,
        trims: Some(Arc::clone(&trims)),
        gate: None,
        paced_by: None,
        lockstep: Some(Arc::clone(&lockstep)),
        stay_open: true,
    };
    let mut output = opener(Arc::clone(&state), RATE);
    output.reopen_format = Some(OutputFormat {
        rate: 44_100,
        period_frames: 441,
        ..output.format
    });
    let mut engine = start_with(
        lockstep_config(ClockPreference::Auto, Arc::clone(&lockstep)),
        capture,
        output,
        StartOptions {
            qpc_frequency: QPC_FREQUENCY,
            prepared: Some(prepared(RATE)),
        },
    )
    .unwrap();
    for _ in 0..6 {
        lockstep.step_read(2);
        lockstep.step_period(1);
    }
    for _ in 0..24 {
        lockstep.step_read(1);
        lockstep.step_period(1);
    }
    let before_switch_trims = trims.lock().unwrap().clone();
    assert!(
        before_switch_trims.iter().any(|ppm| ppm.abs() > 0.1),
        "Trim mode never requested a nonzero correction: {before_switch_trims:?}"
    );
    let old_periods = state.periods_by_layout.lock().unwrap().clone();
    assert!(old_periods.iter().any(|(layout, samples)| {
        *layout == OutputLayout::STEREO && samples.iter().any(|sample| *sample != 0.0)
    }));
    engine
        .apply(LiveChange::Prepared(speaker_prepared(12, 0)))
        .unwrap();
    for _ in 0..40 {
        lockstep.step_read(1);
        lockstep.step_period(1);
        if engine.status().output_reopens == 1
            && trims
                .lock()
                .unwrap()
                .windows(2)
                .any(|pair| pair[0].abs() > 0.1 && pair[1] == 0.0)
            && state
                .periods_by_layout
                .lock()
                .unwrap()
                .iter()
                .any(|(layout, samples)| {
                    layout.channels == 12 && samples.iter().any(|sample| *sample != 0.0)
                })
        {
            break;
        }
    }
    let status = engine.status();
    let events = engine.poll();
    lockstep.release();
    let report = engine.stop();
    assert_eq!(status.output_channels, 12);
    assert_eq!(status.output_channel_mask, 0);
    assert_eq!(status.output_reopens, 1);
    assert_eq!(status.output_rate, 44_100);
    assert!(status.trim_available);
    assert_eq!(status.clock, ClockStatus::Asrc);
    let trims = trims.lock().unwrap().clone();
    assert!(
        trims
            .windows(2)
            .any(|pair| pair[0].abs() > 0.1 && pair[1] == 0.0),
        "the capture source did not receive zero after leaving Trim: {trims:?}"
    );
    assert_eq!(
        state.opened_layouts.lock().unwrap().as_slice(),
        [
            OutputLayout::STEREO,
            OutputLayout {
                channels: 12,
                channel_mask: 0,
            },
        ]
    );
    let periods = state.periods_by_layout.lock().unwrap();
    let old_sink: Vec<_> = periods
        .iter()
        .filter(|(layout, _)| *layout == OutputLayout::STEREO)
        .map(|(_, samples)| samples)
        .collect();
    let old_last = old_sink
        .last()
        .copied()
        .expect("old sink emitted a final ASRC-tail period");
    let old_penultimate = old_sink
        .get(old_sink.len().saturating_sub(2))
        .copied()
        .expect("old sink emitted audio before its ASRC tail");
    let tail_energy = period_energy(old_last);
    let preceding_energy = period_energy(old_penultimate);
    // At a layout boundary no new stereo input may be consumed. The last old-
    // sink period therefore consists solely of the old ASRC's delayed output;
    // its energy must be present, but below the preceding tone period. Exact
    // sample counts are filter-implementation details, so energy is the stable
    // completeness witness.
    assert!(
        tail_energy > 0.0 && tail_energy < preceding_energy,
        "old ASRC tail energy {tail_energy} did not follow tone energy {preceding_energy}"
    );
    assert_eq!(old_last[old_last.len() - 2..], [0.0, 0.0]);
    // output_delay spans two 10 ms periods at this ratio, so include the last
    // rendered period followed by both drain periods.
    let old_boundary: Vec<_> = old_sink[old_sink.len().saturating_sub(3)..]
        .iter()
        .map(|samples| samples.as_slice())
        .collect();
    assert_boundary_tail_is_continuous_and_silent_after_zero(&old_boundary, frequency);
    let new_first = periods
        .iter()
        .find(|(layout, samples)| layout.channels == 12 && samples.iter().any(|x| *x != 0.0))
        .map(|(_, samples)| samples)
        .expect("new sink emitted audio");
    assert!(new_first[..12].iter().all(|sample| *sample == 0.0));
    assert!(new_first[12..96 * 12].iter().any(|sample| *sample != 0.0));
    drop(periods);
    assert!(events.iter().any(|event| matches!(
        event,
        EngineEvent::OutputLayoutChanged {
            channels: 12,
            channel_mask: 0,
        }
    )));
    assert_eq!(report.output_reopens, 1);
    assert_eq!(report.output_channels, 12);
    assert_eq!(report.final_status.output_rate, 44_100);
    assert_eq!(report.final_status.clock, ClockStatus::Asrc);
}

#[test]
fn asrc_layout_reopen_fades_through_the_real_tail_and_stays_silent() {
    let lockstep = Arc::new(Lockstep::default());
    let state = SinkState::lockstep(0, Arc::clone(&lockstep));
    let frequency = 997.0;
    let mut records = vec![format_record(RATE), stream_start_record(ENDPOINT_PRIMARY)];
    records.extend(
        (0..96_u64).map(|block| tone_record(block * BLOCK as u64, block * BLOCK as u64, frequency)),
    );
    let capture = RecordsCapture {
        records,
        next: 0,
        trim_supported: false,
        trims: None,
        gate: None,
        paced_by: None,
        lockstep: Some(Arc::clone(&lockstep)),
        stay_open: true,
    };
    let mut output = opener_with_period(Arc::clone(&state), 44_100, 441);
    output.reopen_format = Some(OutputFormat {
        rate: 44_100,
        period_frames: 441,
        ..output.format
    });
    let engine = start_with(
        lockstep_config(ClockPreference::Asrc, Arc::clone(&lockstep)),
        capture,
        output,
        StartOptions {
            qpc_frequency: QPC_FREQUENCY,
            prepared: Some(prepared(RATE)),
        },
    )
    .unwrap();

    for _ in 0..24 {
        lockstep.step_read(1);
        lockstep.step_period(1);
    }
    engine
        .apply(LiveChange::Prepared(speaker_prepared(12, 0)))
        .unwrap();
    for _ in 0..40 {
        lockstep.step_read(1);
        lockstep.step_period(1);
        if engine.status().output_reopens == 1 {
            break;
        }
    }
    let periods = state.periods_by_layout.lock().unwrap().clone();
    lockstep.release();
    let report = engine.stop();

    let first_reopened = periods
        .iter()
        .position(|(layout, _)| layout.channels == 12)
        .expect("the new-layout sink emitted a period");
    let old_tail = &periods[..first_reopened];
    let old_last = &old_tail.last().expect("old sink emitted a drain period").1;
    let old_penultimate = &old_tail
        .get(old_tail.len().saturating_sub(2))
        .expect("old sink emitted the preceding drain period")
        .1;
    let tail_energy = period_energy(old_last);
    let preceding_energy = period_energy(old_penultimate);
    assert!(
        tail_energy > 0.0 && tail_energy < preceding_energy,
        "old ASRC tail energy {tail_energy} did not follow tone energy {preceding_energy}"
    );
    let boundary: Vec<_> = old_tail
        .iter()
        .rev()
        .take(3)
        .rev()
        .map(|(_, samples)| samples.as_slice())
        .collect();
    assert_boundary_tail_is_continuous_and_silent_after_zero(&boundary, frequency);
    let frames = boundary_left_frames(&boundary);
    let last_nonzero = frames
        .iter()
        .rposition(|sample| *sample != 0.0)
        .expect("old sink emitted no tail signal");
    // This fixture's final padded take contributes 438 output frames before
    // rubato's 117-frame delay; the first boundary period precedes that take.
    let predicted_signal_end = 441 + 438 + 117;
    assert!(
        last_nonzero.abs_diff(predicted_signal_end - 1) <= 2,
        "last nonzero frame {last_nonzero} missed predicted tail end {}",
        predicted_signal_end - 1
    );
    assert!(
        frames[predicted_signal_end..]
            .iter()
            .all(|sample| *sample == 0.0)
    );
    let fade_start = predicted_signal_end - 88;
    let cycle_frames = (44_100.0_f32 / frequency).ceil() as usize;
    let peak = frames[fade_start - cycle_frames..fade_start]
        .iter()
        .map(|sample| sample.abs())
        .fold(0.0_f32, f32::max);
    assert!(
        peak >= 0.97,
        "pre-fade tone peak {peak} was below 97% of full scale"
    );
    assert_eq!(report.output_reopens, 1);
}

#[test]
fn forced_trim_incompatible_layout_reopen_retries_and_stops_without_success() {
    // Wall time is intentional here: the property is that stop interrupts the
    // retry backoff after multiple incompatible opens.
    let lockstep = Arc::new(Lockstep::default());
    let state = SinkState::lockstep(0, Arc::clone(&lockstep));
    let mut records = vec![format_record(RATE), stream_start_record(ENDPOINT_PRIMARY)];
    records.extend(
        (0..96_u64).map(|block| {
            data_record_for(ENDPOINT_PRIMARY, block * BLOCK as u64, 0.25, 0.25, BLOCK)
        }),
    );
    let capture = RecordsCapture {
        records,
        next: 0,
        trim_supported: true,
        trims: None,
        gate: None,
        paced_by: None,
        lockstep: Some(Arc::clone(&lockstep)),
        stay_open: true,
    };
    let mut output = opener(Arc::clone(&state), RATE);
    output.reopen_format = Some(OutputFormat {
        rate: 44_100,
        period_frames: 441,
        ..output.format
    });
    let engine = start_with(
        lockstep_config(ClockPreference::Trim, Arc::clone(&lockstep)),
        capture,
        output,
        StartOptions {
            qpc_frequency: QPC_FREQUENCY,
            prepared: Some(prepared(RATE)),
        },
    )
    .unwrap();
    for _ in 0..30 {
        lockstep.step_read(1);
        lockstep.step_period(1);
    }
    engine
        .apply(LiveChange::Prepared(speaker_prepared(12, 0)))
        .unwrap();
    for _ in 0..40 {
        lockstep.step_read(1);
        lockstep.step_period(1);
        if state.opened_formats.lock().unwrap().len() >= 2 {
            break;
        }
    }
    wait_for_thread_exit(Duration::from_secs(2), || {
        state.opened_formats.lock().unwrap().len() >= 3
    });
    let status = engine.status();
    lockstep.release();
    let start = Instant::now();
    let report = engine.stop();

    assert!(start.elapsed() < Duration::from_millis(200));
    assert!(state.opened_formats.lock().unwrap().len() >= 3);
    assert_eq!(status.output_reopens, 0);
    assert_eq!(status.output_reconnects, 0);
    assert_eq!(report.output_reopens, 0);
    assert_eq!(report.final_status.output_reconnects, 0);
    assert_eq!(report.final_status.clock, ClockStatus::Trim);
}

#[test]
fn output_failure_discards_mixed_layout_crossing_audio_and_recovers() {
    let lockstep = Arc::new(Lockstep::default());
    let state = SinkState::lockstep(0, Arc::clone(&lockstep));
    let mut records = vec![format_record(RATE), stream_start_record(ENDPOINT_PRIMARY)];
    records.extend(
        (0..80_u64).map(|block| {
            data_record_for(ENDPOINT_PRIMARY, block * BLOCK as u64, 0.25, 0.25, BLOCK)
        }),
    );
    let capture = RecordsCapture {
        records,
        next: 0,
        trim_supported: true,
        trims: None,
        gate: None,
        paced_by: Some(Pacing {
            state: Arc::clone(&state),
            lead: 8,
            scene_rate: RATE,
        }),
        lockstep: Some(Arc::clone(&lockstep)),
        stay_open: true,
    };
    let engine = start_with(
        lockstep_config(ClockPreference::Trim, Arc::clone(&lockstep)),
        capture,
        opener(Arc::clone(&state), RATE),
        StartOptions {
            qpc_frequency: QPC_FREQUENCY,
            prepared: Some(prepared(RATE)),
        },
    )
    .unwrap();

    for _ in 0..8 {
        lockstep.step_read(1);
    }
    lockstep.step_period(3);
    engine
        .apply(LiveChange::Prepared(speaker_prepared(12, 0)))
        .unwrap();
    // Keep old stereo markers and samples queued while the capture thread starts
    // appending 12-channel markers behind them, then fail before either layout is
    // drained. The reconnect discard must consume by each marker's own width.
    for _ in 0..6 {
        lockstep.step_read(1);
    }
    state
        .fail_from_period
        .store(state.periods.load(Ordering::Acquire), Ordering::Release);
    // `OutputEnd` fires before the loop requests its next sink period. Permit
    // that request separately, then wait until the sink has actually returned
    // Removed before making the reopened sink healthy.
    lockstep.step_period(1);
    lockstep.start_period(1);
    lockstep.wait_output_failure(&state.failure_observed);
    state.fail_from_period.store(u64::MAX, Ordering::Release);

    for _ in 0..60 {
        lockstep.step_read(1);
        lockstep.step_period(1);
        let status = engine.status();
        if status.output_reconnects == 1
            && status.output_reopens == 1
            && state
                .periods_by_layout
                .lock()
                .unwrap()
                .iter()
                .any(|(layout, samples)| {
                    layout.channels == 12 && samples.iter().any(|sample| *sample != 0.0)
                })
        {
            break;
        }
    }
    let status = engine.status();
    let periods = state.periods_by_layout.lock().unwrap().clone();
    let opened = state.opened_layouts.lock().unwrap().clone();
    lockstep.release();
    let _ = engine.stop();

    assert_eq!(status.output_reconnects, 1);
    assert_eq!(status.output_reopens, 1);
    assert!(status.output_overflow_frames > 0);
    assert_eq!(
        opened,
        [
            OutputLayout::STEREO,
            OutputLayout::STEREO,
            OutputLayout {
                channels: 12,
                channel_mask: 0,
            },
        ]
    );
    let resumed = periods
        .iter()
        .find(|(layout, samples)| {
            layout.channels == 12 && samples.iter().any(|sample| *sample != 0.0)
        })
        .map(|(_, samples)| samples)
        .expect("12-channel output resumes after crossing-ring discard");
    assert!(resumed[..12].iter().all(|sample| *sample == 0.0));
    assert!(resumed[12..96 * 12].iter().any(|sample| *sample != 0.0));
}

#[test]
fn sustained_slow_cadence_grows_the_period_and_reopens_once() {
    let lockstep = Arc::new(Lockstep::default());
    let state = SinkState::lockstep(0, Arc::clone(&lockstep));
    state.played_numerator.store(4, Ordering::Release);
    state.played_denominator.store(5, Ordering::Release);
    state
        .inject_cadence_tail_enabled
        .store(true, Ordering::Release);
    let mut records = vec![format_record(RATE), stream_start_record(ENDPOINT_PRIMARY)];
    let frequency = 997.0;
    records.extend(
        (0..1_200_u64)
            .map(|block| tone_record(block * BLOCK as u64, block * BLOCK as u64, frequency)),
    );
    let capture = RecordsCapture {
        records,
        next: 0,
        trim_supported: false,
        trims: None,
        gate: None,
        paced_by: Some(Pacing {
            state: Arc::clone(&state),
            lead: 8,
            scene_rate: RATE,
        }),
        lockstep: Some(Arc::clone(&lockstep)),
        stay_open: true,
    };
    let mut output = opener_with_period(Arc::clone(&state), 44_100, 441);
    output.reopen_format = Some(OutputFormat::exclusive(44_100, 2, 0x3, 882));
    let engine = start_with(
        lockstep_config(ClockPreference::Auto, Arc::clone(&lockstep)),
        capture,
        output,
        StartOptions {
            qpc_frequency: QPC_FREQUENCY,
            prepared: Some(prepared(RATE)),
        },
    )
    .unwrap();

    // 10 ms synthetic periods make each cadence window one second. Four
    // windows cross the sustained three-second threshold and leave room for
    // the cadence-triggered reopen to complete.
    for _ in 0..401 {
        lockstep.step_read(1);
        lockstep.step_period(1);
    }
    while state.opened_formats.lock().unwrap().len() < 2 {
        lockstep.step_read(1);
        lockstep.step_period(1);
    }
    let requested = state.requested_periods.lock().unwrap().clone();
    let opened: Vec<_> = state
        .opened_formats
        .lock()
        .unwrap()
        .iter()
        .map(|format| format.period_frames)
        .collect();
    let status = engine.status();

    // The reopened sink keeps reporting 80%, but this session must not ask for
    // another period increase or another reopen.
    for _ in 0..200 {
        lockstep.step_read(1);
        lockstep.step_period(1);
    }
    let final_requested = state.requested_periods.lock().unwrap().len();
    let final_opened = state.opened_formats.lock().unwrap().len();
    let periods = state.periods_by_layout.lock().unwrap().clone();
    lockstep.release();
    let report = engine.stop();

    let first_reopened = periods
        .iter()
        .position(|(_, samples)| samples.len() == 882 * 2)
        .expect("the larger-period sink emitted a period");
    let old_tail = &periods[..first_reopened];
    let old_last = &old_tail
        .last()
        .expect("the old cadence sink emitted its ASRC tail")
        .1;
    let old_penultimate = &old_tail
        .get(old_tail.len().saturating_sub(2))
        .expect("the old cadence sink emitted tone before its ASRC tail")
        .1;
    let tail_energy = period_energy(old_last);
    let preceding_energy = period_energy(old_penultimate);
    // The cadence request is raised only after an old-sink period completes.
    // The following old-sink period is consequently fed only zero input and is
    // the delayed ASRC contribution. Energy avoids coupling the proof to
    // rubato's exact fractional-delay sample count.
    assert!(
        tail_energy > 0.0 && tail_energy < preceding_energy,
        "old ASRC tail energy {tail_energy} did not follow tone energy {preceding_energy}"
    );
    // output_delay spans two 10 ms periods at this ratio, so include the last
    // rendered period followed by both drain periods.
    let old_boundary: Vec<_> = old_tail
        .iter()
        .rev()
        .take(3)
        .rev()
        .map(|(_, samples)| samples.as_slice())
        .collect();
    assert_boundary_tail_is_continuous_and_silent_after_zero(&old_boundary, frequency);
    let boundary_frames = boundary_left_frames(&old_boundary);
    let drain_start = 441;
    assert!(
        boundary_frames[..drain_start]
            .iter()
            .any(|sample| *sample != 0.0),
        "cadence fixture did not make the final written period audible"
    );
    assert_tone_step_bound(&boundary_frames[..drain_start + 117], frequency);
    let resumed = periods[first_reopened..]
        .iter()
        .any(|(_, samples)| samples.iter().any(|sample| *sample != 0.0));
    assert_eq!(requested, [Duration::from_millis(20)]);
    assert_eq!(opened, [441, 882]);
    assert!(status.output_cadence_faults >= 1);
    assert_eq!(status.clock, ClockStatus::Asrc);
    assert_eq!(report.final_status.clock, ClockStatus::Asrc);
    assert!(
        status.output_cadence_ratio < 0.9,
        "unexpected cadence ratio: {}",
        status.output_cadence_ratio
    );
    assert_eq!(final_requested, 1);
    assert_eq!(final_opened, 2);
    assert!(resumed, "audio did not resume after the cadence reopen");
}

#[test]
fn a_failed_reopen_recovers_and_clears_the_output_fault() {
    let lockstep = Arc::new(Lockstep::default());
    let state = SinkState::lockstep(0, Arc::clone(&lockstep));
    let mut records = vec![format_record(RATE)];
    records.extend((0..12_u64).map(|block| data_record(block * BLOCK as u64, 0.25)));
    let capture = RecordsCapture {
        records,
        next: 0,
        trim_supported: true,
        trims: None,
        gate: None,
        paced_by: None,
        lockstep: Some(Arc::clone(&lockstep)),
        stay_open: true,
    };
    let mut output = opener(Arc::clone(&state), RATE);
    output.fail_on_open = Some(2);
    let mut engine = start_with(
        lockstep_config(ClockPreference::Trim, Arc::clone(&lockstep)),
        capture,
        output,
        StartOptions {
            qpc_frequency: QPC_FREQUENCY,
            prepared: Some(prepared(RATE)),
        },
    )
    .unwrap();
    lockstep.step_read(3);
    lockstep.step_period(2);
    engine
        .apply(LiveChange::Prepared(speaker_prepared(6, 0x3f)))
        .unwrap();
    for _ in 0..16 {
        lockstep.step_read(1);
        lockstep.step_period(1);
        if engine.status().output_reopens == 1 {
            break;
        }
    }
    assert_eq!(engine.status().fault, None);
    assert_eq!(engine.status().output_reopens, 1);
    assert_eq!(engine.status().output_reconnects, 0);
    let events = engine.poll();
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, EngineEvent::Fault(EngineFault::OutputLost(_))))
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| **event == EngineEvent::FaultCleared)
            .count(),
        1
    );
    lockstep.release();
    let _ = engine.stop();
}

#[test]
fn reconnect_at_44_1_khz_switches_to_asrc_and_fades_audio_back_in() {
    let lockstep = Arc::new(Lockstep::default());
    let state = SinkState::lockstep(0, Arc::clone(&lockstep));
    state.fail_from_period.store(4, Ordering::Release);
    let mut records = vec![format_record(RATE)];
    records.extend(
        (0..80_u64).map(|block| {
            data_record_for(ENDPOINT_PRIMARY, block * BLOCK as u64, 0.25, 0.25, BLOCK)
        }),
    );
    let capture = RecordsCapture {
        records,
        next: 0,
        trim_supported: true,
        trims: None,
        gate: None,
        paced_by: Some(Pacing {
            state: Arc::clone(&state),
            lead: 8,
            scene_rate: RATE,
        }),
        lockstep: Some(Arc::clone(&lockstep)),
        stay_open: true,
    };
    let mut output = opener(Arc::clone(&state), RATE);
    output.reopen_format = Some(OutputFormat {
        rate: 44_100,
        period_frames: 441,
        ..output.format
    });
    let engine = start_with(
        lockstep_config(ClockPreference::Auto, Arc::clone(&lockstep)),
        capture,
        output,
        StartOptions {
            qpc_frequency: QPC_FREQUENCY,
            prepared: Some(prepared(RATE)),
        },
    )
    .unwrap();
    for _ in 0..8 {
        lockstep.step_read(1);
    }
    lockstep.step_period(5);
    state.fail_from_period.store(u64::MAX, Ordering::Release);
    for _ in 0..60 {
        lockstep.step_read(1);
        lockstep.step_period(1);
        if engine.status().output_reconnects == 1
            && state.opened_formats.lock().unwrap().len() == 2
            && state
                .periods_by_layout
                .lock()
                .unwrap()
                .iter()
                .any(|(_, samples)| samples.len() == 441 * 2 && samples.iter().any(|x| *x != 0.0))
        {
            break;
        }
    }
    let status = engine.status();
    let periods = state.periods_by_layout.lock().unwrap().clone();
    lockstep.release();
    let report = engine.stop();

    assert_eq!(status.output_reconnects, 1);
    assert_eq!(state.opened_formats.lock().unwrap()[1].rate, 44_100);
    assert_eq!(status.clock, ClockStatus::Asrc);
    assert_eq!(report.final_status.clock, ClockStatus::Asrc);
    let resumed = periods
        .iter()
        .find(|(_, samples)| samples.len() == 441 * 2 && samples.iter().any(|x| *x != 0.0))
        .map(|(_, samples)| samples)
        .unwrap_or_else(|| {
            panic!(
                "44.1 kHz sink resumed audible output; periods={} nonzero={:?} status={status:?}",
                periods.len(),
                periods
                    .iter()
                    .map(|(_, samples)| (
                        samples.len(),
                        samples.iter().filter(|x| **x != 0.0).count()
                    ))
                    .collect::<Vec<_>>()
            )
        });
    assert_eq!(resumed[..2], [0.0, 0.0]);
    assert!(resumed[2..88 * 2].iter().any(|sample| *sample != 0.0));
}

#[test]
fn an_output_failure_reports_why_the_sink_failed() {
    // The lockstep sink fails with Removed after its last permitted period; the
    // fault must carry that error rather than fixed text. Later opens fail too,
    // keeping the fault observable until the harness releases the output thread.
    let lockstep = Arc::new(Lockstep::default());
    let state = SinkState::lockstep(0, Arc::clone(&lockstep));
    state.fail_from_period.store(5, Ordering::Release);
    let capture = RecordsCapture {
        records: vec![format_record(RATE)],
        next: 0,
        trim_supported: false,
        trims: None,
        gate: None,
        paced_by: None,
        lockstep: Some(Arc::clone(&lockstep)),
        stay_open: true,
    };
    let mut output = opener(Arc::clone(&state), RATE);
    output.fail_after_first = true;
    let mut engine = start_with_clock(
        lockstep_config(ClockPreference::Asrc, Arc::clone(&lockstep)),
        capture,
        output,
        QPC_FREQUENCY,
    )
    .unwrap();
    lockstep.step_period(6);
    let expected = EngineFault::OutputLost(OutputError::Removed.to_string());
    let mut status = engine.status();
    for _ in 0..16 {
        if status.fault.is_some() {
            break;
        }
        std::thread::yield_now();
        status = engine.status();
    }
    let events = engine.poll();
    lockstep.release();
    let _ = engine.stop();
    assert_eq!(status.fault, Some(expected.clone()));
    assert!(
        events
            .iter()
            .any(|event| *event == EngineEvent::Fault(expected.clone())),
        "the fault event carries the same text"
    );
}

#[test]
fn a_stopped_source_lets_the_brir_tail_play_and_counts_no_underrun() {
    let lockstep = Arc::new(Lockstep::default());
    let state = SinkState::lockstep(0, Arc::clone(&lockstep));
    let capture = RecordsCapture {
        records: vec![
            format_record(RATE),
            stream_start_record(ENDPOINT_PRIMARY),
            data_record_for(ENDPOINT_PRIMARY, BLOCK as u64, 0.5, 0.5, BLOCK),
            stream_stop_record(ENDPOINT_PRIMARY),
        ],
        next: 0,
        trim_supported: true,
        trims: None,
        gate: None,
        paced_by: None,
        lockstep: Some(Arc::clone(&lockstep)),
        stay_open: true,
    };
    let mut ir = vec![0.0; BLOCK * 2];
    ir[0] = 1.0;
    ir[BLOCK + 17] = 0.5;
    let engine = start_with(
        lockstep_config(ClockPreference::Trim, Arc::clone(&lockstep)),
        capture,
        opener(Arc::clone(&state), RATE),
        StartOptions {
            qpc_frequency: QPC_FREQUENCY,
            prepared: Some(prepared_with_ir(RATE, &ir)),
        },
    )
    .unwrap();
    lockstep.step_read(4);
    // One second of virtual idle lets both 480-frame BRIR partitions leave the
    // mixer while the output advances under the same synthetic clock.
    for _ in 0..100 {
        lockstep.step_read(1);
        lockstep.step_period(1);
    }
    let samples = state.samples.lock().unwrap();
    let nonzero_periods: Vec<_> = samples
        .as_chunks::<{ BLOCK * 2 }>()
        .0
        .iter()
        .enumerate()
        .filter_map(|(period, samples)| {
            samples
                .iter()
                .any(|sample| *sample != 0.0)
                .then_some(period)
        })
        .collect();
    let tail_was_audible = nonzero_periods.len() >= 2;
    drop(samples);
    let underruns = engine.status().underruns;
    assert!(tail_was_audible, "tail periods were {nonzero_periods:?}");
    assert_eq!(underruns, 0);
    lockstep.release();
    drop(engine);
}

#[test]
fn ended_input_drains_a_thirty_two_channel_ring_below_the_priming_threshold() {
    let lockstep = Arc::new(Lockstep::default());
    let state = SinkState::lockstep(0, Arc::clone(&lockstep));
    let capture = RecordsCapture {
        records: vec![format_record(RATE), data_record(0, 0.25)],
        next: 0,
        trim_supported: true,
        trims: None,
        gate: None,
        paced_by: None,
        lockstep: Some(Arc::clone(&lockstep)),
        stay_open: false,
    };
    let mut runtime = lockstep_config(ClockPreference::Trim, Arc::clone(&lockstep));
    runtime.target_ms = 0.0;
    runtime.capacity_ms = 10.0;
    let engine = start_with(
        runtime,
        capture,
        opener(Arc::clone(&state), RATE),
        StartOptions {
            qpc_frequency: QPC_FREQUENCY,
            prepared: Some(speaker_prepared(32, 0)),
        },
    )
    .unwrap();

    lockstep.step_read(2);
    // A third entry acknowledges the data read and reports EOF. Finishing must
    // fit its blocks by MIX_FRAMES at the active 32-channel layout, not by the
    // decoder's larger max_block, and EOF must release output priming.
    lockstep.start_read(1);
    lockstep.wait_capture_finished();
    {
        let mut steps = lockstep.steps.lock().unwrap();
        steps.period_allowed += 16;
        lockstep.changed.notify_all();
    }
    wait_for_thread_exit(Duration::from_secs(5), || {
        state.stopped.load(Ordering::Acquire)
    });
    let samples = state.samples.lock().unwrap();
    let audible_frames = samples
        .as_chunks::<32>()
        .0
        .iter()
        .filter(|frame| frame.iter().any(|sample| *sample != 0.0))
        .count();
    assert!(
        audible_frames > 0,
        "EOF left the sub-threshold block in priming"
    );
    drop(samples);
    assert!(
        state.stopped.load(Ordering::Acquire),
        "runtime did not drain EOF"
    );
    lockstep.release();
    let report = engine.stop();
    assert!(report.blocks > 0);
    assert_eq!(report.output_channels, 32);
}

#[test]
fn asrc_eof_fades_through_the_real_tail_and_stays_silent() {
    let lockstep = Arc::new(Lockstep::default());
    let state = SinkState::lockstep(0, Arc::clone(&lockstep));
    let frequency = 997.0;
    let mut records = vec![format_record(RATE), stream_start_record(ENDPOINT_PRIMARY)];
    records.extend(
        (0..32_u64).map(|block| tone_record(block * BLOCK as u64, block * BLOCK as u64, frequency)),
    );
    let record_count = records.len();
    let capture = RecordsCapture {
        records,
        next: 0,
        trim_supported: false,
        trims: None,
        gate: None,
        paced_by: None,
        lockstep: Some(Arc::clone(&lockstep)),
        stay_open: false,
    };
    let mut runtime = lockstep_config(ClockPreference::Asrc, Arc::clone(&lockstep));
    runtime.target_ms = 0.0;
    let engine = start_with(
        runtime,
        capture,
        opener_with_period(Arc::clone(&state), 44_100, 441),
        StartOptions {
            qpc_frequency: QPC_FREQUENCY,
            prepared: Some(prepared(RATE)),
        },
    )
    .unwrap();

    lockstep.step_read(record_count as u64);
    lockstep.start_read(1);
    lockstep.wait_capture_finished();
    {
        let mut steps = lockstep.steps.lock().unwrap();
        steps.period_allowed += 64;
        lockstep.changed.notify_all();
    }
    wait_for_thread_exit(Duration::from_secs(5), || {
        state.stopped.load(Ordering::Acquire)
    });
    let periods = state.periods_by_layout.lock().unwrap();
    let last_audible = periods
        .iter()
        .rposition(|(_, samples)| samples.iter().any(|sample| *sample != 0.0))
        .expect("EOF emitted no audible period");
    // Cover the final rendered period and both drain periods without including
    // the ASRC startup transient.
    let boundary_start = last_audible.saturating_sub(2);
    let boundary_end = (last_audible + 2).min(periods.len());
    let boundary: Vec<_> = periods[boundary_start..boundary_end]
        .iter()
        .map(|(_, samples)| samples.as_slice())
        .collect();
    assert_boundary_tail_is_continuous_and_silent_after_zero(&boundary, frequency);
    let frames = boundary_left_frames(&boundary);
    let signal_end = frames
        .iter()
        .rposition(|sample| sample.abs() > 1.0e-4)
        .map(|frame| frame + 1)
        .expect("EOF emitted no tail signal");
    assert!(
        frames[signal_end..]
            .iter()
            .all(|sample| sample.abs() <= 1.0e-4),
        "EOF output rose above the ASRC numerical-noise floor after the signal"
    );
    let fade_start = signal_end.saturating_sub(88);
    let cycle_frames = (44_100.0_f32 / frequency).ceil() as usize;
    let peak = frames[fade_start - cycle_frames..fade_start]
        .iter()
        .map(|sample| sample.abs())
        .fold(0.0_f32, f32::max);
    assert!(
        peak >= 0.97,
        "pre-fade EOF tone peak {peak} was below 97% of full scale"
    );
    drop(periods);
    lockstep.release();
    let _ = engine.stop();
}

#[test]
fn second_endpoint_first_block_from_finish_reaches_runtime_output() {
    let lockstep = Arc::new(Lockstep::default());
    let state = SinkState::lockstep(0, Arc::clone(&lockstep));
    let capture = RecordsCapture {
        records: vec![
            format_record_for(1, RATE),
            data_record_for(1, 0, 0.25, 0.25, 96),
        ],
        next: 0,
        trim_supported: false,
        trims: None,
        gate: None,
        paced_by: None,
        lockstep: Some(Arc::clone(&lockstep)),
        stay_open: false,
    };
    let engine = start_with(
        lockstep_config(ClockPreference::Asrc, Arc::clone(&lockstep)),
        capture,
        opener(Arc::clone(&state), RATE),
        StartOptions {
            qpc_frequency: QPC_FREQUENCY,
            prepared: Some(prepared(RATE)),
        },
    )
    .unwrap();
    lockstep.step_read(2);
    // The third entry acknowledges the data read and returns Ended; the capture
    // thread then drains the finish stages as ring space allows. An ended read
    // has no following entry, so wait for its start rather than completion.
    lockstep.start_read(1);
    lockstep.wait_capture_finished();
    for _ in 0..24 {
        lockstep.step_period(1);
        if state
            .samples
            .lock()
            .unwrap()
            .iter()
            .any(|sample| *sample != 0.0)
        {
            break;
        }
    }
    let status = engine.status();
    let samples_have_audio = state
        .samples
        .lock()
        .unwrap()
        .iter()
        .any(|sample| *sample != 0.0);
    lockstep.release();
    let report = engine.stop();
    assert_eq!(status.endpoints[0].format, None);
    assert_eq!(status.endpoints[1].format, Some(pcm_format(RATE)));
    assert!(samples_have_audio);
    assert!(report.blocks > 0);
}

#[test]
fn stop_interrupts_output_reconnect_backoff_promptly() {
    // This intentionally uses wall time: interrupting the output thread's real
    // reconnect wait is the property under test, not audio progression.
    let lockstep = Arc::new(Lockstep::default());
    let state = SinkState::lockstep(0, Arc::clone(&lockstep));
    state.fail_from_period.store(0, Ordering::Release);
    let capture = RecordsCapture {
        records: vec![format_record(RATE)],
        next: 0,
        trim_supported: false,
        trims: None,
        gate: None,
        paced_by: None,
        lockstep: Some(Arc::clone(&lockstep)),
        stay_open: true,
    };
    let mut output = opener(Arc::clone(&state), RATE);
    output.fail_after_first = true;
    let engine = start_with_clock(
        lockstep_config(ClockPreference::Asrc, Arc::clone(&lockstep)),
        capture,
        output,
        QPC_FREQUENCY,
    )
    .unwrap();

    // Completing the failing period puts the output thread in its first
    // reconnect backoff before any reopen can be attempted.
    lockstep.step_period(1);
    assert_eq!(engine.status().output_reconnect_attempts, 0);
    // Release the capture-side lockstep wait; the output-side wait being timed
    // is the runtime's reconnect backoff, which does not use this fixture.
    lockstep.release();
    let start = Instant::now();
    let report = engine.stop();
    assert!(start.elapsed() < Duration::from_millis(200));
    assert_eq!(report.final_status.output_reconnect_attempts, 0);
    assert!(state.stopped.load(Ordering::Acquire));
}

#[test]
fn stop_and_drop_join_threads_promptly() {
    // This intentionally uses wall time: prompt thread joining is the property
    // under test, unlike audio progression, which the other runtime tests drive
    // with Lockstep's synthetic clock.
    for drop_engine in [false, true] {
        let state = SinkState::new(0);
        let capture = RecordsCapture {
            records: vec![format_record(RATE)],
            next: 0,
            trim_supported: false,
            trims: None,
            gate: None,
            paced_by: None,
            lockstep: None,
            stay_open: false,
        };
        let engine = start_with_clock(
            config(ClockPreference::Asrc),
            capture,
            opener(Arc::clone(&state), RATE),
            QPC_FREQUENCY,
        )
        .unwrap();
        let start = Instant::now();
        if drop_engine {
            drop(engine);
        } else {
            let _ = engine.stop();
        }
        assert!(start.elapsed() < Duration::from_millis(200));
        assert!(state.stopped.load(Ordering::Acquire));
    }
}

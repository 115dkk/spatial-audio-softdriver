#![forbid(unsafe_code)]

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bio_abi::{RecordWriter, wave_format_bytes};
use bio_types::{
    CaptureError, CaptureSource, Direction, Layout, OutputError, OutputFormat, OutputOpener,
    OutputSink, PcmFormat, SampleType, SourceFormat, TrimUnsupported,
};
use brir::PreparedBrir;
use convolver::FilterBank;
use engine::{
    ClockPreference, EngineConfig, EngineEvent, LiveChange, Prepared, StartError, start_with_clock,
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

fn format_record(rate: u32) -> Vec<u8> {
    let mut record = vec![0; 256];
    let mut writer = RecordWriter::new(&mut record);
    assert!(writer.format_change(1, 0, &wave_format_bytes(&pcm_format(rate))));
    let len = writer.len();
    record.truncate(len);
    record
}

fn data_record(qpc: u64, value: f32) -> Vec<u8> {
    let mut pcm = vec![0_u8; BLOCK * 2 * 4];
    for frame in pcm.as_chunks_mut::<8>().0 {
        frame[..4].copy_from_slice(&value.to_le_bytes());
        frame[4..].copy_from_slice(&(-value).to_le_bytes());
    }
    let mut record = vec![0; bio_abi::record_size(pcm.len())];
    let mut writer = RecordWriter::new(&mut record);
    assert!(writer.data(1, qpc * 8, qpc, &pcm));
    let len = writer.len();
    record.truncate(len);
    record
}

fn prepared(rate: u32) -> Box<Prepared> {
    let ir = [1.0_f32];
    let filters = [[ir.as_slice(), ir.as_slice()]];
    let brir = Arc::new(PreparedBrir {
        layout: Layout::new(vec![Direction::new("front", 0.0, 0.0)]).unwrap(),
        bank: Arc::new(FilterBank::new(&filters, BLOCK).unwrap()),
        rate,
        partition: BLOCK,
        lfe: None,
        gain: 1.0,
        limiter_ceiling: 1.0,
        ir_frames: 1,
    });
    Box::new(Prepared::new(brir, BLOCK).unwrap())
}

struct RecordsCapture {
    records: Vec<Vec<u8>>,
    next: usize,
    trim_supported: bool,
    gate: Option<Arc<AtomicBool>>,
    /// Without it every record arrives at once, the output's priming drops
    /// what lies beyond target + one block, and QPCs written in advance no
    /// longer match when the remaining blocks play.
    paced_by: Option<Pacing>,
}

/// Hands out records like a live source, at most `lead` records ahead of the
/// sink's periods, and stamps each with the sink's frame count at that moment
/// (these tests count QPC in frames, `QPC_FREQUENCY` = `RATE`).
struct Pacing {
    state: Arc<SinkState>,
    lead: u64,
}

const RECORD_QPC: usize = std::mem::offset_of!(bio_abi::RecordHeader, qpc);

impl CaptureSource for RecordsCapture {
    fn read(&mut self, buffer: &mut [u8], _wait: Duration) -> Result<usize, CaptureError> {
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
            return Err(CaptureError::Ended);
        };
        assert!(record.len() <= buffer.len());
        buffer[..record.len()].copy_from_slice(record);
        if let Some(pacing) = &self.paced_by {
            let now = pacing.state.frames.load(Ordering::Acquire);
            buffer[RECORD_QPC..RECORD_QPC + 8].copy_from_slice(&now.to_le_bytes());
        }
        self.next += 1;
        Ok(record.len())
    }

    fn set_rate_trim(&mut self, _ppm: f64) -> Result<(), TrimUnsupported> {
        if self.trim_supported {
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
    fail: bool,
}

struct SinkState {
    frames: AtomicU64,
    periods: AtomicU64,
    samples: Mutex<Vec<f32>>,
    stopped: AtomicBool,
    delay_frames: u64,
    max_periods: AtomicU64,
}

impl SinkState {
    fn new(delay_frames: u64) -> Arc<Self> {
        Arc::new(Self {
            frames: AtomicU64::new(0),
            periods: AtomicU64::new(0),
            samples: Mutex::new(Vec::new()),
            stopped: AtomicBool::new(false),
            delay_frames,
            max_periods: AtomicU64::new(u64::MAX),
        })
    }
}

impl OutputOpener for TestOpener {
    type Sink = TestSink;

    fn open(self) -> Result<Self::Sink, OutputError> {
        if self.fail {
            return Err(OutputError::NotFound);
        }
        Ok(TestSink {
            state: self.state,
            format: self.format,
            buffer: vec![0.0; self.format.period_frames * 2],
        })
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
        if periods >= self.state.max_periods.load(Ordering::Acquire) {
            return Err(OutputError::Removed);
        }
        fill(&mut self.buffer);
        self.state
            .samples
            .lock()
            .unwrap()
            .extend_from_slice(&self.buffer);
        let frame = self
            .state
            .frames
            .fetch_add(self.format.period_frames as u64, Ordering::AcqRel);
        self.state.periods.fetch_add(1, Ordering::Release);
        std::thread::sleep(Duration::from_millis(5));
        Ok(frame + self.state.delay_frames)
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

fn opener(state: Arc<SinkState>, rate: u32) -> TestOpener {
    opener_with_period(state, rate, BLOCK)
}

fn opener_with_period(state: Arc<SinkState>, rate: u32, period_frames: usize) -> TestOpener {
    TestOpener {
        state,
        format: OutputFormat {
            rate,
            channels: 2,
            period_frames,
        },
        fail: false,
    }
}

fn wait_until(timeout: Duration, mut predicate: impl FnMut() -> bool) {
    let start = Instant::now();
    while !predicate() {
        assert!(start.elapsed() < timeout, "condition timed out");
        std::thread::yield_now();
    }
}

#[test]
fn output_open_failure_is_returned_from_start() {
    let state = SinkState::new(0);
    let capture = RecordsCapture {
        records: Vec::new(),
        next: 0,
        trim_supported: false,
        gate: None,
        paced_by: None,
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
fn required_trim_rejects_an_unsupported_capture() {
    let state = SinkState::new(0);
    let capture = RecordsCapture {
        records: Vec::new(),
        next: 0,
        trim_supported: false,
        gate: None,
        paced_by: None,
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
fn format_and_needs_brir_events_reach_poll() {
    let state = SinkState::new(0);
    let capture = RecordsCapture {
        records: vec![format_record(RATE), data_record(0, 0.25)],
        next: 0,
        trim_supported: false,
        gate: None,
        paced_by: None,
    };
    let mut engine = start_with_clock(
        config(ClockPreference::Asrc),
        capture,
        opener(state, RATE),
        QPC_FREQUENCY,
    )
    .unwrap();
    wait_until(Duration::from_secs(1), || engine.status().format.is_some());
    std::thread::sleep(Duration::from_millis(10));
    let events = engine.poll();
    assert!(events.iter().any(|event| matches!(
        event,
        EngineEvent::FormatChanged { epoch: 1, format } if *format == pcm_format(RATE)
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        EngineEvent::NeedsBrir {
            rate: RATE,
            block: BLOCK
        }
    )));
    let report = engine.stop();
    assert_eq!(report.blocks, 0);
}

#[test]
fn prepared_swap_is_retired_through_poll() {
    let state = SinkState::new(0);
    let gate = Arc::new(AtomicBool::new(false));
    let mut records = vec![format_record(RATE)];
    for block in 0..8_u64 {
        records.push(data_record(block * BLOCK as u64, 0.25));
    }
    let capture = RecordsCapture {
        records,
        next: 0,
        trim_supported: true,
        gate: Some(Arc::clone(&gate)),
        paced_by: None,
    };
    let mut engine = start_with_clock(
        config(ClockPreference::Trim),
        capture,
        opener(state, RATE),
        QPC_FREQUENCY,
    )
    .unwrap();
    engine.apply(LiveChange::Prepared(prepared(RATE))).unwrap();
    gate.store(true, Ordering::Release);
    wait_until(Duration::from_secs(1), || engine.status().format.is_some());
    engine.apply(LiveChange::Prepared(prepared(RATE))).unwrap();
    std::thread::sleep(Duration::from_millis(10));
    let _ = engine.poll();
    let _ = engine.stop();
}

#[test]
fn latency_uses_playback_qpc_and_source_qpc() {
    let delay = 960_u64;
    let state = SinkState::new(delay);
    let mut records = vec![format_record(RATE)];
    for block in 0..20_u64 {
        records.push(data_record(block * BLOCK as u64, 0.25));
    }
    let capture = RecordsCapture {
        records,
        next: 0,
        trim_supported: true,
        gate: None,
        paced_by: Some(Pacing {
            state: Arc::clone(&state),
            lead: 0,
        }),
    };
    let engine = start_with_clock(
        config(ClockPreference::Trim),
        capture,
        opener(Arc::clone(&state), RATE),
        QPC_FREQUENCY,
    )
    .unwrap();
    engine.apply(LiveChange::Prepared(prepared(RATE))).unwrap();
    wait_until(Duration::from_secs(1), || {
        engine.status().latency.samples > 0
    });
    wait_until(Duration::from_secs(2), || {
        engine.status().latency.samples >= 5
    });
    // The sink plays each period 960 frames (20 ms) after its playback QPC,
    // and a block waits in the ring behind the primed level: the 10 ms target
    // (30 ms in all, as measured), at most target + one block + one period.
    let status = engine.status();
    assert!(
        (25.0..=55.0).contains(&status.latency.p50_ms),
        "latency was {:?}",
        status.latency
    );
    let _ = engine.stop();
}

#[test]
fn mute_changes_samples_by_at_most_one_ramp_increment() {
    let state = SinkState::new(0);
    let mut records = vec![format_record(RATE)];
    for block in 0..80_u64 {
        records.push(data_record(block * BLOCK as u64, 0.5));
    }
    let capture = RecordsCapture {
        records,
        next: 0,
        trim_supported: true,
        gate: None,
        paced_by: Some(Pacing {
            state: Arc::clone(&state),
            lead: 4,
        }),
    };
    let engine = start_with_clock(
        config(ClockPreference::Trim),
        capture,
        opener(Arc::clone(&state), RATE),
        QPC_FREQUENCY,
    )
    .unwrap();
    engine.apply(LiveChange::Prepared(prepared(RATE))).unwrap();
    wait_until(Duration::from_secs(1), || {
        state.periods.load(Ordering::Acquire) >= 3
    });
    engine.apply(LiveChange::Mute(true)).unwrap();
    wait_until(Duration::from_secs(1), || {
        state.periods.load(Ordering::Acquire) >= 8
    });
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
    let gate = Arc::new(AtomicBool::new(false));
    let state = SinkState::new(0);
    state.max_periods.store(1_000, Ordering::Release);
    let mut records = vec![format_record(RATE)];
    for block in 0..1_000_u64 {
        records.push(data_record(block * BLOCK as u64, 0.25));
    }
    let capture = RecordsCapture {
        records,
        next: 0,
        trim_supported: false,
        gate: Some(Arc::clone(&gate)),
        paced_by: None,
    };
    let engine = start_with_clock(
        EngineConfig {
            clock: ClockPreference::Asrc,
            target_ms: 40.0,
            capacity_ms: 120.0,
            read_wait: Duration::ZERO,
            ..EngineConfig::default()
        },
        capture,
        opener_with_period(Arc::clone(&state), 44_100, 441),
        QPC_FREQUENCY,
    )
    .unwrap();
    engine.apply(LiveChange::Prepared(prepared(RATE))).unwrap();
    gate.store(true, Ordering::Release);
    wait_until(Duration::from_secs(8), || {
        state.periods.load(Ordering::Acquire) >= 1_000
    });
    let _ = engine.stop();
    let produced = state.frames.load(Ordering::Acquire);
    let expected = 441_000_u64;
    let error = produced.abs_diff(expected) as f64 / expected as f64;
    assert!(error <= 0.001, "produced {produced}, expected {expected}");
}

#[test]
fn stop_and_drop_join_threads_promptly() {
    for drop_engine in [false, true] {
        let state = SinkState::new(0);
        let capture = RecordsCapture {
            records: vec![format_record(RATE)],
            next: 0,
            trim_supported: false,
            gate: None,
            paced_by: None,
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

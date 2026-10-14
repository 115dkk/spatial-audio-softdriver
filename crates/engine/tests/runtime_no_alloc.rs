#![forbid(unsafe_code)]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use assert_no_alloc::AllocDisabler;
use brir::PreparedBrir;
use convolver::FilterBank;
use engine::{ClockPreference, EngineConfig, LiveChange, Prepared, RtSection, start_with_clock};
use speakers::{PreparedSpeaker, PreparedSpeakers};
use ssr_abi::{RecordWriter, VolumePayload, wave_format_bytes};
use ssr_types::{
    CaptureError, CaptureSource, Direction, Layout, OutputError, OutputFormat, OutputLayout,
    OutputOpener, OutputSink, PcmFormat, SampleType, SourceFormat, TrimUnsupported,
};

#[global_allocator]
static ALLOCATOR: AllocDisabler = AllocDisabler;

const RATE: u32 = 48_000;
const BLOCK: usize = 480;
const WARMUP: u64 = 8;

fn records(blocks: usize) -> Vec<Vec<u8>> {
    let format = SourceFormat::Pcm(PcmFormat {
        channels: 2,
        sample_rate: RATE,
        bits_per_sample: 32,
        valid_bits: 32,
        sample_type: SampleType::Float,
        channel_mask: 3,
    });
    let mut records = Vec::with_capacity(blocks + 1);
    let mut format_record = vec![0; 256];
    let mut writer = RecordWriter::new(&mut format_record);
    assert!(writer.format_change(1, 0, &wave_format_bytes(&format)));
    let len = writer.len();
    format_record.truncate(len);
    records.push(format_record);

    for block in 0..blocks {
        let mut pcm = vec![0_u8; BLOCK * 8];
        for frame in pcm.as_chunks_mut::<8>().0 {
            frame[..4].copy_from_slice(&0.1_f32.to_le_bytes());
            frame[4..].copy_from_slice(&0.1_f32.to_le_bytes());
        }
        let mut record = vec![0; ssr_abi::record_size(pcm.len())];
        let mut writer = RecordWriter::new(&mut record);
        assert!(writer.data(1, (block * BLOCK * 8) as u64, (block * BLOCK) as u64, &pcm));
        let len = writer.len();
        record.truncate(len);
        records.push(record);
        if block == WARMUP as usize {
            let mut levels = [0; ssr_abi::MAX_VOLUME_CHANNELS];
            levels[..2].fill(-6 * 65_536);
            let payload = VolumePayload {
                channel_count: 2,
                mute_mask: 0,
                level_q16: levels,
            };
            let mut volume = vec![0; ssr_abi::record_size(std::mem::size_of::<VolumePayload>())];
            let mut writer = RecordWriter::new(&mut volume);
            assert!(writer.volume((block * BLOCK) as u64, false, &payload));
            let len = writer.len();
            volume.truncate(len);
            records.push(volume);
        }
    }
    records
}

fn prepared(channels: usize) -> Box<Prepared> {
    if channels == 2 {
        let ir = [1.0_f32];
        let filters = [[ir.as_slice(), ir.as_slice()]];
        let brir = Arc::new(PreparedBrir {
            layout: Layout::new(vec![Direction::new("front", 0.0, 0.0)]).unwrap(),
            bank: Arc::new(FilterBank::new(&filters, BLOCK).unwrap()),
            rate: RATE,
            partition: BLOCK,
            lfe: None,
            gain: 1.0,
            limiter_ceiling: 1.0,
            ir_frames: 1,
            omitted_silent_directions: Vec::new(),
            trimmed_leading_frames: 0,
        });
        return Box::new(Prepared::new(brir, BLOCK).unwrap());
    }
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
                channel_mask: 0,
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

struct Capture {
    records: Vec<Vec<u8>>,
    next: usize,
    gate: Arc<AtomicBool>,
    periods: Arc<AtomicU64>,
    now_qpc: u64,
    processed: Arc<AtomicU64>,
    fail_once_at: Option<usize>,
    failed: bool,
    reconnects: Arc<AtomicU64>,
    end_after_records: bool,
    end_gate: Option<Arc<AtomicBool>>,
}

impl CaptureSource for Capture {
    fn read(&mut self, buffer: &mut [u8], _wait: Duration) -> Result<usize, CaptureError> {
        if self.fail_once_at == Some(self.next) && !self.failed {
            self.failed = true;
            return Err(CaptureError::NotPresent);
        }
        if !self.gate.load(Ordering::Acquire) {
            std::thread::yield_now();
            return Ok(0);
        }
        if self.next > self.periods.load(Ordering::Acquire) as usize + 16 {
            std::thread::yield_now();
            return Ok(0);
        }
        let Some(record) = self.records.get(self.next) else {
            return if self.end_after_records
                && self
                    .end_gate
                    .as_ref()
                    .is_none_or(|gate| gate.load(Ordering::Acquire))
            {
                Err(CaptureError::Ended)
            } else {
                Ok(0)
            };
        };
        buffer[..record.len()].copy_from_slice(record);
        self.next += 1;
        self.processed.fetch_add(1, Ordering::Release);
        Ok(record.len())
    }

    fn now_qpc(&self) -> Option<u64> {
        Some(self.now_qpc)
    }

    fn set_rate_trim(&mut self, _ppm: f64) -> Result<(), TrimUnsupported> {
        Ok(())
    }

    fn reconnect(&mut self) -> Result<(), CaptureError> {
        let _outside_scope_allocation = Vec::<u8>::with_capacity(1);
        self.reconnects.fetch_add(1, Ordering::Release);
        Ok(())
    }
}

struct Opener {
    periods: Arc<AtomicU64>,
    rate: u32,
    period_frames: usize,
    emitted: Arc<AtomicU64>,
    fail_drain: Arc<AtomicBool>,
}

impl OutputOpener for Opener {
    type Sink = Sink;

    fn open(&mut self, layout: OutputLayout) -> Result<Self::Sink, OutputError> {
        Ok(Sink {
            periods: Arc::clone(&self.periods),
            rate: self.rate,
            period_frames: self.period_frames,
            layout,
            samples: vec![0.0; self.period_frames * usize::from(layout.channels)],
            emitted: Arc::clone(&self.emitted),
            fail_drain: Arc::clone(&self.fail_drain),
        })
    }
}

struct Sink {
    periods: Arc<AtomicU64>,
    rate: u32,
    period_frames: usize,
    layout: OutputLayout,
    samples: Vec<f32>,
    emitted: Arc<AtomicU64>,
    fail_drain: Arc<AtomicBool>,
}

impl OutputSink for Sink {
    fn format(&self) -> OutputFormat {
        OutputFormat {
            rate: self.rate,
            channels: self.layout.channels,
            channel_mask: self.layout.channel_mask,
            period_frames: self.period_frames,
            mode: ssr_types::OutputMode::Exclusive,
            fell_back: false,
            mix_channels: self.layout.channels,
        }
    }

    fn next_period(&mut self, fill: &mut dyn FnMut(&mut [f32])) -> Result<u64, OutputError> {
        fill(&mut self.samples);
        if self.fail_drain.swap(false, Ordering::AcqRel) {
            return Err(OutputError::Removed);
        }
        if self.samples.iter().any(|sample| *sample != 0.0) {
            self.emitted.fetch_add(1, Ordering::Release);
        }
        let period = self.periods.fetch_add(1, Ordering::AcqRel);
        std::thread::yield_now();
        Ok(period * self.period_frames as u64)
    }
}

fn run_no_alloc(
    clock: ClockPreference,
    output_rate: u32,
    period_frames: usize,
    channels: usize,
    reconnect: bool,
) {
    let periods = Arc::new(AtomicU64::new(0));
    let processed = Arc::new(AtomicU64::new(0));
    let emitted = Arc::new(AtomicU64::new(0));
    let gate = Arc::new(AtomicBool::new(false));
    let reconnects = Arc::new(AtomicU64::new(0));
    let fail_drain = Arc::new(AtomicBool::new(false));
    let hook_periods = Arc::clone(&periods);
    let hook_processed = Arc::clone(&processed);
    let config = EngineConfig {
        clock,
        target_ms: 10.0,
        capacity_ms: 80.0,
        read_wait: Duration::ZERO,
        rt_scope: Some(Arc::new(move |section, work| {
            let armed = match section {
                RtSection::Capture => hook_processed.load(Ordering::Acquire) >= WARMUP,
                RtSection::Output => hook_periods.load(Ordering::Acquire) >= WARMUP,
            };
            if armed {
                assert_no_alloc::assert_no_alloc(work);
            } else {
                work();
            }
        })),
        ..EngineConfig::default()
    };
    let capture = Capture {
        records: records(32),
        next: 0,
        gate: Arc::clone(&gate),
        periods: Arc::clone(&periods),
        now_qpc: (40 * BLOCK) as u64,
        processed: Arc::clone(&processed),
        fail_once_at: reconnect.then_some(WARMUP as usize + 2),
        failed: false,
        reconnects: Arc::clone(&reconnects),
        end_after_records: false,
        end_gate: None,
    };
    let engine = start_with_clock(
        config,
        capture,
        Opener {
            periods: Arc::clone(&periods),
            rate: output_rate,
            period_frames,
            emitted: Arc::clone(&emitted),
            fail_drain,
        },
        RATE as u64,
    )
    .unwrap();
    engine
        .apply(LiveChange::Prepared(prepared(channels)))
        .unwrap();
    gate.store(true, Ordering::Release);
    let start = std::time::Instant::now();
    while (periods.load(Ordering::Acquire) < 12
        || processed.load(Ordering::Acquire) < WARMUP
        || reconnect && reconnects.load(Ordering::Acquire) == 0)
        && start.elapsed() < Duration::from_secs(5)
    {
        std::thread::yield_now();
    }
    gate.store(false, Ordering::Release);
    while emitted.load(Ordering::Acquire) == 0 && start.elapsed() < Duration::from_secs(5) {
        std::thread::yield_now();
    }
    let observed_periods = periods.load(Ordering::Acquire);
    let status = engine.status();
    let report = engine.stop();
    assert!(
        observed_periods >= 12,
        "{clock:?} at {output_rate} Hz stopped after {observed_periods} periods: status={status:?}, report={report:?}"
    );
    assert!(processed.load(Ordering::Acquire) >= WARMUP);
    assert_eq!(reconnects.load(Ordering::Acquire), u64::from(reconnect));
    assert!(
        emitted.load(Ordering::Acquire) > 0,
        "{clock:?} at {output_rate} Hz emitted no audio: processed={}, periods={observed_periods}, status={status:?}, report={report:?}",
        processed.load(Ordering::Acquire)
    );
}

#[test]
fn eof_drain_sink_failure_returns_from_the_allocation_free_scope() {
    let periods = Arc::new(AtomicU64::new(0));
    let processed = Arc::new(AtomicU64::new(0));
    let emitted = Arc::new(AtomicU64::new(0));
    let gate = Arc::new(AtomicBool::new(false));
    let reconnects = Arc::new(AtomicU64::new(0));
    let fail_drain = Arc::new(AtomicBool::new(false));
    let end_gate = Arc::new(AtomicBool::new(false));
    let event_processed = Arc::clone(&processed);
    let event_end_gate = Arc::clone(&end_gate);
    let event_fail_drain = Arc::clone(&fail_drain);
    let config = EngineConfig {
        clock: ClockPreference::Asrc,
        target_ms: 0.0,
        capacity_ms: 80.0,
        read_wait: Duration::ZERO,
        rt_scope: Some(Arc::new(move |_section, work| {
            assert_no_alloc::assert_no_alloc(work);
        })),
        rt_event: Some(Arc::new(move |event| {
            if event == engine::RtLoopEvent::OutputEnd
                && event_processed.load(Ordering::Acquire) >= 2
            {
                event_end_gate.store(true, Ordering::Release);
            }
            // Only the tail written after the last rendered period fails.
            if event == engine::RtLoopEvent::OutputTailBegin {
                event_fail_drain.store(true, Ordering::Release);
            }
        })),
        ..EngineConfig::default()
    };
    let capture = Capture {
        records: records(1),
        next: 0,
        gate: Arc::clone(&gate),
        periods: Arc::clone(&periods),
        now_qpc: (2 * BLOCK) as u64,
        processed: Arc::clone(&processed),
        fail_once_at: None,
        failed: false,
        reconnects,
        end_after_records: true,
        end_gate: Some(end_gate),
    };
    let engine = start_with_clock(
        config,
        capture,
        Opener {
            periods: Arc::clone(&periods),
            rate: 44_100,
            period_frames: 441,
            emitted,
            fail_drain: Arc::clone(&fail_drain),
        },
        RATE as u64,
    )
    .unwrap();
    engine.apply(LiveChange::Prepared(prepared(2))).unwrap();
    gate.store(true, Ordering::Release);

    let start = std::time::Instant::now();
    while engine.status().fault.is_none() && start.elapsed() < Duration::from_secs(5) {
        std::thread::yield_now();
    }
    let status = engine.status();
    let report = engine.stop();
    assert!(
        !fail_drain.load(Ordering::Acquire),
        "the end of capture wrote no tail for the sink to fail"
    );
    assert_eq!(
        status.fault,
        Some(ssr_types::EngineFault::OutputLost(
            "the output device was removed".into()
        )),
        "terminal drain failure was not published outside the realtime scope: {status:?}, {report:?}"
    );
}

#[test]
fn volume_record_and_ramp_do_not_allocate() {
    run_no_alloc(ClockPreference::Trim, RATE, BLOCK, 2, false);
}

#[test]
fn capture_and_output_steady_state_do_not_allocate() {
    run_no_alloc(ClockPreference::Trim, RATE, BLOCK, 2, false);
    run_no_alloc(ClockPreference::Asrc, 44_100, 441, 2, false);
}

#[test]
fn twelve_channel_output_steady_state_does_not_allocate() {
    run_no_alloc(ClockPreference::Trim, RATE, BLOCK, 12, false);
    run_no_alloc(ClockPreference::Asrc, 44_100, 441, 12, false);
}

#[test]
fn capture_reconnect_and_layout_change_stay_outside_the_allocation_free_scope() {
    run_no_alloc(ClockPreference::Trim, RATE, BLOCK, 12, true);
    run_no_alloc(ClockPreference::Asrc, 44_100, 441, 12, false);
}

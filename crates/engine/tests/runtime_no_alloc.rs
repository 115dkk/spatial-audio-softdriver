#![forbid(unsafe_code)]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use assert_no_alloc::AllocDisabler;
use bio_abi::{RecordWriter, wave_format_bytes};
use bio_types::{
    CaptureError, CaptureSource, Direction, Layout, OutputError, OutputFormat, OutputOpener,
    OutputSink, PcmFormat, SampleType, SourceFormat, TrimUnsupported,
};
use brir::PreparedBrir;
use convolver::FilterBank;
use engine::{ClockPreference, EngineConfig, LiveChange, Prepared, start_with_clock};

#[global_allocator]
static ALLOCATOR: AllocDisabler = AllocDisabler;

const RATE: u32 = 48_000;
const BLOCK: usize = 480;

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
            frame[4..].copy_from_slice(&(-0.1_f32).to_le_bytes());
        }
        let mut record = vec![0; bio_abi::record_size(pcm.len())];
        let mut writer = RecordWriter::new(&mut record);
        assert!(writer.data(1, (block * BLOCK * 8) as u64, (block * BLOCK) as u64, &pcm));
        let len = writer.len();
        record.truncate(len);
        records.push(record);
    }
    records
}

fn prepared() -> Box<Prepared> {
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
    });
    Box::new(Prepared::new(brir, BLOCK).unwrap())
}

struct Capture {
    records: Vec<Vec<u8>>,
    next: usize,
    checked: bool,
    gate: Arc<AtomicBool>,
}

impl CaptureSource for Capture {
    fn read(&mut self, buffer: &mut [u8], _wait: Duration) -> Result<usize, CaptureError> {
        if !self.gate.load(Ordering::Acquire) {
            std::thread::yield_now();
            return Ok(0);
        }
        let copy = |this: &mut Self, buffer: &mut [u8]| {
            let Some(record) = this.records.get(this.next) else {
                return Err(CaptureError::Ended);
            };
            buffer[..record.len()].copy_from_slice(record);
            this.next += 1;
            Ok(record.len())
        };
        if self.next >= 8 && !self.checked {
            self.checked = true;
            assert_no_alloc::assert_no_alloc(|| copy(self, buffer))
        } else {
            copy(self, buffer)
        }
    }

    fn set_rate_trim(&mut self, _ppm: f64) -> Result<(), TrimUnsupported> {
        Ok(())
    }
}

struct Opener {
    periods: Arc<AtomicU64>,
    rate: u32,
    period_frames: usize,
}

impl OutputOpener for Opener {
    type Sink = Sink;

    fn open(self) -> Result<Self::Sink, OutputError> {
        Ok(Sink {
            periods: self.periods,
            rate: self.rate,
            period_frames: self.period_frames,
            samples: vec![0.0; self.period_frames * 2],
        })
    }
}

struct Sink {
    periods: Arc<AtomicU64>,
    rate: u32,
    period_frames: usize,
    samples: Vec<f32>,
}

impl OutputSink for Sink {
    fn format(&self) -> OutputFormat {
        OutputFormat {
            rate: self.rate,
            channels: 2,
            period_frames: self.period_frames,
        }
    }

    fn next_period(&mut self, fill: &mut dyn FnMut(&mut [f32])) -> Result<u64, OutputError> {
        if self.periods.load(Ordering::Acquire) < 8 {
            fill(&mut self.samples);
        } else {
            assert_no_alloc::assert_no_alloc(|| fill(&mut self.samples));
        }
        let period = self.periods.fetch_add(1, Ordering::AcqRel);
        std::thread::sleep(Duration::from_millis(1));
        Ok(period * self.period_frames as u64)
    }
}

fn run_no_alloc(clock: ClockPreference, output_rate: u32, period_frames: usize) {
    let periods = Arc::new(AtomicU64::new(0));
    let gate = Arc::new(AtomicBool::new(false));
    let config = EngineConfig {
        clock,
        target_ms: 10.0,
        capacity_ms: 80.0,
        read_wait: Duration::ZERO,
        thread_hook: None,
        ..EngineConfig::default()
    };
    let capture = Capture {
        records: records(32),
        next: 0,
        checked: false,
        gate: Arc::clone(&gate),
    };
    let engine = start_with_clock(
        config,
        capture,
        Opener {
            periods: Arc::clone(&periods),
            rate: output_rate,
            period_frames,
        },
        RATE as u64,
    )
    .unwrap();
    engine.apply(LiveChange::Prepared(prepared())).unwrap();
    gate.store(true, Ordering::Release);
    let start = std::time::Instant::now();
    while periods.load(Ordering::Acquire) < 12 && start.elapsed() < Duration::from_secs(1) {
        std::thread::yield_now();
    }
    assert!(periods.load(Ordering::Acquire) >= 12);
    let _ = engine.stop();
}

#[test]
fn capture_and_output_steady_state_do_not_allocate() {
    run_no_alloc(ClockPreference::Trim, RATE, BLOCK);
    run_no_alloc(ClockPreference::Asrc, 44_100, 441);
}

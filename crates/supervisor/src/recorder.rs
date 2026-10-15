use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use ssr_abi::RecordWriter;
use ssr_types::{CaptureError, CaptureSource, TrimUnsupported};

const DEFAULT_RING_BYTES: usize = 8 << 20;
const READ_END_BYTES: usize = ssr_abi::READ_END_RECORD_BYTES;

#[derive(Clone, Debug)]
pub struct RecorderConfig {
    pub path: PathBuf,
    pub now: fn() -> u64,
}

fn marker(
    write: impl FnOnce(&mut RecordWriter<'_>) -> bool,
) -> [u8; ssr_abi::RECORDER_GAP_RECORD_BYTES] {
    let mut bytes = [0; ssr_abi::RECORDER_GAP_RECORD_BYTES];
    let mut writer = RecordWriter::new(&mut bytes);
    assert!(write(&mut writer));
    bytes
}

fn read_end_marker(qpc: u64) -> [u8; ssr_abi::RECORDER_GAP_RECORD_BYTES] {
    marker(|writer| writer.read_end(qpc))
}

fn recorder_gap_marker(qpc: u64, lost_bytes: u64) -> [u8; ssr_abi::RECORDER_GAP_RECORD_BYTES] {
    marker(|writer| writer.recorder_gap(qpc, lost_bytes))
}

/// Copies capture READs into a lock-free byte ring drained by a file writer.
pub struct TeeCapture<C> {
    inner: C,
    tap: Option<rtrb::Producer<u8>>,
    dropped: Arc<AtomicU64>,
    pending_gap: u64,
    now: fn() -> u64,
    stats_before_reconnect: Option<Box<dyn FnMut() -> Result<(), CaptureError> + Send>>,
    stats_after_reconnect: Option<Box<dyn FnMut() -> Result<(), CaptureError> + Send>>,
}

impl<C> std::fmt::Debug for TeeCapture<C> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TeeCapture")
            .field("recording", &self.tap.is_some())
            .field("pending_gap", &self.pending_gap)
            .finish_non_exhaustive()
    }
}

impl<C> TeeCapture<C> {
    pub fn new(inner: C, now: fn() -> u64) -> Self {
        Self::without_recorder(inner, now)
    }

    pub(crate) fn without_recorder(inner: C, now: fn() -> u64) -> Self {
        Self {
            inner,
            tap: None,
            dropped: Arc::new(AtomicU64::new(0)),
            pending_gap: 0,
            now,
            stats_before_reconnect: None,
            stats_after_reconnect: None,
        }
    }

    pub(crate) fn with_reconnect_observers(
        mut self,
        before: impl FnMut() -> Result<(), CaptureError> + Send + 'static,
        after: impl FnMut() -> Result<(), CaptureError> + Send + 'static,
    ) -> Self {
        self.stats_before_reconnect = Some(Box::new(before));
        self.stats_after_reconnect = Some(Box::new(after));
        self
    }
}

impl<C: CaptureSource> CaptureSource for TeeCapture<C> {
    fn read(&mut self, buf: &mut [u8], wait: Duration) -> Result<usize, CaptureError> {
        let n = self.inner.read(buf, wait)?;
        if let Some(tap) = self.tap.as_mut() {
            let now = self.inner.now_qpc().unwrap_or_else(self.now);
            let end = read_end_marker(now);
            let gap = recorder_gap_marker(now, self.pending_gap);
            let gap_len = usize::from(self.pending_gap != 0) * gap.len();
            let total = gap_len + n + READ_END_BYTES;
            match tap.write_chunk_uninit(total) {
                Ok(chunk) => {
                    chunk.fill_from_iter(
                        gap[..gap_len]
                            .iter()
                            .chain(&buf[..n])
                            .chain(&end[..READ_END_BYTES])
                            .copied(),
                    );
                    self.pending_gap = 0;
                }
                Err(_) => {
                    self.pending_gap = self.pending_gap.saturating_add(n as u64);
                    self.dropped.fetch_add(n as u64, Ordering::Relaxed);
                }
            }
        }
        Ok(n)
    }

    fn now_qpc(&self) -> Option<u64> {
        self.inner.now_qpc()
    }

    fn set_rate_trim(&mut self, ppm: f64) -> Result<(), TrimUnsupported> {
        self.inner.set_rate_trim(ppm)
    }

    fn reconnect(&mut self) -> Result<(), CaptureError> {
        if let Some(before) = self.stats_before_reconnect.as_mut() {
            let _ = before();
        }
        self.inner.reconnect()?;
        if let Some(after) = self.stats_after_reconnect.as_mut() {
            after()?;
        }
        Ok(())
    }
}

/// The writer thread that drains the byte ring to the dump file. Stopping or
/// dropping it writes out everything already handed to the ring and joins
/// the thread, so a capture that ends in an error still leaves a whole dump.
pub struct Recorder {
    stopped: Arc<AtomicBool>,
    dropped: Arc<AtomicU64>,
    writer: Option<JoinHandle<Result<(), String>>>,
}

impl std::fmt::Debug for Recorder {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Recorder")
            .field("dropped", &self.dropped.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

impl Recorder {
    pub fn start<C>(
        config: RecorderConfig,
        qpc_frequency: u64,
        capture: TeeCapture<C>,
    ) -> Result<(TeeCapture<C>, Self), String> {
        Self::start_with_capacity(config, qpc_frequency, capture, DEFAULT_RING_BYTES)
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn start_with_capacity<C>(
        config: RecorderConfig,
        qpc_frequency: u64,
        mut capture: TeeCapture<C>,
        capacity: usize,
    ) -> Result<(TeeCapture<C>, Self), String> {
        let file = File::create(&config.path)
            .map_err(|error| format!("{}: {error}", config.path.display()))?;
        let mut writer = BufWriter::with_capacity(1 << 20, file);
        writer
            .write_all(&ssr_abi::dump_header_bytes(qpc_frequency))
            .map_err(|error| error.to_string())?;
        let (producer, mut consumer) = rtrb::RingBuffer::<u8>::new(capacity);
        capture.tap = Some(producer);
        capture.now = config.now;
        let stopped = Arc::new(AtomicBool::new(false));
        let writer_stopped = Arc::clone(&stopped);
        let handle = thread::spawn(move || -> Result<(), String> {
            loop {
                let available = consumer.slots();
                if available > 0 {
                    let chunk = consumer
                        .read_chunk(available)
                        .map_err(|error| format!("{error:?}"))?;
                    let (a, b) = chunk.as_slices();
                    writer.write_all(a).map_err(|error| error.to_string())?;
                    writer.write_all(b).map_err(|error| error.to_string())?;
                    chunk.commit_all();
                } else if writer_stopped.load(Ordering::Acquire) {
                    break;
                } else {
                    thread::sleep(Duration::from_millis(5));
                }
            }
            writer.flush().map_err(|error| error.to_string())
        });
        let dropped = Arc::clone(&capture.dropped);
        Ok((
            capture,
            Self {
                stopped,
                dropped,
                writer: Some(handle),
            },
        ))
    }

    pub fn dropped_bytes(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    pub fn stop(mut self) -> Result<(), String> {
        self.finish()
    }

    fn finish(&mut self) -> Result<(), String> {
        let Some(writer) = self.writer.take() else {
            return Ok(());
        };
        self.stopped.store(true, Ordering::Release);
        writer
            .join()
            .map_err(|_| "the dump writer panicked".to_owned())?
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        // An early return (a capture error) must not lose what was read.
        let _ = self.finish();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;
    use std::time::{SystemTime, UNIX_EPOCH};

    use assert_no_alloc::{AllocDisabler, assert_no_alloc};
    use engine::{Chain, ChainConfig, ChainEvent, Prepared};
    use speakers::{PreparedSpeaker, PreparedSpeakers};
    use ssr_abi::{dump_header, reads, wave_format_bytes};
    use ssr_types::{Direction, Layout, PcmFormat, SampleType, SourceFormat};

    use super::*;

    #[global_allocator]
    static ALLOCATOR: AllocDisabler = AllocDisabler;

    struct Capture {
        reads: Vec<Vec<u8>>,
        next: usize,
    }

    impl CaptureSource for Capture {
        fn read(&mut self, buf: &mut [u8], _wait: Duration) -> Result<usize, CaptureError> {
            let Some(read) = self.reads.get(self.next) else {
                return Err(CaptureError::Ended);
            };
            buf[..read.len()].copy_from_slice(read);
            self.next += 1;
            Ok(read.len())
        }

        fn now_qpc(&self) -> Option<u64> {
            Some(self.next as u64)
        }

        fn set_rate_trim(&mut self, _ppm: f64) -> Result<(), TrimUnsupported> {
            Err(TrimUnsupported)
        }
    }

    fn now() -> u64 {
        7
    }

    fn data(value: u8) -> Vec<u8> {
        let mut bytes = vec![0; 128];
        let mut writer = RecordWriter::new(&mut bytes);
        assert!(writer.data(1, 0, 0, &[value; 8]));
        let len = writer.len();
        bytes.truncate(len);
        bytes
    }

    fn temp_dump(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "supervisor-{name}-{}-{}.ssrdump",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    fn file_reads(path: &std::path::Path) -> Vec<(Vec<u8>, Option<u64>)> {
        let bytes = std::fs::read(path).unwrap();
        let (header, stream) = ssr_abi::split_dump(&bytes).unwrap();
        ssr_abi::reads(&header, stream)
            .map(|read| (read.bytes.to_vec(), read.end_qpc))
            .collect()
    }

    // The byte ring is large enough for every read, so nothing depends on
    // when the writer thread runs; overflow is covered with a hand-driven
    // consumer (tee_capture_preserves_reads_and_records_dropped_bytes).
    #[test]
    fn recorder_writes_every_read_with_its_end_marker() {
        let path = temp_dump("recorder");
        let reads = vec![data(1), data(2), data(3)];
        let tee = TeeCapture::without_recorder(
            Capture {
                reads: reads.clone(),
                next: 0,
            },
            now,
        );
        let (mut tee, recorder) = Recorder::start(
            RecorderConfig {
                path: path.clone(),
                now,
            },
            1_000,
            tee,
        )
        .unwrap();
        let mut buffer = [0; 128];
        for _ in 0..3 {
            tee.read(&mut buffer, Duration::ZERO).unwrap();
        }
        assert_eq!(recorder.dropped_bytes(), 0);
        drop(tee);
        recorder.stop().unwrap();

        let written = file_reads(&path);
        std::fs::remove_file(&path).unwrap();
        assert_eq!(
            written,
            reads
                .into_iter()
                .zip(1..)
                .map(|(bytes, qpc)| (bytes, Some(qpc)))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_recorder_dropped_on_a_capture_error_keeps_what_was_read() {
        let path = temp_dump("recorder-error");
        let tee = TeeCapture::without_recorder(
            Capture {
                reads: vec![data(1)],
                next: 0,
            },
            now,
        );
        let (mut tee, recorder) = Recorder::start(
            RecorderConfig {
                path: path.clone(),
                now,
            },
            1_000,
            tee,
        )
        .unwrap();
        let mut buffer = [0; 128];
        tee.read(&mut buffer, Duration::ZERO).unwrap();
        let failed = tee.read(&mut buffer, Duration::ZERO);
        assert!(matches!(failed, Err(CaptureError::Ended)));
        // As `record` does when a read fails: return without stopping.
        drop(recorder);

        let written = file_reads(&path);
        std::fs::remove_file(&path).unwrap();
        assert_eq!(written, vec![(data(1), Some(1))]);
    }

    #[test]
    fn tee_read_does_not_allocate() {
        let (producer, _consumer) = rtrb::RingBuffer::new(256);
        let dropped = Arc::new(AtomicU64::new(0));
        let mut tee = TeeCapture {
            inner: Capture {
                reads: vec![data(1)],
                next: 0,
            },
            tap: Some(producer),
            dropped,
            pending_gap: 0,
            now,
            stats_before_reconnect: None,
            stats_after_reconnect: None,
        };
        let mut buffer = [0; 128];
        assert_no_alloc(|| {
            tee.read(&mut buffer, Duration::ZERO).unwrap();
        });
        assert_eq!(tee.dropped.load(Ordering::Relaxed), 0);
    }

    struct FixedCapture {
        next: u8,
    }

    struct ScriptedCapture {
        reads: Vec<Vec<u8>>,
        next: usize,
        qpc: u64,
    }

    impl CaptureSource for ScriptedCapture {
        fn read(&mut self, buf: &mut [u8], _wait: Duration) -> Result<usize, CaptureError> {
            let Some(read) = self.reads.get(self.next) else {
                return Err(CaptureError::Ended);
            };
            buf[..read.len()].copy_from_slice(read);
            self.next += 1;
            self.qpc += 10_000;
            Ok(read.len())
        }

        fn now_qpc(&self) -> Option<u64> {
            Some(self.qpc)
        }

        fn set_rate_trim(&mut self, _ppm: f64) -> Result<(), TrimUnsupported> {
            Err(TrimUnsupported)
        }
    }

    impl CaptureSource for FixedCapture {
        fn read(&mut self, buf: &mut [u8], _wait: Duration) -> Result<usize, CaptureError> {
            if self.next == 0 {
                self.next = 1;
                return Ok(0);
            }
            let mut writer = RecordWriter::new(buf);
            assert!(writer.data(1, u64::from(self.next) * 8, 1, &[self.next; 8]));
            self.next += 1;
            Ok(writer.len())
        }

        fn now_qpc(&self) -> Option<u64> {
            Some(456)
        }

        fn set_rate_trim(&mut self, _ppm: f64) -> Result<(), TrimUnsupported> {
            Err(TrimUnsupported)
        }
    }

    fn fixed_now() -> u64 {
        123
    }

    fn drain(consumer: &mut rtrb::Consumer<u8>, out: &mut Vec<u8>) {
        let available = consumer.slots();
        let chunk = consumer.read_chunk(available).unwrap();
        let (a, b) = chunk.as_slices();
        out.extend_from_slice(a);
        out.extend_from_slice(b);
        chunk.commit_all();
    }

    fn tee_from(
        capacity: usize,
        next: u8,
    ) -> (TeeCapture<FixedCapture>, rtrb::Consumer<u8>, Arc<AtomicU64>) {
        let (producer, consumer) = rtrb::RingBuffer::new(capacity);
        let dropped = Arc::new(AtomicU64::new(0));
        (
            TeeCapture {
                inner: FixedCapture { next },
                tap: Some(producer),
                dropped: dropped.clone(),
                pending_gap: 0,
                now: fixed_now,
                stats_before_reconnect: None,
                stats_after_reconnect: None,
            },
            consumer,
            dropped,
        )
    }

    fn tee(capacity: usize) -> (TeeCapture<FixedCapture>, rtrb::Consumer<u8>, Arc<AtomicU64>) {
        tee_from(capacity, 1)
    }

    #[test]
    fn tee_capture_records_empty_successful_reads() {
        let (mut tee, mut consumer, dropped) = tee_from(READ_END_BYTES, 0);
        let mut buf = [0; 64];
        assert_eq!(tee.read(&mut buf, Duration::ZERO).unwrap(), 0);
        let mut recorded = Vec::new();
        drain(&mut consumer, &mut recorded);
        let chunks: Vec<_> = reads(&dump_header(1), &recorded).collect();
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].bytes.is_empty());
        assert_eq!(chunks[0].end_qpc, Some(456));
        assert_eq!(dropped.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn tee_capture_preserves_reads_and_records_dropped_bytes() {
        let one_read = ssr_abi::record_size(8) + READ_END_BYTES;
        let (mut tee, mut consumer, dropped) = tee(one_read * 2);
        let mut buf = [0; 64];
        tee.read(&mut buf, Duration::ZERO).unwrap();
        tee.read(&mut buf, Duration::ZERO).unwrap();
        tee.read(&mut buf, Duration::ZERO).unwrap();
        assert_eq!(
            dropped.load(Ordering::Relaxed),
            ssr_abi::record_size(8) as u64
        );

        let mut recorded = Vec::new();
        drain(&mut consumer, &mut recorded);
        tee.read(&mut buf, Duration::ZERO).unwrap();
        drain(&mut consumer, &mut recorded);

        let chunks: Vec<_> = reads(&dump_header(1), &recorded).collect();
        assert_eq!(chunks.len(), 3);
        for (chunk, payload) in chunks.iter().zip([1, 2, 4]) {
            assert!(matches!(
                ssr_abi::records(chunk.bytes).next(),
                Some(Ok(ssr_abi::Record::Data { bytes, .. })) if bytes == [payload; 8]
            ));
        }
        assert_eq!(chunks[0].end_qpc, Some(456));
        assert_eq!(chunks[1].end_qpc, Some(456));
        assert_eq!(chunks[2].end_qpc, Some(456));
        assert_eq!(chunks[2].lost_bytes, ssr_abi::record_size(8) as u64);
    }

    #[test]
    fn tee_capture_stop_idle_restart_replays_like_the_live_chain() {
        let format = SourceFormat::Pcm(PcmFormat {
            channels: 2,
            sample_rate: 48_000,
            bits_per_sample: 32,
            valid_bits: 32,
            sample_type: SampleType::Float,
            channel_mask: 3,
        });
        let record = |write: &dyn Fn(&mut RecordWriter<'_>) -> bool| {
            let mut bytes = vec![0; 4_096];
            let mut writer = RecordWriter::new(&mut bytes);
            assert!(write(&mut writer));
            let len = writer.len();
            bytes.truncate(len);
            bytes
        };
        let first = 0.125_f32.to_le_bytes().repeat(480 * 2);
        let second = 0.25_f32.to_le_bytes().repeat(480 * 2);
        let scripted = vec![
            record(&|writer| writer.format_change(1, 0, &wave_format_bytes(&format))),
            record(&|writer| writer.data(1, 0, 10_000, &first)),
            record(&|writer| writer.stream_stop(1, 20_000)),
            Vec::new(),
            Vec::new(),
            record(&|writer| writer.stream_start(2, 50_000)),
            record(&|writer| writer.data(2, 0, 60_000, &second)),
        ];
        let capacity =
            scripted.iter().map(Vec::len).sum::<usize>() + scripted.len() * READ_END_BYTES;
        let (producer, mut consumer) = rtrb::RingBuffer::new(capacity);
        let dropped = Arc::new(AtomicU64::new(0));
        let mut tee = TeeCapture {
            inner: ScriptedCapture {
                reads: scripted,
                next: 0,
                qpc: 0,
            },
            tap: Some(producer),
            dropped: Arc::clone(&dropped),
            pending_gap: 0,
            now: fixed_now,
            stats_before_reconnect: None,
            stats_after_reconnect: None,
        };
        let mut buf = vec![0; 4_096];
        let mut live_reads = Vec::new();
        for _ in 0..7 {
            let n = tee.read(&mut buf, Duration::ZERO).unwrap();
            live_reads.push((buf[..n].to_vec(), tee.now_qpc().unwrap()));
        }
        let mut marked = Vec::new();
        drain(&mut consumer, &mut marked);
        assert_eq!(dropped.load(Ordering::Relaxed), 0);

        let prepared = || {
            Box::new(
                Prepared::speakers(
                    Arc::new(PreparedSpeakers {
                        layout: Layout::new(vec![
                            Direction::new("FL", 30.0, 0.0),
                            Direction::new("FR", -30.0, 0.0),
                        ])
                        .unwrap(),
                        rate: 48_000,
                        partition: 480,
                        device_channels: 2,
                        channel_mask: 3,
                        speakers: vec![
                            PreparedSpeaker {
                                name: "FL".into(),
                                device_channel: 0,
                                small: false,
                                crossover: None,
                                delay_frames: 0,
                                gain: 1.0,
                                fir: None,
                            },
                            PreparedSpeaker {
                                name: "FR".into(),
                                device_channel: 1,
                                small: false,
                                crossover: None,
                                delay_frames: 0,
                                gain: 1.0,
                                fir: None,
                            },
                        ],
                        subwoofers: Vec::new(),
                        bass_to: Vec::new(),
                        lfe_gain: 10.0_f32.sqrt(),
                        gain: 1.0,
                        limiter_ceiling: 1.0,
                    }),
                    480,
                )
                .unwrap(),
            )
        };
        let render = |input: Vec<(Vec<u8>, u64)>| {
            let mut chain = Chain::with_qpc_frequency(
                ChainConfig {
                    arrival_slack_frames: 0,
                    ..ChainConfig::default()
                },
                1_000_000,
            );
            chain.set_prepared(prepared());
            let mut output = Vec::new();
            let mut collect = |event: ChainEvent<'_>| {
                if let ChainEvent::Block(block) = event {
                    output.extend_from_slice(block.samples);
                }
            };
            for (bytes, end_qpc) in input {
                chain.push_records(&bytes, &mut collect);
                chain.idle(end_qpc, &mut collect);
            }
            chain.finish(&mut collect);
            output
        };
        let live = render(live_reads);
        let header = dump_header(1_000_000);
        let replay = render(
            reads(&header, &marked)
                .map(|chunk| (chunk.bytes.to_vec(), chunk.end_qpc.unwrap()))
                .collect(),
        );
        let first_audible = |samples: &[f32]| {
            samples
                .iter()
                .position(|sample| sample.abs() > 1.0e-6)
                .expect("render contains audible output")
        };
        let live_start = first_audible(&live);
        let replay_start = first_audible(&replay);
        assert_eq!(live[live_start..], replay[replay_start..]);
    }
}

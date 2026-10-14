//! Port adapters that need no Windows.
//!
//! * [`DumpCapture`]: a [`CaptureSource`] that replays a dump at its recorded
//!   pace on an injected clock, optionally looping it.
//! * [`WavOpener`] / [`WavSink`]: an output that writes a 32-bit float stereo
//!   WAV, reporting a synthetic playback QPC.
//! * [`NullOpener`] / [`NullSink`]: an output that discards.

use std::fs::File;
use std::io::BufWriter;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use ssr_types::{
    CaptureError, CaptureSource, OutputError, OutputFormat, OutputLayout, OutputOpener, OutputSink,
    TrimUnsupported,
};

const QPC_FREQUENCY: u64 = 10_000_000;

/// Where one READ chunk ends in the cleaned stream. `recorded` is false for a
/// chunk the recorder did not close with a READ end: the unterminated tail
/// (all of a dump written by a tool rather than recorded) or bytes cut short by
/// a recorder gap. Only recorded ends are READ boundaries.
#[derive(Clone, Copy, Debug)]
struct ReadEnd {
    at: usize,
    recorded: bool,
}

/// The READ-aligned records `--loop` repeats, and how far one pass moves the
/// dump's clock and byte position.
#[derive(Clone, Copy, Debug)]
struct LoopSpan {
    start: usize,
    end: usize,
    first_read_end: Option<usize>,
    qpc: u64,
    bytes: u64,
}

/// Finds the repeat span. Format 2 supplies READ ends so the span expands from
/// the first data record's READ start through the last one's READ end, when the
/// recorder closed that READ. Otherwise (legacy dumps, and dumps with no
/// recorded READ around the last data record) the span is the data records
/// alone, so state records before them play once.
fn loop_span(stream: &[u8], read_ends: Option<&[ReadEnd]>) -> Result<LoopSpan, String> {
    let data: Vec<_> = ssr_abi::raw_records(stream)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?
        .into_iter()
        .filter(|raw| raw.header.kind == ssr_abi::RECORD_KIND_DATA)
        .collect();
    let (first, last) = match (data.first(), data.last()) {
        (Some(first), Some(last)) if data.len() >= 2 => (*first, *last),
        _ => return Err("--loop needs a dump with at least two data records".to_owned()),
    };
    let spacing = data[1].header.qpc.saturating_sub(first.header.qpc);
    let recorded_span = read_ends.and_then(|ends| {
        let first_read = ends.partition_point(|end| end.at <= first.offset);
        let last_read = ends.partition_point(|end| end.at <= last.offset);
        let end = ends.get(last_read).filter(|end| end.recorded)?;
        let start = first_read
            .checked_sub(1)
            .map_or(0, |before| ends[before].at);
        Some((start, end.at, Some(first_read)))
    });
    let (start, end, first_read_end) =
        recorded_span.unwrap_or((first.offset, last.offset + last.size, None));
    Ok(LoopSpan {
        start,
        end,
        first_read_end,
        qpc: last.header.qpc.saturating_sub(first.header.qpc) + spacing,
        bytes: (last.header.byte_pos + u64::from(last.header.len))
            .saturating_sub(first.header.byte_pos),
    })
}

/// Replays a dump at the pace its QPC stamps give, restamping every record with
/// the QPC at which it was handed out, as a driver read would. One read hands
/// out at most one recorded READ, and only whole records. The clock is
/// injected: ssr-live passes the system QPC, tests a counter.
pub struct DumpCapture {
    stream: Vec<u8>,
    offset: usize,
    read_ends: Vec<usize>,
    next_read_end: usize,
    origin: Option<(u64, u64)>,
    dump_frequency: u64,
    frequency: u64,
    now: Arc<dyn Fn() -> u64 + Send + Sync>,
    ended: Option<Arc<AtomicBool>>,
    repeat: Option<LoopSpan>,
    qpc_shift: u64,
    pos_shift: u64,
}

impl std::fmt::Debug for DumpCapture {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DumpCapture")
            .field("offset", &self.offset)
            .finish_non_exhaustive()
    }
}

impl DumpCapture {
    /// `bytes` is a whole dump file; `frequency` and `now` are the local
    /// clock the replay is paced on and restamped with. `ended` is set when
    /// the last record has been handed out. With `repeat`, the data records
    /// (whole recorded READs where the dump has them) play again and again,
    /// each pass later in time and byte position.
    pub fn from_dump(
        bytes: &[u8],
        frequency: u64,
        now: impl Fn() -> u64 + Send + Sync + 'static,
        ended: Option<Arc<AtomicBool>>,
        repeat: bool,
    ) -> Result<Self, String> {
        let (header, stream) = ssr_abi::split_dump(bytes).map_err(|error| error.to_string())?;
        // Checked once here, so `read` can slice without checks failing. Only
        // framing is checked: a record whose contents are malformed replays
        // as the driver handed it out, for the engine to report.
        for raw in ssr_abi::raw_records(stream) {
            raw.map_err(|error| error.to_string())?;
        }
        if header.qpc_frequency == 0 || frequency == 0 {
            return Err("the dump or local QPC frequency is zero".to_owned());
        }
        let mut clean = Vec::with_capacity(stream.len());
        let mut read_ends = Vec::new();
        let mut bounds = Vec::new();
        for chunk in ssr_abi::reads(&header, stream) {
            clean.extend_from_slice(chunk.bytes);
            read_ends.push(clean.len());
            bounds.push(ReadEnd {
                at: clean.len(),
                recorded: chunk.end_qpc.is_some(),
            });
        }
        let repeat = repeat
            .then(|| {
                loop_span(
                    &clean,
                    (header.format_version >= 2).then_some(bounds.as_slice()),
                )
            })
            .transpose()?;
        Ok(Self {
            stream: clean,
            offset: 0,
            read_ends,
            next_read_end: 0,
            origin: None,
            dump_frequency: header.qpc_frequency,
            frequency,
            now: Arc::new(now),
            ended,
            repeat,
            qpc_shift: 0,
            pos_shift: 0,
        })
    }

    fn wrap(&mut self) {
        let Some(span) = self.repeat.filter(|span| self.offset == span.end) else {
            return;
        };
        self.offset = span.start;
        if let Some(first) = span.first_read_end {
            self.next_read_end = first;
        }
        self.qpc_shift = self.qpc_shift.saturating_add(span.qpc);
        self.pos_shift = self.pos_shift.saturating_add(span.bytes);
    }
}

impl CaptureSource for DumpCapture {
    fn read(&mut self, buf: &mut [u8], wait: Duration) -> Result<usize, CaptureError> {
        let deadline =
            (self.now)().saturating_add((wait.as_secs_f64() * self.frequency as f64) as u64);
        loop {
            self.wrap();
            if self.offset == self.stream.len() {
                if let Some(ended) = &self.ended {
                    ended.store(true, Ordering::Release);
                }
                return Err(CaptureError::Ended);
            }
            let now = (self.now)();
            let mut written = 0;
            while self.offset < self.stream.len() {
                self.wrap();
                let raw = ssr_abi::raw_records(&self.stream[self.offset..])
                    .next()
                    .expect("validated stream")
                    .expect("validated stream");
                let dump_qpc = raw.header.qpc.saturating_add(self.qpc_shift);
                let (dump0, local0) = *self.origin.get_or_insert((dump_qpc, now));
                let due = local0.saturating_add(
                    (u128::from(dump_qpc.saturating_sub(dump0)) * u128::from(self.frequency)
                        / u128::from(self.dump_frequency)) as u64,
                );
                if written == 0 && raw.size > buf.len() {
                    return Err(CaptureError::Io(format!(
                        "a {}-byte record does not fit the {}-byte capture buffer",
                        raw.size,
                        buf.len()
                    )));
                }
                if due > now || written + raw.size > buf.len() {
                    break;
                }
                let source = self.offset;
                let out = &mut buf[written..written + raw.size];
                out.copy_from_slice(&self.stream[source..source + raw.size]);
                let header = ssr_abi::header_mut(out).expect("validated header");
                header.qpc = now;
                header.qpc_written = now;
                if matches!(
                    header.kind,
                    ssr_abi::RECORD_KIND_DATA | ssr_abi::RECORD_KIND_OVERRUN
                ) {
                    header.byte_pos = header.byte_pos.saturating_add(self.pos_shift);
                }
                written += raw.size;
                self.offset += raw.size;
                if self
                    .read_ends
                    .get(self.next_read_end)
                    .is_some_and(|end| self.offset == *end)
                {
                    self.next_read_end += 1;
                    break;
                }
            }
            if written > 0 || now >= deadline {
                return Ok(written);
            }
            thread::sleep(Duration::from_millis(1));
        }
    }

    fn now_qpc(&self) -> Option<u64> {
        Some((self.now)())
    }

    fn set_rate_trim(&mut self, _ppm: f64) -> Result<(), TrimUnsupported> {
        Err(TrimUnsupported)
    }
}

/// Opens a [`WavSink`](WavOpener::Sink) writing `path` at `rate` with
/// `period_frames` per period.
#[derive(Debug, Clone)]
pub struct WavOpener {
    pub path: PathBuf,
    pub rate: u32,
    pub period_frames: usize,
}

impl OutputOpener for WavOpener {
    type Sink = WavSink;

    fn open(&mut self, layout: OutputLayout) -> Result<Self::Sink, OutputError> {
        let writer = hound::WavWriter::create(
            &self.path,
            hound::WavSpec {
                channels: layout.channels,
                sample_rate: self.rate,
                bits_per_sample: 32,
                sample_format: hound::SampleFormat::Float,
            },
        )
        .map_err(|error| OutputError::Io(format!("{}: {error}", self.path.display())))?;
        Ok(WavSink {
            writer: Some(writer),
            format: OutputFormat {
                rate: self.rate,
                channels: layout.channels,
                channel_mask: layout.channel_mask,
                period_frames: self.period_frames,
                mode: ssr_types::OutputMode::Exclusive,
                fell_back: false,
                mix_channels: layout.channels,
            },
            interleaved: vec![
                0.0;
                self.period_frames
                    .saturating_mul(usize::from(layout.channels))
            ],
            frames_written: 0,
        })
    }
}

/// A 32-bit float WAV output sink with the opened layout's channel count.
pub struct WavSink {
    writer: Option<hound::WavWriter<BufWriter<File>>>,
    format: OutputFormat,
    interleaved: Vec<f32>,
    frames_written: u64,
}

impl std::fmt::Debug for WavSink {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WavSink")
            .field("format", &self.format)
            .field("frames_written", &self.frames_written)
            .finish_non_exhaustive()
    }
}

impl WavSink {
    /// Finalizes the RIFF header and reports any write error.
    pub fn finish(mut self) -> Result<(), OutputError> {
        self.finalize()
    }

    fn finalize(&mut self) -> Result<(), OutputError> {
        if let Some(writer) = self.writer.take() {
            writer
                .finalize()
                .map_err(|error| OutputError::Io(error.to_string()))?;
        }
        Ok(())
    }
}

impl OutputSink for WavSink {
    fn format(&self) -> OutputFormat {
        self.format
    }

    fn next_period(&mut self, fill: &mut dyn FnMut(&mut [f32])) -> Result<u64, OutputError> {
        let qpc = frames_to_qpc(self.frames_written, self.format.rate);
        fill(&mut self.interleaved);
        let writer = self
            .writer
            .as_mut()
            .ok_or_else(|| OutputError::Io("the WAV sink is already finalized".into()))?;
        for &sample in &self.interleaved {
            writer
                .write_sample(sample)
                .map_err(|error| OutputError::Io(error.to_string()))?;
        }
        self.frames_written = self
            .frames_written
            .saturating_add(self.format.period_frames as u64);
        Ok(qpc)
    }
}

impl Drop for WavSink {
    fn drop(&mut self) {
        let _ = self.finalize();
    }
}

/// Opens a sink that discards everything, at `rate` with `period_frames`.
#[derive(Debug, Clone, Copy)]
pub struct NullOpener {
    pub rate: u32,
    pub period_frames: usize,
}

impl OutputOpener for NullOpener {
    type Sink = NullSink;

    fn open(&mut self, layout: OutputLayout) -> Result<Self::Sink, OutputError> {
        Ok(NullSink {
            format: OutputFormat {
                rate: self.rate,
                channels: layout.channels,
                channel_mask: layout.channel_mask,
                period_frames: self.period_frames,
                mode: ssr_types::OutputMode::Exclusive,
                fell_back: false,
                mix_channels: layout.channels,
            },
            interleaved: vec![
                0.0;
                self.period_frames
                    .saturating_mul(usize::from(layout.channels))
            ],
            frames_written: 0,
        })
    }
}

/// An output sink that invokes the producer and discards its samples.
#[derive(Debug)]
pub struct NullSink {
    format: OutputFormat,
    interleaved: Vec<f32>,
    frames_written: u64,
}

impl OutputSink for NullSink {
    fn format(&self) -> OutputFormat {
        self.format
    }

    fn next_period(&mut self, fill: &mut dyn FnMut(&mut [f32])) -> Result<u64, OutputError> {
        let qpc = frames_to_qpc(self.frames_written, self.format.rate);
        fill(&mut self.interleaved);
        self.frames_written = self
            .frames_written
            .saturating_add(self.format.period_frames as u64);
        Ok(qpc)
    }
}

fn frames_to_qpc(frames: u64, rate: u32) -> u64 {
    if rate == 0 {
        0
    } else {
        ((u128::from(frames) * u128::from(QPC_FREQUENCY)) / u128::from(rate)) as u64
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicU64;
    use std::time::{Duration, SystemTime};

    use ssr_abi::RecordWriter;

    use super::*;

    fn dump(records: &[u8], frequency: u64) -> Vec<u8> {
        let mut dump = ssr_abi::dump_header_bytes(frequency);
        dump.extend_from_slice(records);
        dump
    }

    fn clock(value: u64) -> (Arc<AtomicU64>, impl Fn() -> u64 + Send + Sync + 'static) {
        let clock = Arc::new(AtomicU64::new(value));
        let read = Arc::clone(&clock);
        (clock, move || read.load(Ordering::Acquire))
    }

    fn first_raw(bytes: &[u8]) -> ssr_abi::RawRecord<'_> {
        ssr_abi::raw_records(bytes).next().unwrap().unwrap()
    }

    #[test]
    fn dump_capture_paces_and_restamps_records_from_the_first_record() {
        let mut stream = vec![0; 512];
        let mut writer = RecordWriter::new(&mut stream);
        assert!(writer.stream_start(1, 100));
        assert!(writer.data(1, 0, 110, &[1; 8]));
        let len = writer.len();
        let dump = dump(&stream[..len], 1_000);
        let (now, read_now) = clock(5_000);
        let mut capture = DumpCapture::from_dump(&dump, 1_000, read_now, None, false).unwrap();
        let mut buffer = [0; 128];

        let first = ssr_abi::record_size(0);
        assert_eq!(capture.read(&mut buffer, Duration::ZERO), Ok(first));
        let raw = first_raw(&buffer[..first]);
        assert_eq!((raw.header.qpc, raw.header.qpc_written), (5_000, 5_000));
        assert_eq!(capture.read(&mut buffer, Duration::ZERO), Ok(0));

        now.store(5_010, Ordering::Release);
        let second = ssr_abi::record_size(8);
        assert_eq!(capture.read(&mut buffer, Duration::ZERO), Ok(second));
        let raw = first_raw(&buffer[..second]);
        assert_eq!((raw.header.qpc, raw.header.qpc_written), (5_010, 5_010));
        assert_eq!(raw.payload, &[1; 8]);
    }

    #[test]
    fn dump_capture_format_two_returns_one_recorded_read_at_a_time() {
        let mut stream = vec![0; 512];
        let mut writer = RecordWriter::new(&mut stream);
        assert!(writer.stream_start(1, 0));
        let first = writer.len();
        assert!(writer.read_end(1));
        assert!(writer.data(1, 0, 0, &[3; 8]));
        let second = ssr_abi::record_size(8);
        assert!(writer.read_end(2));
        let len = writer.len();
        let dump = dump(&stream[..len], 1_000);
        let (_now, read_now) = clock(10);
        let mut capture = DumpCapture::from_dump(&dump, 1_000, read_now, None, false).unwrap();
        let mut buffer = [0; 256];

        assert_eq!(capture.read(&mut buffer, Duration::ZERO), Ok(first));
        assert_eq!(capture.read(&mut buffer, Duration::ZERO), Ok(second));
        assert_eq!(
            capture.read(&mut buffer, Duration::ZERO),
            Err(CaptureError::Ended)
        );
    }

    #[test]
    fn dump_capture_keeps_record_boundaries() {
        let mut stream = vec![0; 256];
        let mut writer = RecordWriter::new(&mut stream);
        assert!(writer.data(1, 0, 0, &[1; 32]));
        let len = writer.len();
        let dump = dump(&stream[..len], 1_000);
        let (_now, read_now) = clock(0);
        let mut capture = DumpCapture::from_dump(&dump, 1_000, read_now, None, false).unwrap();
        let mut short = [0; 40];
        assert!(matches!(
            capture.read(&mut short, Duration::ZERO),
            Err(CaptureError::Io(_))
        ));
        let mut exact = [0; 72];
        assert_eq!(capture.read(&mut exact, Duration::ZERO), Ok(len));
    }

    #[test]
    fn dump_capture_reports_end_after_the_last_record() {
        let mut stream = vec![0; 128];
        let mut writer = RecordWriter::new(&mut stream);
        assert!(writer.stream_start(1, 0));
        let len = writer.len();
        let dump = dump(&stream[..len], 1_000);
        let ended = Arc::new(AtomicBool::new(false));
        let (_now, read_now) = clock(7);
        let mut capture =
            DumpCapture::from_dump(&dump, 1_000, read_now, Some(Arc::clone(&ended)), false)
                .unwrap();
        let mut buffer = [0; 64];

        assert_eq!(capture.read(&mut buffer, Duration::ZERO), Ok(len));
        assert!(!ended.load(Ordering::Acquire));
        assert_eq!(
            capture.read(&mut buffer, Duration::ZERO),
            Err(CaptureError::Ended)
        );
        assert!(ended.load(Ordering::Acquire));
        assert_eq!(capture.now_qpc(), Some(7));
    }

    #[test]
    fn dump_capture_refuses_a_dump_cut_inside_a_record() {
        let mut stream = vec![0; 128];
        let mut writer = RecordWriter::new(&mut stream);
        assert!(writer.stream_start(1, 0));
        assert!(writer.read_end(1));
        let len = writer.len();
        let mut dump = dump(&stream[..len], 1_000);
        // Three bytes of a next record header.
        dump.extend_from_slice(&[1, 2, 3]);
        assert!(DumpCapture::from_dump(&dump, 1_000, || 0, None, false).is_err());
    }

    #[test]
    fn format_two_loop_span_uses_whole_recorded_reads() {
        let mut stream = vec![0; 512];
        let mut writer = RecordWriter::new(&mut stream);
        assert!(writer.stream_start(1, 1));
        assert!(writer.read_end(2));
        assert!(writer.format_change(1, 3, &[]));
        assert!(writer.data(1, 0, 10, &[1]));
        assert!(writer.read_end(11));
        assert!(writer.data(1, 1, 20, &[2]));
        assert!(writer.stream_stop(1, 21));
        assert!(writer.read_end(22));
        assert!(writer.stream_start(2, 30));
        assert!(writer.read_end(31));
        let len = writer.len();
        let dump = dump(&stream[..len], 1_000);
        let (_now, read_now) = clock(0);
        let capture = DumpCapture::from_dump(&dump, 1_000, read_now, None, true).unwrap();
        let span = capture.repeat.unwrap();
        assert_eq!(span.first_read_end, Some(1));
        assert_eq!(span.start, capture.read_ends[0]);
        assert_eq!(span.end, capture.read_ends[2]);
    }

    /// A format-2 dump written by a tool (the CI fixtures) has no READ ends:
    /// its one unterminated chunk is not a READ, so the loop repeats the data
    /// records and never the format or stream records before them.
    #[test]
    fn unrecorded_reads_loop_the_data_records() {
        let mut stream = [0; 512];
        let mut writer = RecordWriter::new(&mut stream);
        assert!(writer.format_change(1, 0, &[]));
        assert!(writer.stream_start(1, 0));
        let first_data = writer.len();
        assert!(writer.data(1, 0, 10, &[1; 16]));
        assert!(writer.data(1, 16, 20, &[2; 16]));
        let after_data = writer.len();
        assert!(writer.stream_stop(1, 25));
        let len = writer.len();

        let tail = [ReadEnd {
            at: len,
            recorded: false,
        }];
        let span = loop_span(&stream[..len], Some(&tail)).unwrap();
        assert_eq!((span.start, span.end), (first_data, after_data));
        assert_eq!(span.first_read_end, None);
        assert_eq!(span.bytes, 32);

        // A recorded READ before the data does not make the unterminated tail
        // that holds the last data record a READ either.
        let ends = [
            ReadEnd {
                at: first_data,
                recorded: true,
            },
            ReadEnd {
                at: len,
                recorded: false,
            },
        ];
        let span = loop_span(&stream[..len], Some(&ends)).unwrap();
        assert_eq!((span.start, span.end), (first_data, after_data));
        assert_eq!(span.first_read_end, None);
    }

    #[test]
    fn dump_capture_rejects_zero_frequencies() {
        let valid_frequency = dump(&[], 1_000);
        assert!(DumpCapture::from_dump(&valid_frequency, 0, || 0, None, false).is_err());
        let zero_frequency = dump(&[], 0);
        assert!(DumpCapture::from_dump(&zero_frequency, 1_000, || 0, None, false).is_err());
    }

    #[test]
    fn dump_capture_loops_with_continuous_positions_and_times() {
        let mut stream = vec![0; 512];
        let mut writer = RecordWriter::new(&mut stream);
        assert!(writer.stream_start(1, 0));
        assert!(writer.data(1, 0, 10, &[1; 16]));
        assert!(writer.data(1, 16, 20, &[2; 16]));
        assert!(writer.stream_stop(1, 25));
        let len = writer.len();
        let dump = dump(&stream[..len], 1_000);
        let (now, read_now) = clock(1_000);
        let mut capture = DumpCapture::from_dump(&dump, 1_000, read_now, None, true).unwrap();
        let mut buffer = [0; 56];

        assert_eq!(capture.read(&mut buffer, Duration::ZERO), Ok(40));
        for (local, payload, position) in [
            (1_010, 1, 0),
            (1_020, 2, 16),
            (1_030, 1, 32),
            (1_040, 2, 48),
            (1_050, 1, 64),
        ] {
            now.store(local, Ordering::Release);
            assert_eq!(capture.read(&mut buffer, Duration::ZERO), Ok(56));
            let raw = first_raw(&buffer);
            assert_eq!(raw.payload[0], payload);
            assert_eq!(raw.header.byte_pos, position);
            assert_eq!((raw.header.qpc, raw.header.qpc_written), (local, local));
        }
    }

    #[test]
    fn dump_capture_loop_needs_two_data_records() {
        let mut stream = vec![0; 128];
        let mut writer = RecordWriter::new(&mut stream);
        assert!(writer.data(1, 0, 0, &[1; 8]));
        let len = writer.len();
        let dump = dump(&stream[..len], 1_000);
        assert!(DumpCapture::from_dump(&dump, 1_000, || 0, None, true).is_err());
    }

    #[test]
    fn wav_sink_writes_readable_float_stereo() {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("clock follows the Unix epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("engine-wav-sink-{unique}.wav"));
        let mut sink = WavOpener {
            path: path.clone(),
            rate: 48_000,
            period_frames: 4,
        }
        .open(OutputLayout::STEREO)
        .unwrap();
        let mut fill = |samples: &mut [f32]| {
            for (index, sample) in samples.iter_mut().enumerate() {
                *sample = index as f32 / 8.0;
            }
        };
        assert_eq!(sink.next_period(&mut fill), Ok(0));
        assert_eq!(sink.next_period(&mut fill), Ok(833));
        sink.finish().unwrap();

        let reader = hound::WavReader::open(&path).unwrap();
        assert_eq!(reader.spec().channels, 2);
        assert_eq!(reader.spec().sample_rate, 48_000);
        assert_eq!(reader.spec().sample_format, hound::SampleFormat::Float);
        assert_eq!(reader.duration(), 8);
        drop(reader);
        std::fs::remove_file(path).unwrap();
    }
}

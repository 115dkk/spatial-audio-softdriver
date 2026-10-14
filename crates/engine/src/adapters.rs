//! Port adapters that need no Windows.
//!
//! * [`DumpCapture`]: a [`CaptureSource`] over a dump's record stream.
//! * [`WavOpener`] / [`WavSink`]: an output that writes a 32-bit float stereo
//!   WAV, reporting a synthetic playback QPC.
//! * [`NullOpener`] / [`NullSink`]: an output that discards.

use std::fs::File;
use std::io::BufWriter;
use std::path::PathBuf;
use std::time::Duration;

use ssr_types::{
    CaptureError, CaptureSource, OutputError, OutputFormat, OutputLayout, OutputOpener, OutputSink,
    TrimUnsupported,
};

const RECORD_HEADER_BYTES: usize = size_of::<ssr_abi::RecordHeader>();
const RECORD_LENGTH_OFFSET: usize = std::mem::offset_of!(ssr_abi::RecordHeader, len);
const RECORD_QPC_OFFSET: usize = std::mem::offset_of!(ssr_abi::RecordHeader, qpc);
const QPC_FREQUENCY: u64 = 10_000_000;

/// Replays a dump's record stream, handing out whole records only.
#[derive(Debug)]
pub struct DumpCapture {
    stream: Vec<u8>,
    offset: usize,
    format_version: u32,
    last_read_qpc: Option<u64>,
}

impl DumpCapture {
    /// Constructs a legacy capture without recorded READ boundaries.
    pub fn new(stream: Vec<u8>) -> Self {
        Self::with_format(stream, 1)
    }

    /// `stream` and `format_version` come from [`ssr_abi::split_dump`].
    pub fn with_format(stream: Vec<u8>, format_version: u32) -> Self {
        Self {
            stream,
            offset: 0,
            format_version,
            last_read_qpc: None,
        }
    }
}

impl CaptureSource for DumpCapture {
    /// Copies as many whole records as fit in `buf`; `Err(Ended)` once the
    /// stream is exhausted. A record larger than `buf` is an error.
    fn read(&mut self, buf: &mut [u8], _wait: Duration) -> Result<usize, CaptureError> {
        if self.offset == self.stream.len() {
            return Err(CaptureError::Ended);
        }

        self.last_read_qpc = None;
        let mut source = self.offset;
        let mut written = 0;
        while source < self.stream.len() {
            let rest = &self.stream[source..];
            if rest.len() < RECORD_HEADER_BYTES {
                return Err(CaptureError::Io(format!(
                    "record at offset {source} has a truncated header"
                )));
            }
            let kind = u16::from_le_bytes(rest[..2].try_into().expect("two bytes were sliced"));
            let payload_len = u32::from_le_bytes(
                rest[RECORD_LENGTH_OFFSET..RECORD_LENGTH_OFFSET + 4]
                    .try_into()
                    .expect("four bytes were sliced"),
            ) as usize;
            let size = ssr_abi::record_size(payload_len);
            if rest.len() < size {
                return Err(CaptureError::Io(format!(
                    "record at offset {source} needs {size} bytes, but only {} remain",
                    rest.len()
                )));
            }
            if self.format_version >= 2
                && (kind == ssr_abi::RECORD_KIND_READ_END
                    || kind == ssr_abi::RECORD_KIND_RECORDER_GAP)
            {
                source += size;
                if kind == ssr_abi::RECORD_KIND_READ_END {
                    self.last_read_qpc = Some(u64::from_le_bytes(
                        rest[RECORD_QPC_OFFSET..RECORD_QPC_OFFSET + 8]
                            .try_into()
                            .expect("eight bytes were sliced"),
                    ));
                    break;
                }
                continue;
            }
            if size > buf.len() && written == 0 {
                return Err(CaptureError::Io(format!(
                    "a {size}-byte record does not fit the {}-byte capture buffer",
                    buf.len()
                )));
            }
            if written + size > buf.len() {
                break;
            }
            buf[written..written + size].copy_from_slice(&rest[..size]);
            written += size;
            source += size;
        }
        self.offset = source;
        Ok(written)
    }

    fn now_qpc(&self) -> Option<u64> {
        (self.format_version >= 2)
            .then_some(self.last_read_qpc)
            .flatten()
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
    use std::time::{Duration, SystemTime};

    use ssr_abi::RecordWriter;

    use super::*;

    #[test]
    fn dump_capture_keeps_record_boundaries() {
        let mut stream = vec![0; 512];
        let mut writer = RecordWriter::new(&mut stream);
        assert!(writer.data(1, 0, 0, &[1; 8]));
        assert!(writer.data(1, 8, 1, &[2; 32]));
        let len = writer.len();
        stream.truncate(len);
        let first_size = ssr_abi::record_size(8);
        let mut capture = DumpCapture::new(stream);
        let mut buffer = vec![0; first_size];
        assert_eq!(capture.read(&mut buffer, Duration::ZERO), Ok(first_size));
        assert!(matches!(
            capture.read(&mut buffer, Duration::ZERO),
            Err(CaptureError::Io(_))
        ));
    }

    #[test]
    fn dump_capture_format_two_returns_one_recorded_read_at_a_time() {
        let mut stream = vec![0; 512];
        let mut writer = RecordWriter::new(&mut stream);
        assert!(writer.stream_start(1, 0));
        let first = writer.len();
        assert!(writer.read_end(1));
        assert!(writer.data(1, 0, 2, &[3; 8]));
        let second = ssr_abi::record_size(8);
        assert!(writer.read_end(3));
        let len = writer.len();
        stream.truncate(len);
        let mut capture = DumpCapture::with_format(stream, 2);
        let mut buffer = [0; 256];
        assert_eq!(capture.read(&mut buffer, Duration::ZERO), Ok(first));
        assert_eq!(capture.now_qpc(), Some(1));
        assert_eq!(capture.read(&mut buffer, Duration::ZERO), Ok(second));
        assert_eq!(capture.now_qpc(), Some(3));
        assert_eq!(
            capture.read(&mut buffer, Duration::ZERO),
            Err(CaptureError::Ended)
        );
    }

    #[test]
    fn dump_capture_clears_qpc_when_the_next_read_fails() {
        let mut stream = vec![0; 128];
        let mut writer = RecordWriter::new(&mut stream);
        assert!(writer.stream_start(1, 0));
        let first = writer.len();
        assert!(writer.read_end(7));
        let mut len = writer.len();
        stream[len..len + 3].copy_from_slice(&[1, 2, 3]);
        len += 3;
        stream.truncate(len);

        let mut capture = DumpCapture::with_format(stream, 2);
        let mut buffer = [0; 64];
        assert_eq!(capture.read(&mut buffer, Duration::ZERO), Ok(first));
        assert_eq!(capture.now_qpc(), Some(7));
        assert!(matches!(
            capture.read(&mut buffer, Duration::ZERO),
            Err(CaptureError::Io(_))
        ));
        assert_eq!(capture.now_qpc(), None);
    }

    #[test]
    fn dump_capture_reports_end_after_the_last_record() {
        let mut stream = vec![0; 128];
        let mut writer = RecordWriter::new(&mut stream);
        assert!(writer.stream_start(1, 0));
        let len = writer.len();
        stream.truncate(len);
        let mut capture = DumpCapture::new(stream);
        let mut buffer = [0; 64];
        assert_eq!(capture.read(&mut buffer, Duration::ZERO), Ok(len));
        assert_eq!(capture.now_qpc(), None);
        assert_eq!(
            capture.read(&mut buffer, Duration::ZERO),
            Err(CaptureError::Ended)
        );
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

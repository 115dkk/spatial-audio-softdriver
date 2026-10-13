#![forbid(unsafe_code)]

//! bio-live: development tool for the SpatialAudioLab test PC (ADR 0018).
//!
//! * `record` writes the SAL ring's record stream to a dump file, the input
//!   the MAT decoder and the black-box goldens are built from (ADR 0005).
//! * `play` runs SAL ring → engine → WASAPI with a BRIR profile, printing
//!   latency, crossing-ring level, underruns and clock correction every
//!   second; `--record` keeps a dump of the same stream. `--from-dump` plays
//!   a dump instead of the ring (`--loop` repeats it) and `--report` writes
//!   the run's figures as JSON, which is how CI soaks the live path.
//! * `devices` lists render endpoints.
//!
//! The SAL device opens only from an administrator prompt.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand, ValueEnum};

#[derive(Parser)]
#[command(
    version,
    about = "Spatial SoftReceiver live tool for the SAL development PC"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum Clock {
    Auto,
    Asrc,
    Trim,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum CaptureKind {
    Driver,
    Sal,
}

#[cfg_attr(not(any(windows, test)), allow(dead_code))]
fn parse_endpoints(value: Option<&str>) -> Result<engine::EndpointFilter, String> {
    let Some(value) = value else {
        return Ok(engine::EndpointFilter::All);
    };
    let mut mask = 0_u32;
    let mut count = 0;
    for part in value.split(',') {
        let part = part.trim();
        let endpoint = part
            .parse::<u16>()
            .map_err(|_| format!("invalid endpoint number {part:?}"))?;
        if endpoint as usize >= bio_abi::MAX_ENDPOINTS {
            return Err(format!(
                "endpoint {endpoint} is outside 0..{}",
                bio_abi::MAX_ENDPOINTS - 1
            ));
        }
        let bit = 1_u32 << endpoint;
        if mask & bit != 0 {
            return Err(format!("endpoint {endpoint} is listed more than once"));
        }
        mask |= bit;
        count += 1;
    }
    if count == 0 {
        return Err("--endpoints requires at least one endpoint number".to_owned());
    }
    Ok(engine::EndpointFilter::Mask(mask))
}

#[cfg(test)]
mod endpoint_tests {
    use super::*;

    #[test]
    fn endpoint_parser_accepts_arbitrary_subsets_and_rejects_bad_masks() {
        let subset = parse_endpoints(Some("0,2")).unwrap();
        assert!(subset.includes(0));
        assert!(!subset.includes(1));
        assert!(subset.includes(2));
        assert!(!subset.includes(3));
        assert!(parse_endpoints(Some("4")).is_err());
        assert!(parse_endpoints(Some("2,2")).is_err());
        assert!(parse_endpoints(Some("")).is_err());
    }

    #[test]
    fn play_direct_layouts_are_repeatable_and_record_rejects_them() {
        let cli = Cli::try_parse_from([
            "bio-live",
            "play",
            "--profile",
            "profile.toml",
            "--direct-layout",
            "2=L,R",
            "--direct-layout",
            "12=7.1.4",
        ])
        .unwrap();
        let Command::Play { direct_layouts, .. } = cli.command else {
            panic!("expected play command");
        };
        assert_eq!(direct_layouts.len(), 2);
        assert_eq!(direct_layouts[0].channels, 2);
        assert_eq!(direct_layouts[1].channels, 12);

        assert!(
            Cli::try_parse_from([
                "bio-live",
                "record",
                "--out",
                "capture.biodump",
                "--direct-layout",
                "2=stereo",
            ])
            .is_err()
        );
        assert!(
            Cli::try_parse_from([
                "bio-live",
                "play",
                "--profile",
                "profile.toml",
                "--direct-layout",
                "2=L,L",
            ])
            .is_err()
        );
    }
}

#[derive(Subcommand)]
#[allow(clippy::large_enum_variant)]
enum Command {
    /// Record a capture ring (the Spatial SoftReceiver driver or SAL) to a dump file (run as administrator).
    Record {
        /// Capture source; the Spatial SoftReceiver driver is the normal source.
        #[arg(long, value_enum, default_value_t = CaptureKind::Driver)]
        source: CaptureKind,
        /// Output file (`.biodump`).
        #[arg(long)]
        out: PathBuf,
        /// Seconds to record.
        #[arg(long, default_value_t = 60.0)]
        seconds: f64,
    },
    /// Play a capture ring through a BRIR profile (run as administrator).
    Play {
        /// Capture source; ignored when --from-dump supplies the source.
        #[arg(long, value_enum, default_value_t = CaptureKind::Driver)]
        source: CaptureKind,
        /// Endpoints to consume: all exposed endpoints, or a comma-separated subset.
        #[arg(long)]
        endpoints: Option<String>,
        /// Profile TOML (see crates/brir).
        #[arg(long)]
        profile: PathBuf,
        /// Part of the output device's name; the default device when omitted.
        #[arg(long)]
        output: Option<String>,
        /// Use WASAPI shared mode instead of exclusive.
        #[arg(long)]
        shared: bool,
        /// Permit an exclusive request to fall back to WASAPI shared mode.
        #[arg(long, conflicts_with = "shared")]
        allow_shared_fallback: bool,
        /// Device sample rate.
        #[arg(long, default_value_t = 48_000)]
        rate: u32,
        /// Device period in milliseconds; the device's minimum when omitted.
        #[arg(long)]
        period_ms: Option<f64>,
        /// Crossing-ring level to hold, in milliseconds (10 for games, 40 for music).
        #[arg(long, default_value_t = 10.0)]
        target_ms: f32,
        #[arg(long, value_enum, default_value_t = Clock::Auto)]
        clock: Clock,
        /// Also write the captured record stream to this dump file.
        #[arg(long)]
        record: Option<PathBuf>,
        /// Stop after this many seconds (otherwise press Enter to stop).
        #[arg(long)]
        seconds: Option<f64>,
        /// Play a dump file at its recorded pace instead of the SAL ring, to run
        /// the live path on a PC without SAL. Stops when the dump ends.
        #[arg(long)]
        from_dump: Option<PathBuf>,
        /// With --from-dump: repeat the dump's data records, with continuous
        /// positions and times, until --seconds or Enter stops the run.
        #[arg(long = "loop", requires = "from_dump")]
        repeat: bool,
        /// Run everything but send silence to the device.
        #[arg(long)]
        mute: bool,
        /// Write the run's figures (underruns, restarts, faults, latency,
        /// ring trough, clock) as JSON to this file when it stops.
        #[arg(long)]
        report: Option<PathBuf>,
        /// Override a zero-mask PCM order, as CHANNELS=PRESET or CHANNELS=SPEAKER,SPEAKER,...
        #[arg(long = "direct-layout")]
        direct_layouts: Vec<engine::DirectLayoutOverride>,
    },
    /// List render endpoints (name, default flag, id).
    Devices {
        /// Include shared mix format and standard-layout exclusive support.
        #[arg(long)]
        formats: bool,
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Record {
            source,
            out,
            seconds,
        } => platform::record(source, &out, seconds),
        Command::Play {
            source,
            endpoints,
            profile,
            output,
            shared,
            allow_shared_fallback,
            rate,
            period_ms,
            target_ms,
            clock,
            record,
            seconds,
            from_dump,
            repeat,
            mute,
            report,
            direct_layouts,
        } => platform::play(platform::PlayOptions {
            source,
            endpoints,
            profile,
            output,
            shared,
            allow_shared_fallback,
            rate,
            period_ms,
            target_ms,
            clock,
            record,
            seconds,
            from_dump,
            repeat,
            mute,
            report,
            direct_layouts,
        }),
        Command::Devices { formats, json } => platform::devices(formats, json),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("bio-live: {message}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(any(windows, test))]
mod paced_bounds {
    use std::mem::{offset_of, size_of};

    use bio_abi::RecordHeader;

    #[cfg_attr(not(windows), allow(dead_code))]
    const HEADER_BYTES: usize = size_of::<RecordHeader>();
    #[cfg_attr(not(windows), allow(dead_code))]
    const KIND_AT: usize = offset_of!(RecordHeader, kind);
    #[cfg_attr(not(windows), allow(dead_code))]
    const POS_AT: usize = offset_of!(RecordHeader, byte_pos);
    #[cfg_attr(not(windows), allow(dead_code))]
    const LEN_AT: usize = offset_of!(RecordHeader, len);
    #[cfg_attr(not(windows), allow(dead_code))]
    const QPC_AT: usize = offset_of!(RecordHeader, qpc);

    /// Where one READ chunk ends in the cleaned stream. `recorded` is false for
    /// a chunk the recorder did not close with a READ end: the unterminated
    /// tail (all of a dump written by a tool rather than recorded) or bytes
    /// cut short by a recorder gap. Only recorded ends are READ boundaries.
    #[derive(Clone, Copy, Debug)]
    #[cfg_attr(not(windows), allow(dead_code))]
    pub(super) struct ReadEnd {
        pub(super) at: usize,
        pub(super) recorded: bool,
    }

    /// The READ-aligned records `--loop` repeats, and how far one pass moves
    /// the dump's clock and byte position.
    #[derive(Clone, Copy, Debug)]
    #[cfg_attr(not(windows), allow(dead_code))]
    pub(super) struct LoopSpan {
        pub(super) start: usize,
        pub(super) end: usize,
        pub(super) first_read_end: Option<usize>,
        pub(super) qpc: u64,
        pub(super) bytes: u64,
    }

    /// Size (padding included, clamped to the stream) and kind, byte position,
    /// payload length and dump QPC of the record at `at`.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub(super) fn record_at(stream: &[u8], at: usize) -> (usize, u16, u64, u32, u64) {
        let header = &stream[at..at + HEADER_BYTES];
        let kind = u16::from_le_bytes(header[KIND_AT..KIND_AT + 2].try_into().expect("2 bytes"));
        let pos = u64::from_le_bytes(header[POS_AT..POS_AT + 8].try_into().expect("8 bytes"));
        let len = u32::from_le_bytes(header[LEN_AT..LEN_AT + 4].try_into().expect("4 bytes"));
        let qpc = u64::from_le_bytes(header[QPC_AT..QPC_AT + 8].try_into().expect("8 bytes"));
        let size = bio_abi::record_size(len as usize).min(stream.len() - at);
        (size, kind, pos, len, qpc)
    }

    #[cfg_attr(not(windows), allow(dead_code))]
    pub(super) fn wrap_loop(
        span: LoopSpan,
        offset: &mut usize,
        next_read_end: &mut usize,
        qpc_shift: &mut u64,
        pos_shift: &mut u64,
    ) -> bool {
        if *offset != span.end {
            return false;
        }
        *offset = span.start;
        if let Some(first_read_end) = span.first_read_end {
            *next_read_end = first_read_end;
        }
        *qpc_shift += span.qpc;
        *pos_shift += span.bytes;
        true
    }

    /// Finds the repeat span. Format 2 supplies READ ends so the span expands
    /// from the first data record's READ start through the last one's READ end,
    /// when the recorder closed that READ. Otherwise (legacy dumps, and dumps
    /// with no recorded READ around the last data record) the span is the data
    /// records alone, so state records before them play once.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub(super) fn loop_span(
        stream: &[u8],
        read_ends: Option<&[ReadEnd]>,
    ) -> Result<LoopSpan, String> {
        let mut data = Vec::new();
        let mut at = 0;
        while at < stream.len() {
            let (size, kind, pos, len, qpc) = record_at(stream, at);
            if kind == bio_abi::RECORD_KIND_DATA {
                data.push((at, size, pos, len, qpc));
            }
            at += size;
        }
        let (&first, &last) = match (data.first(), data.last()) {
            (Some(first), Some(last)) if data.len() >= 2 => (first, last),
            _ => return Err("--loop needs a dump with at least two data records".to_owned()),
        };
        let spacing = data[1].4.saturating_sub(first.4);
        let recorded_span = read_ends.and_then(|ends| {
            let first_read = ends.partition_point(|end| end.at <= first.0);
            let last_read = ends.partition_point(|end| end.at <= last.0);
            let end = ends.get(last_read).filter(|end| end.recorded)?;
            let start = first_read
                .checked_sub(1)
                .map_or(0, |before| ends[before].at);
            Some((start, end.at, Some(first_read)))
        });
        let (start, end, first_read_end) =
            recorded_span.unwrap_or((first.0, last.0 + last.1, None));
        Ok(LoopSpan {
            start,
            end,
            first_read_end,
            qpc: last.4.saturating_sub(first.4) + spacing,
            bytes: (last.2 + u64::from(last.3)).saturating_sub(first.2),
        })
    }

    #[cfg(test)]
    mod tests {
        use bio_abi::RecordWriter;

        use super::*;

        #[test]
        fn format_two_loop_span_uses_whole_recorded_reads() {
            let mut stream = [0; 512];
            let mut writer = RecordWriter::new(&mut stream);
            assert!(writer.stream_start(1, 1));
            let read_zero = writer.len();
            assert!(writer.format_change(1, 2, &[]));
            assert!(writer.data(1, 0, 10, &[1]));
            let read_one = writer.len();
            assert!(writer.data(1, 1, 20, &[2]));
            assert!(writer.stream_stop(1, 21));
            let read_two = writer.len();
            assert!(writer.stream_start(2, 30));
            let read_three = writer.len();
            let len = writer.len();

            let ends = [read_zero, read_one, read_two, read_three]
                .map(|at| ReadEnd { at, recorded: true });
            let span = loop_span(&stream[..len], Some(&ends)).unwrap();
            assert_eq!(span.start, read_zero);
            assert_eq!(span.end, read_two);
            assert_eq!(span.first_read_end, Some(1));

            let mut offset = span.end;
            let mut next_read_end = 3;
            let mut qpc_shift = 0;
            let mut pos_shift = 0;
            assert!(wrap_loop(
                span,
                &mut offset,
                &mut next_read_end,
                &mut qpc_shift,
                &mut pos_shift,
            ));
            assert_eq!(offset, read_zero);
            assert_eq!(next_read_end, 1);
            assert_eq!(qpc_shift, span.qpc);
            assert_eq!(pos_shift, span.bytes);
        }

        /// A format-2 dump written by a tool (the CI fixtures) has no READ
        /// ends: its one unterminated chunk is not a READ, so the loop repeats
        /// the data records and never the format or stream records before
        /// them.
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

            // A recorded READ before the data does not make the unterminated
            // tail that holds the last data record a READ either.
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
    }
}

#[cfg(any(windows, test))]
mod capture_tap {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    use bio_abi::RecordWriter;
    use bio_types::{CaptureError, CaptureSource, TrimUnsupported};

    #[cfg_attr(not(windows), allow(dead_code))]
    pub(super) const READ_END_BYTES: usize = 40;

    #[cfg_attr(not(windows), allow(dead_code))]
    fn marker(write: impl FnOnce(&mut RecordWriter<'_>) -> bool) -> [u8; 48] {
        let mut bytes = [0; 48];
        let mut writer = RecordWriter::new(&mut bytes);
        assert!(write(&mut writer));
        bytes
    }

    #[cfg_attr(not(windows), allow(dead_code))]
    pub(super) fn read_end_marker(qpc: u64) -> [u8; 48] {
        marker(|writer| writer.read_end(qpc))
    }

    #[cfg_attr(not(windows), allow(dead_code))]
    fn recorder_gap_marker(qpc: u64, lost_bytes: u64) -> [u8; 48] {
        marker(|writer| writer.recorder_gap(qpc, lost_bytes))
    }

    /// Copies every chunk the capture thread reads into a byte ring that a
    /// writer thread drains to the dump file, so the real-time thread never
    /// touches the file. A chunk that does not fit is dropped whole (records
    /// stay intact) and counted.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub(super) struct TeeCapture<C> {
        pub(super) inner: C,
        pub(super) tap: Option<rtrb::Producer<u8>>,
        pub(super) dropped: Arc<AtomicU64>,
        pub(super) pending_gap: u64,
        pub(super) now: fn() -> u64,
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
            self.inner.reconnect()
        }
    }

    #[cfg(test)]
    mod tests {
        use assert_no_alloc::{AllocDisabler, assert_no_alloc};
        use bio_abi::{dump_header, reads, wave_format_bytes};
        use bio_types::{Direction, Layout, PcmFormat, SampleType, SourceFormat};
        use engine::{Chain, ChainConfig, ChainEvent, Prepared};
        use speakers::{PreparedSpeaker, PreparedSpeakers};

        use super::*;

        #[global_allocator]
        static ALLOCATOR: AllocDisabler = AllocDisabler;

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
            let one_read = bio_abi::record_size(8) + READ_END_BYTES;
            let (mut tee, mut consumer, dropped) = tee(one_read * 2);
            let mut buf = [0; 64];
            tee.read(&mut buf, Duration::ZERO).unwrap();
            tee.read(&mut buf, Duration::ZERO).unwrap();
            tee.read(&mut buf, Duration::ZERO).unwrap();
            assert_eq!(
                dropped.load(Ordering::Relaxed),
                bio_abi::record_size(8) as u64
            );

            let mut recorded = Vec::new();
            drain(&mut consumer, &mut recorded);
            tee.read(&mut buf, Duration::ZERO).unwrap();
            drain(&mut consumer, &mut recorded);

            let chunks: Vec<_> = reads(&dump_header(1), &recorded).collect();
            assert_eq!(chunks.len(), 3);
            for (chunk, payload) in chunks.iter().zip([1, 2, 4]) {
                assert!(matches!(
                    bio_abi::records(chunk.bytes).next(),
                    Some(Ok(bio_abi::Record::Data { bytes, .. })) if bytes == [payload; 8]
                ));
            }
            assert_eq!(chunks[0].end_qpc, Some(456));
            assert_eq!(chunks[1].end_qpc, Some(456));
            assert_eq!(chunks[2].end_qpc, Some(456));
            assert_eq!(chunks[2].lost_bytes, bio_abi::record_size(8) as u64);
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

        #[test]
        fn tee_capture_read_does_not_allocate() {
            let (mut tee, _consumer, _dropped) = tee(256);
            let mut buf = [0; 64];
            assert_no_alloc(|| {});
            assert_no_alloc(|| {
                tee.read(&mut buf, Duration::ZERO).unwrap();
            });
        }
    }
}

#[cfg(windows)]
mod platform {
    use std::cell::RefCell;
    use std::collections::BTreeMap;
    use std::fs::File;
    use std::io::{BufRead, BufWriter, Write};
    use std::mem::offset_of;
    #[cfg(test)]
    use std::mem::size_of;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::thread;
    use std::time::{Duration, Instant};

    use bio_abi::{Record, RecordHeader, dump_header_bytes, records};
    use bio_sys::bio_ring::{BioRing, DriverStatsSample};
    use bio_sys::mmcss::{self, MmcssGuard};
    use bio_sys::output::WasapiOpener;
    use bio_sys::{devices, qpc, sal::SalRing};
    use bio_types::{CaptureError, CaptureSource, TrimUnsupported};
    use engine::{
        ClockPreference, EngineConfig, EngineEvent, LiveChange, OutputProfile, Prepared,
        RealtimeThread, StartOptions, partition_for, scene_shape,
    };

    use super::capture_tap::{READ_END_BYTES, TeeCapture, read_end_marker};
    use super::paced_bounds::{LoopSpan, ReadEnd, loop_span, record_at, wrap_loop};
    use super::{CaptureKind, Clock, parse_endpoints};

    pub struct PlayOptions {
        pub source: CaptureKind,
        pub endpoints: Option<String>,
        pub profile: PathBuf,
        pub output: Option<String>,
        pub shared: bool,
        pub allow_shared_fallback: bool,
        pub rate: u32,
        pub period_ms: Option<f64>,
        pub target_ms: f32,
        pub clock: Clock,
        pub record: Option<PathBuf>,
        pub seconds: Option<f64>,
        pub from_dump: Option<PathBuf>,
        pub repeat: bool,
        pub mute: bool,
        pub report: Option<PathBuf>,
        pub direct_layouts: Vec<engine::DirectLayoutOverride>,
    }

    fn open_sal() -> Result<SalRing, String> {
        SalRing::open().map_err(|e| capture_message("SAL", e))
    }

    fn open_driver() -> Result<BioRing, String> {
        BioRing::open().map_err(|e| capture_message("Spatial SoftReceiver", e))
    }

    fn capture_message(name: &str, error: CaptureError) -> String {
        match error {
            CaptureError::NotPresent => format!("the {name} capture device is not present"),
            CaptureError::AccessDenied => {
                format!(
                    "access to the {name} capture device was denied; run from an administrator prompt"
                )
            }
            other => other.to_string(),
        }
    }

    fn create_dump(path: &Path, qpc_frequency: u64) -> Result<BufWriter<File>, String> {
        let file = File::create(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut writer = BufWriter::with_capacity(1 << 20, file);
        writer
            .write_all(&dump_header_bytes(qpc_frequency))
            .map_err(|e| e.to_string())?;
        Ok(writer)
    }

    pub fn record(source: CaptureKind, out: &Path, seconds: f64) -> Result<(), String> {
        let (mut ring, qpc_frequency, source_name) = match source {
            CaptureKind::Driver => {
                let ring = open_driver()?;
                let frequency = ring.info().qpc_frequency;
                let initial = ring.stats_sample().map_err(|e| e.to_string())?;
                (
                    Source::Driver {
                        ring,
                        stats: Arc::new(std::sync::Mutex::new(DriverStatsTotals::new(initial))),
                    },
                    frequency,
                    "Spatial SoftReceiver",
                )
            }
            CaptureKind::Sal => (Source::Sal(open_sal()?), qpc::frequency(), "SAL"),
        };
        let mut writer = create_dump(out, qpc_frequency)?;
        let mut buf = vec![0_u8; 1 << 20];
        let mut bytes = 0_u64;
        let mut endpoints = BTreeMap::<u16, (u64, u64, u64)>::new();
        let started = Instant::now();
        let mut next_report = Duration::from_secs(1);
        eprintln!(
            "recording the {source_name} ring to {} for {seconds} s; start the source now",
            out.display()
        );
        while started.elapsed().as_secs_f64() < seconds {
            let n = ring
                .read(&mut buf, Duration::from_millis(100))
                .map_err(|e| e.to_string())?;
            let read_qpc = ring.now_qpc().unwrap_or_else(qpc::now);
            writer.write_all(&buf[..n]).map_err(|e| e.to_string())?;
            writer
                .write_all(&read_end_marker(read_qpc)[..READ_END_BYTES])
                .map_err(|e| e.to_string())?;
            if n > 0 {
                bytes += n as u64;
                for record in records(&buf[..n]).flatten() {
                    let endpoint = record.endpoint();
                    let counts = endpoints.entry(endpoint).or_default();
                    match record {
                        Record::Data { bytes: payload, .. } => counts.0 += payload.len() as u64,
                        Record::FormatChange { epoch, format, .. } => {
                            counts.1 += 1;
                            eprintln!("endpoint {endpoint} epoch {epoch}: {format:?}");
                        }
                        Record::Overrun { lost_bytes, .. } => counts.2 += lost_bytes,
                        _ => {}
                    }
                }
            }
            if started.elapsed() >= next_report {
                for (endpoint, (data, formats, lost)) in &endpoints {
                    eprintln!(
                        "{:>4} s  endpoint {endpoint}: {data} payload bytes, {formats} format changes, {lost} bytes lost",
                        next_report.as_secs()
                    );
                }
                next_report += Duration::from_secs(1);
            }
        }
        writer.flush().map_err(|e| e.to_string())?;
        let data = endpoints.values().map(|counts| counts.0).sum::<u64>();
        eprintln!(
            "wrote {bytes} bytes of records ({data} payload bytes) to {}",
            out.display()
        );
        if data == 0 {
            return Err(
                "no data arrived: play a game with Dolby Atmos for home theater, or Apple Music, \
                 to the SAL endpoint while recording"
                    .to_owned(),
            );
        }
        Ok(())
    }

    #[derive(Clone, Copy)]
    struct DriverStatsTotals {
        generation: u64,
        first: bio_abi::BioStats,
        last: bio_abi::BioStats,
        streamed_bytes: [u64; bio_abi::MAX_ENDPOINTS],
        overruns: [u64; bio_abi::MAX_ENDPOINTS],
        lost_bytes: [u64; bio_abi::MAX_ENDPOINTS],
        max_timer_lateness_us: u32,
    }

    impl DriverStatsTotals {
        fn new(sample: DriverStatsSample) -> Self {
            let stats = sample.stats;
            Self {
                generation: sample.generation,
                first: stats,
                last: stats,
                streamed_bytes: [0; bio_abi::MAX_ENDPOINTS],
                overruns: [0; bio_abi::MAX_ENDPOINTS],
                lost_bytes: [0; bio_abi::MAX_ENDPOINTS],
                max_timer_lateness_us: stats.max_timer_lateness_us,
            }
        }

        fn observe(&mut self, sample: DriverStatsSample) {
            if sample.generation != self.generation {
                return;
            }
            let stats = sample.stats;
            for endpoint in 0..bio_abi::MAX_ENDPOINTS {
                let before = self.last.endpoints[endpoint];
                let after = stats.endpoints[endpoint];
                self.streamed_bytes[endpoint] = self.streamed_bytes[endpoint]
                    .saturating_add(after.total_bytes.saturating_sub(before.total_bytes));
                self.overruns[endpoint] = self.overruns[endpoint]
                    .saturating_add(after.overruns.saturating_sub(before.overruns));
                self.lost_bytes[endpoint] = self.lost_bytes[endpoint]
                    .saturating_add(after.lost_bytes.saturating_sub(before.lost_bytes));
            }
            self.max_timer_lateness_us =
                self.max_timer_lateness_us.max(stats.max_timer_lateness_us);
            self.last = stats;
        }

        fn begin_generation(&mut self, sample: DriverStatsSample) {
            self.generation = sample.generation;
            self.last = sample.stats;
            self.max_timer_lateness_us = self
                .max_timer_lateness_us
                .max(sample.stats.max_timer_lateness_us);
        }
    }

    enum Source {
        Driver {
            ring: BioRing,
            stats: Arc<std::sync::Mutex<DriverStatsTotals>>,
        },
        Sal(SalRing),
        Dump(PacedDump),
    }

    impl CaptureSource for Source {
        fn read(&mut self, buf: &mut [u8], wait: Duration) -> Result<usize, CaptureError> {
            match self {
                Self::Driver { ring, .. } => ring.read(buf, wait),
                Self::Sal(ring) => ring.read(buf, wait),
                Self::Dump(dump) => dump.read(buf, wait),
            }
        }

        fn now_qpc(&self) -> Option<u64> {
            match self {
                Self::Driver { ring, .. } => ring.now_qpc(),
                Self::Sal(ring) => ring.now_qpc(),
                Self::Dump(dump) => dump.now_qpc(),
            }
        }

        fn set_rate_trim(&mut self, ppm: f64) -> Result<(), TrimUnsupported> {
            match self {
                Self::Driver { ring, .. } => ring.set_rate_trim(ppm),
                Self::Sal(ring) => ring.set_rate_trim(ppm),
                Self::Dump(dump) => dump.set_rate_trim(ppm),
            }
        }

        fn reconnect(&mut self) -> Result<(), CaptureError> {
            match self {
                Self::Driver { ring, stats } => {
                    // The failed read may mean the old control handle can no longer
                    // answer GET_STATS. Preserve a final snapshot when possible, but
                    // never let statistics prevent the reconnect itself.
                    if let Ok(before) = ring.stats_sample() {
                        stats
                            .lock()
                            .map_err(|_| {
                                CaptureError::Io("driver statistics lock was poisoned".into())
                            })?
                            .observe(before);
                    }
                    ring.reconnect()?;
                    let after = ring.stats_sample()?;
                    stats
                        .lock()
                        .map_err(|_| {
                            CaptureError::Io("driver statistics lock was poisoned".into())
                        })?
                        .begin_generation(after);
                    Ok(())
                }
                Self::Sal(ring) => ring.reconnect(),
                Self::Dump(dump) => dump.reconnect(),
            }
        }
    }

    #[cfg(test)]
    const HEADER_BYTES: usize = size_of::<RecordHeader>();
    const POS_AT: usize = offset_of!(RecordHeader, byte_pos);
    const QPC_AT: usize = offset_of!(RecordHeader, qpc);
    const QPC_WRITTEN_AT: usize = offset_of!(RecordHeader, qpc_written);

    /// Replays a dump at the pace its QPC stamps give, restamping every record
    /// with the QPC at which it was handed out, as a driver read would. The
    /// whole live path (real-time threads, WASAPI, clock correction, latency)
    /// then runs on a PC without SAL; the source clock is the QPC.
    struct PacedDump {
        stream: Vec<u8>,
        offset: usize,
        read_ends: Vec<usize>,
        next_read_end: usize,
        /// The first record's dump QPC and the local QPC it was released at.
        origin: Option<(u64, u64)>,
        dump_frequency: u64,
        frequency: u64,
        ended: Arc<AtomicBool>,
        /// With `--loop`, the data records to repeat.
        repeat: Option<LoopSpan>,
        /// Added to the dump QPC and byte position of every record replayed
        /// after the first pass.
        qpc_shift: u64,
        pos_shift: u64,
    }

    impl PacedDump {
        fn new(bytes: &[u8], ended: Arc<AtomicBool>, repeat: bool) -> Result<Self, String> {
            let (header, stream) = bio_abi::split_dump(bytes).map_err(|e| e.to_string())?;
            // Checked once here, so `read` can slice without checks failing.
            for record in records(stream) {
                record.map_err(|e| e.to_string())?;
            }
            if header.qpc_frequency == 0 {
                return Err("the dump header has a zero QPC frequency".to_owned());
            }
            let mut clean = Vec::with_capacity(stream.len());
            let mut read_ends = Vec::new();
            let mut bounds = Vec::new();
            for chunk in bio_abi::reads(&header, stream) {
                clean.extend_from_slice(chunk.bytes);
                read_ends.push(clean.len());
                bounds.push(ReadEnd {
                    at: clean.len(),
                    recorded: chunk.end_qpc.is_some(),
                });
            }
            let repeat = if repeat {
                Some(loop_span(
                    &clean,
                    (header.format_version >= 2).then_some(bounds.as_slice()),
                )?)
            } else {
                None
            };
            Ok(Self {
                stream: clean,
                offset: 0,
                read_ends,
                next_read_end: 0,
                origin: None,
                dump_frequency: header.qpc_frequency,
                frequency: qpc::frequency(),
                ended,
                repeat,
                qpc_shift: 0,
                pos_shift: 0,
            })
        }
    }

    impl PacedDump {
        /// With `--loop`, goes back to the first data record once the last
        /// one has been handed out, one pass later in time and position.
        fn wrap(&mut self) {
            if let Some(span) = self.repeat {
                wrap_loop(
                    span,
                    &mut self.offset,
                    &mut self.next_read_end,
                    &mut self.qpc_shift,
                    &mut self.pos_shift,
                );
            }
        }
    }

    impl CaptureSource for PacedDump {
        fn read(&mut self, buf: &mut [u8], wait: Duration) -> Result<usize, CaptureError> {
            let deadline = qpc::now() + (wait.as_secs_f64() * self.frequency as f64) as u64;
            loop {
                self.wrap();
                if self.offset == self.stream.len() {
                    self.ended.store(true, Ordering::Release);
                    return Err(CaptureError::Ended);
                }
                let now = qpc::now();
                let mut written = 0;
                while self.offset < self.stream.len() {
                    self.wrap();
                    let (size, kind, pos, _, dump_qpc) = record_at(&self.stream, self.offset);
                    let dump_qpc = dump_qpc + self.qpc_shift;
                    let (dump0, local0) = *self.origin.get_or_insert((dump_qpc, now));
                    let due = local0
                        + (u128::from(dump_qpc.saturating_sub(dump0)) * u128::from(self.frequency)
                            / u128::from(self.dump_frequency)) as u64;
                    if due > now || written + size > buf.len() {
                        break;
                    }
                    let out = &mut buf[written..written + size];
                    out.copy_from_slice(&self.stream[self.offset..self.offset + size]);
                    out[QPC_AT..QPC_AT + 8].copy_from_slice(&now.to_le_bytes());
                    out[QPC_WRITTEN_AT..QPC_WRITTEN_AT + 8].copy_from_slice(&now.to_le_bytes());
                    if kind == bio_abi::RECORD_KIND_DATA || kind == bio_abi::RECORD_KIND_OVERRUN {
                        out[POS_AT..POS_AT + 8]
                            .copy_from_slice(&(pos + self.pos_shift).to_le_bytes());
                    }
                    written += size;
                    self.offset += size;
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
            Some(qpc::now())
        }

        fn set_rate_trim(&mut self, _ppm: f64) -> Result<(), TrimUnsupported> {
            Err(TrimUnsupported)
        }
    }

    thread_local! {
        // Keeps each real-time thread registered with MMCSS until it exits.
        static MMCSS: RefCell<Option<MmcssGuard>> = const { RefCell::new(None) };
    }

    fn promote_thread(thread: RealtimeThread) {
        match mmcss::promote("Pro Audio") {
            Ok(guard) => MMCSS.with(|slot| *slot.borrow_mut() = Some(guard)),
            Err(e) => eprintln!("MMCSS registration failed for {thread:?}: {e}"),
        }
    }

    fn prepare_output(profile: &OutputProfile, shape: (u32, usize)) -> Result<Prepared, String> {
        match profile {
            OutputProfile::Headphones(profile) => {
                let prepared = brir::prepare(profile, shape.0, partition_for(shape.1))
                    .map_err(|e| e.to_string())?;
                Prepared::new(Arc::new(prepared), shape.1).map_err(|e| e.to_string())
            }
            OutputProfile::Speakers(profile) => {
                let prepared = speakers::prepare(profile, shape.0, partition_for(shape.1))
                    .map_err(|e| e.to_string())?;
                Prepared::speakers(Arc::new(prepared), shape.1).map_err(|e| e.to_string())
            }
        }
    }

    pub fn play(options: PlayOptions) -> Result<(), String> {
        let profile = engine::load_output_profile(&options.profile).map_err(|e| e.to_string())?;
        let endpoint = devices::find(options.output.as_deref()).map_err(|e| e.to_string())?;
        eprintln!("output: {}", endpoint.name);
        let ended = Arc::new(AtomicBool::new(false));
        let (source, source_qpc, driver_stats) = match &options.from_dump {
            Some(path) => {
                let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
                (
                    Source::Dump(PacedDump::new(&bytes, ended.clone(), options.repeat)?),
                    qpc::frequency(),
                    None,
                )
            }
            None => match options.source {
                CaptureKind::Driver => {
                    let ring = open_driver()?;
                    let frequency = ring.info().qpc_frequency;
                    let start = ring.stats_sample().map_err(|e| e.to_string())?;
                    let observer = ring.observer();
                    let stats = Arc::new(std::sync::Mutex::new(DriverStatsTotals::new(start)));
                    (
                        Source::Driver {
                            ring,
                            stats: Arc::clone(&stats),
                        },
                        frequency,
                        Some((observer, stats)),
                    )
                }
                CaptureKind::Sal => (Source::Sal(open_sal()?), qpc::frequency(), None),
            },
        };

        let dropped = Arc::new(AtomicU64::new(0));
        let stop_writer = Arc::new(AtomicBool::new(false));
        let (tap, writer_thread) = match &options.record {
            Some(path) => {
                let mut writer = create_dump(path, source_qpc)?;
                // About 2.7 s of MAT at 3 MB/s.
                let (producer, mut consumer) = rtrb::RingBuffer::<u8>::new(8 << 20);
                let stop = stop_writer.clone();
                let handle = thread::spawn(move || -> Result<(), String> {
                    loop {
                        let available = consumer.slots();
                        if available > 0 {
                            let chunk = consumer
                                .read_chunk(available)
                                .map_err(|e| format!("{e:?}"))?;
                            let (a, b) = chunk.as_slices();
                            writer.write_all(a).map_err(|e| e.to_string())?;
                            writer.write_all(b).map_err(|e| e.to_string())?;
                            chunk.commit_all();
                        } else if stop.load(Ordering::Acquire) {
                            break;
                        } else {
                            thread::sleep(Duration::from_millis(5));
                        }
                    }
                    writer.flush().map_err(|e| e.to_string())
                });
                (Some(producer), Some(handle))
            }
            None => (None, None),
        };
        let capture = TeeCapture {
            inner: source,
            tap,
            dropped: dropped.clone(),
            pending_gap: 0,
            now: qpc::now,
        };

        let endpoints = parse_endpoints(options.endpoints.as_deref())?;
        let mut direct = engine::DirectLayouts::default();
        for value in &options.direct_layouts {
            direct
                .set(value.channels, value.layout)
                .map_err(str::to_owned)?;
        }
        let config = EngineConfig {
            chain: engine::ChainConfig {
                direct,
                endpoints,
                ..engine::ChainConfig::default()
            },
            target_ms: options.target_ms,
            clock: match options.clock {
                Clock::Auto => ClockPreference::Auto,
                Clock::Asrc => ClockPreference::Asrc,
                Clock::Trim => ClockPreference::Trim,
            },
            thread_hook: Some(Arc::new(promote_thread)),
            ..EngineConfig::default()
        };
        let output = WasapiOpener {
            endpoint: Some(endpoint.id.clone()),
            rate: options.rate,
            exclusive: !options.shared,
            shared_fallback: options.allow_shared_fallback,
            period: options
                .period_ms
                .filter(|ms| ms.is_finite() && *ms > 0.0)
                .map(|ms| Duration::from_secs_f64(ms / 1000.0)),
        };
        let initial_shape = (engine::MIX_RATE, engine::MIX_FRAMES);
        let initial_prepared = Box::new(prepare_output(&profile, initial_shape)?);
        let output_kind = initial_prepared.kind().as_str();
        let output_channels = initial_prepared.channels();
        let output_channel_mask = initial_prepared.channel_mask();
        let mut engine = engine::start_with(
            config,
            capture,
            output,
            StartOptions {
                qpc_frequency: source_qpc,
                prepared: Some(initial_prepared),
            },
        )
        .map_err(|e| e.to_string())?;
        if options.mute {
            engine
                .apply(LiveChange::Mute(true))
                .map_err(|e| e.to_string())?;
        }

        let stop = Arc::new(AtomicBool::new(false));
        if options.seconds.is_none() {
            let stop = stop.clone();
            thread::spawn(move || {
                let mut line = String::new();
                // End of input (no console) is not a request to stop.
                if std::io::stdin()
                    .lock()
                    .read_line(&mut line)
                    .is_ok_and(|n| n > 0)
                {
                    stop.store(true, Ordering::Release);
                }
            });
            eprintln!("playing; press Enter to stop");
        }

        let mut wanted: Option<(u32, usize)> = None;
        let mut applied = Some(initial_shape);
        let started = Instant::now();
        let mut next_report = Duration::from_secs(1);
        let mut ended_at: Option<Instant> = None;
        let mut figures = RunFigures::default();
        while !stop.load(Ordering::Acquire)
            && options
                .seconds
                .is_none_or(|s| started.elapsed().as_secs_f64() < s)
            // A replayed dump stops once the crossing ring has drained.
            && ended_at.is_none_or(|t| t.elapsed() < Duration::from_millis(500))
        {
            if ended_at.is_none() && ended.load(Ordering::Acquire) {
                eprintln!("the dump has ended");
                ended_at = Some(Instant::now());
                // What follows is the ring draining, not the source failing.
                figures.underruns_while_streaming = Some(engine.status().underruns);
            }
            for event in engine.poll() {
                match event {
                    EngineEvent::FormatChanged {
                        endpoint,
                        epoch,
                        format,
                    } => {
                        eprintln!("endpoint {endpoint} epoch {epoch}: {format:?}");
                        if let Some(shape) = scene_shape(&format) {
                            wanted = Some(shape);
                        }
                    }
                    EngineEvent::NeedsBrir { rate, block } => wanted = Some((rate, block)),
                    EngineEvent::Overrun { lost_bytes } => {
                        eprintln!("overrun: {lost_bytes} bytes lost");
                        figures.overruns += 1;
                    }
                    EngineEvent::OutputOverflow { frames } => {
                        eprintln!("output overflow: {frames} rendered frames lost");
                    }
                    EngineEvent::OutputLayoutChanged {
                        channels,
                        channel_mask,
                    } => {
                        eprintln!("output reopened: {channels} channels, mask {channel_mask:#x}");
                    }
                    EngineEvent::OutputReconnected => eprintln!("output reconnected"),
                    EngineEvent::FaultCleared => eprintln!("fault cleared"),
                    EngineEvent::OutputCadenceFault { ratio } => {
                        eprintln!("output cadence fault: {ratio:.3}");
                        figures.faults.push("OutputCadence".to_owned());
                    }
                    EngineEvent::OutputCadenceReopen { period_ms } => {
                        eprintln!("output cadence reopen: {period_ms:.1} ms");
                    }
                    EngineEvent::Fault(fault) => {
                        eprintln!("fault: {fault:?}");
                        figures.faults.push(format!("{fault:?}"));
                    }
                }
            }
            // A format change and the engine's NeedsBrir name the same shape;
            // one Prepared per shape is enough. A rejected change stays wanted
            // and is retried on the next tick.
            if let Some((rate, block)) = wanted.filter(|shape| Some(*shape) != applied) {
                let t = Instant::now();
                let next = Box::new(prepare_output(&profile, (rate, block))?);
                eprintln!(
                    "prepared {} {} output channels at {rate} Hz in {:.0} ms",
                    next.kind().as_str(),
                    next.channels(),
                    t.elapsed().as_secs_f64() * 1000.0
                );
                match engine.apply(LiveChange::Prepared(next)) {
                    Ok(()) => applied = Some((rate, block)),
                    Err(engine::Rejected) => eprintln!("change queue full; retrying"),
                }
            }
            if started.elapsed() >= next_report {
                let s = engine.status();
                figures.observe(&s, next_report);
                eprintln!(
                    "{:>4} s  latency p50 {:.1} ms p99 {:.1} ms  ring trough {:.1} ms  underruns {}  device restarts {}  clock {:+.0} ppm  lost {} B  tee-dropped {} B{}",
                    next_report.as_secs(),
                    s.latency.p50_ms,
                    s.latency.p99_ms,
                    s.ring_level_ms,
                    s.underruns,
                    s.output_restarts,
                    s.clock_ppm,
                    s.lost_bytes,
                    dropped.load(Ordering::Relaxed),
                    s.fault
                        .as_ref()
                        .map(|f| format!("  FAULT {f:?}"))
                        .unwrap_or_default()
                );
                next_report += Duration::from_secs(1);
            }
            thread::sleep(Duration::from_millis(33));
        }

        let seconds = started.elapsed().as_secs_f64();
        let (driver_stats, driver_stats_error) = match driver_stats {
            Some((observer, totals)) => {
                let end = observer.stats_sample().map_err(|error| error.to_string());
                let mut totals = totals
                    .lock()
                    .map_err(|_| "driver statistics lock was poisoned".to_owned())?;
                let error = match end {
                    Ok(sample) => {
                        totals.observe(sample);
                        None
                    }
                    Err(error) => Some(error),
                };
                (Some(*totals), error)
            }
            None => (None, None),
        };
        let underruns_while_streaming = figures
            .underruns_while_streaming
            .unwrap_or_else(|| engine.status().underruns);
        let report = engine.stop();
        stop_writer.store(true, Ordering::Release);
        if let Some(handle) = writer_thread {
            handle
                .join()
                .map_err(|_| "the dump writer panicked".to_owned())??;
        }
        eprintln!(
            "stopped: {} blocks, {} underruns, {} device restarts, {} bytes lost, {} recorder bytes lost, latency p50 {:.1} ms p99 {:.1} ms",
            report.blocks,
            report.underruns,
            report.final_status.output_restarts,
            report.lost_bytes,
            dropped.load(Ordering::Relaxed),
            report.final_status.latency.p50_ms,
            report.final_status.latency.p99_ms
        );
        if let Some(path) = &options.report {
            let mut faults = figures.faults;
            if let Some(fault) = &report.final_status.fault {
                faults.push(format!("{fault:?}"));
            }
            let driver = driver_stats.as_ref().map(driver_stats_json);
            let clock_strategy = match report.final_status.clock {
                bio_types::ClockStatus::Trim => "trim",
                bio_types::ClockStatus::Asrc => "asrc",
            };
            let output_mode = match report.final_status.output_mode {
                bio_types::OutputMode::Exclusive => "exclusive",
                bio_types::OutputMode::Shared => "shared",
            };
            let endpoints = report
                .final_status
                .endpoints
                .iter()
                .enumerate()
                .map(|(endpoint, status)| {
                    serde_json::json!({
                        "endpoint": endpoint,
                        "format": status.format.map(|format| format!("{format:?}")),
                        "epoch": status.epoch,
                        "unsupported": status.unsupported,
                    })
                })
                .collect::<Vec<_>>();
            let json = serde_json::json!({
                "mode": output_mode,
                "output_mode": output_mode,
                "output_fell_back": report.final_status.output_fell_back,
                "output_kind": output_kind,
                "output_channels": output_channels,
                "output_channel_mask": output_channel_mask,
                "direct_layouts": options.direct_layouts.iter().map(|value| serde_json::json!({
                    "channels": value.channels,
                    "speakers": value.layout.speakers().iter().copied().map(engine::speaker_name).collect::<Vec<_>>(),
                })).collect::<Vec<_>>(),
                "clock_strategy": clock_strategy,
                "seconds": seconds,
                "blocks": report.blocks,
                "underruns": report.underruns,
                "underruns_while_streaming": underruns_while_streaming,
                "output_restarts": report.final_status.output_restarts,
                "output_reopens": report.final_status.output_reopens,
                "output_reconnects": report.final_status.output_reconnects,
                "output_reconnect_attempts": report.final_status.output_reconnect_attempts,
                "output_mix_channels": report.final_status.output_mix_channels,
                "output_cadence_ratio": report.final_status.output_cadence_ratio,
                "output_cadence_faults": report.final_status.output_cadence_faults,
                "lost_bytes": report.lost_bytes,
                "lane_underrun_frames": report.lane_underrun_frames,
                "dropped_frames": report.dropped_frames,
                "volume_transitions_coalesced": report.final_status.volume_transitions_coalesced,
                "output_overflow_frames": report.output_overflow_frames,
                "idle_blocks": report.idle_blocks,
                "dropped_events": report.dropped_events,
                "endpoints": endpoints,
                "overruns": figures.overruns,
                "faults": faults,
                "latency_p50_ms": report.final_status.latency.p50_ms,
                "latency_p99_ms": report.final_status.latency.p99_ms,
                "ring_trough_min_ms": figures.ring_trough_min_ms,
                "clock_ppm": report.final_status.clock_ppm,
                "clock_ppm_max_abs": figures.clock_ppm_max_abs,
                "tee_dropped_bytes": dropped.load(Ordering::Relaxed),
                "recorder_lost_bytes": dropped.load(Ordering::Relaxed),
                "driver": driver,
                "driver_stats_error": driver_stats_error,
            });
            let text = serde_json::to_string_pretty(&json).map_err(|e| e.to_string())?;
            std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))?;
        }
        Ok(())
    }

    fn driver_stats_json(totals: &DriverStatsTotals) -> serde_json::Value {
        let endpoints = totals
            .first
            .endpoints
            .iter()
            .zip(totals.last.endpoints.iter())
            .enumerate()
            .map(|(endpoint, (before, after))| {
                serde_json::json!({
                    "endpoint": endpoint,
                    "start_total_bytes": before.total_bytes,
                    "end_total_bytes": after.total_bytes,
                    "streamed_bytes": totals.streamed_bytes[endpoint],
                    "start_overruns": before.overruns,
                    "end_overruns": after.overruns,
                    "overruns": totals.overruns[endpoint],
                    "start_lost_bytes": before.lost_bytes,
                    "end_lost_bytes": after.lost_bytes,
                    "lost_bytes": totals.lost_bytes[endpoint],
                    "epoch": after.epoch,
                    "state": after.state,
                })
            })
            .collect::<Vec<_>>();
        serde_json::json!({
            "start_open_count": totals.first.open_count,
            "end_open_count": totals.last.open_count,
            "max_timer_lateness_us": totals.max_timer_lateness_us,
            "endpoints": endpoints,
        })
    }

    /// What `--report` writes besides the engine's own stop report.
    #[derive(Default)]
    struct RunFigures {
        overruns: u64,
        faults: Vec<String>,
        /// Underruns counted when the dump ended. Runtime idle reads now keep
        /// source time advancing while the stream is open, so this snapshot
        /// excludes only the deliberate post-EOF crossing-ring drain.
        underruns_while_streaming: Option<u64>,
        /// Lowest per-second ring trough after the first seconds of priming.
        ring_trough_min_ms: Option<f32>,
        clock_ppm_max_abs: f64,
    }

    impl RunFigures {
        const SETTLE: Duration = Duration::from_secs(5);

        fn observe(&mut self, status: &bio_types::EngineStatus, at: Duration) {
            if at < Self::SETTLE || self.underruns_while_streaming.is_some() {
                return;
            }
            let trough = self.ring_trough_min_ms.get_or_insert(status.ring_level_ms);
            *trough = trough.min(status.ring_level_ms);
            self.clock_ppm_max_abs = self.clock_ppm_max_abs.max(status.clock_ppm.abs());
        }
    }

    pub fn devices(formats: bool, json: bool) -> Result<(), String> {
        if json {
            let endpoints = if formats {
                devices::render_endpoint_formats()
                    .map_err(|e| e.to_string())?
                    .into_iter()
                    .map(|formats| {
                        serde_json::json!({
                            "id": formats.endpoint.id,
                            "name": formats.endpoint.name,
                            "default": formats.endpoint.is_default,
                            "mix": {
                                "rate": formats.mix.rate,
                                "channels": formats.mix.channels,
                                "channel_mask": formats.mix.channel_mask,
                            },
                            "layouts": formats.layouts.into_iter().map(|layout| serde_json::json!({
                                "channels": layout.channels,
                                "channel_mask": layout.channel_mask,
                                "exclusive": layout.exclusive,
                            })).collect::<Vec<_>>(),
                        })
                    })
                    .collect::<Vec<_>>()
            } else {
                devices::render_endpoints()
                    .map_err(|e| e.to_string())?
                    .into_iter()
                    .map(|endpoint| {
                        serde_json::json!({
                            "id": endpoint.id,
                            "name": endpoint.name,
                            "default": endpoint.is_default,
                        })
                    })
                    .collect::<Vec<_>>()
            };
            println!(
                "{}",
                serde_json::to_string_pretty(&endpoints).map_err(|e| e.to_string())?
            );
            return Ok(());
        }
        match BioRing::open() {
            Ok(ring) => {
                let info = ring.info();
                println!(
                    "Spatial SoftReceiver driver: endpoint mask 0x{:X}, QPC {} Hz",
                    info.endpoint_mask, info.qpc_frequency
                );
                for endpoint in 0..bio_abi::MAX_ENDPOINTS {
                    if info.endpoint_mask & (1_u32 << endpoint) != 0 {
                        let formats = info.endpoints[endpoint];
                        let codecs = [
                            (bio_abi::FORMAT_AC3, "AC-3"),
                            (bio_abi::FORMAT_EAC3, "E-AC-3"),
                            (bio_abi::FORMAT_EAC3_ATMOS, "E-AC-3 Atmos"),
                            (bio_abi::FORMAT_TRUEHD, "TrueHD"),
                            (bio_abi::FORMAT_MAT20, "MAT 2.0"),
                            (bio_abi::FORMAT_MAT21, "MAT 2.1"),
                            (bio_abi::FORMAT_DTS, "DTS"),
                            (bio_abi::FORMAT_DTS_HD, "DTS-HD"),
                        ]
                        .into_iter()
                        .filter_map(|(bit, name)| (formats.codecs & bit != 0).then_some(name))
                        .collect::<Vec<_>>();
                        let rates = bio_abi::PCM_RATES
                            .iter()
                            .enumerate()
                            .filter(|(i, _)| formats.pcm.rates & (1 << i) != 0)
                            .map(|(_, rate)| rate.to_string())
                            .collect::<Vec<_>>();
                        let shapes = ["int16", "int24 packed", "int24-in-32", "int32", "float32"]
                            .into_iter()
                            .enumerate()
                            .filter_map(|(i, shape)| {
                                (formats.pcm.shapes & (1 << i) != 0).then_some(shape)
                            })
                            .collect::<Vec<_>>();
                        let mut direct = Vec::new();
                        let mut channel = 1_u32;
                        while channel <= 32 {
                            if formats.pcm.direct_channels & (1 << (channel - 1)) == 0 {
                                channel += 1;
                                continue;
                            }
                            let first = channel;
                            while channel < 32 && formats.pcm.direct_channels & (1 << channel) != 0
                            {
                                channel += 1;
                            }
                            direct.push(if first == channel {
                                first.to_string()
                            } else {
                                format!("{first}-{channel}")
                            });
                            channel += 1;
                        }
                        println!("  endpoint {endpoint}: codecs [{}]", codecs.join(", "));
                        println!(
                            "    PCM: rates [{}] Hz; shapes [{}]; masked channels <= {}; direct-out channels [{}]",
                            rates.join(", "),
                            shapes.join(", "),
                            formats.pcm.max_masked_channels,
                            direct.join(", ")
                        );
                    }
                }
            }
            Err(CaptureError::NotPresent) => println!("Spatial SoftReceiver driver: not present"),
            Err(error) => println!("Spatial SoftReceiver driver: {error}"),
        }
        if formats {
            for formats in devices::render_endpoint_formats().map_err(|e| e.to_string())? {
                let mark = if formats.endpoint.is_default {
                    "*"
                } else {
                    " "
                };
                println!(
                    "{mark} {}  [{}]",
                    formats.endpoint.name, formats.endpoint.id
                );
                println!(
                    "    mix: {} Hz, {} channels, mask {:#x}",
                    formats.mix.rate, formats.mix.channels, formats.mix.channel_mask
                );
                for layout in formats.layouts {
                    println!(
                        "    {:>2} channels mask {:#07x}: exclusive {}",
                        layout.channels,
                        layout.channel_mask,
                        if layout.exclusive { "yes" } else { "no" }
                    );
                }
            }
        } else {
            for endpoint in devices::render_endpoints().map_err(|e| e.to_string())? {
                let mark = if endpoint.is_default { "*" } else { " " };
                println!("{mark} {}  [{}]", endpoint.name, endpoint.id);
            }
        }
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use bio_abi::{BioStats, EndpointStats, RecordWriter};

        fn stats(total_bytes: u64) -> BioStats {
            BioStats {
                struct_size: size_of::<BioStats>() as u32,
                open_count: 1,
                max_timer_lateness_us: 0,
                reserved: 0,
                endpoints: [EndpointStats {
                    total_bytes,
                    overruns: total_bytes / 10,
                    lost_bytes: total_bytes / 5,
                    epoch: 1,
                    state: 1,
                }; bio_abi::MAX_ENDPOINTS],
            }
        }

        #[test]
        fn stale_driver_stats_sample_cannot_enter_a_new_generation() {
            let mut totals = DriverStatsTotals::new(DriverStatsSample {
                generation: 0,
                stats: stats(100),
            });
            let stale = DriverStatsSample {
                generation: 0,
                stats: stats(150),
            };
            totals.begin_generation(DriverStatsSample {
                generation: 1,
                stats: stats(20),
            });
            totals.observe(stale);

            assert_eq!(totals.generation, 1);
            assert_eq!(totals.last, stats(20));
            assert_eq!(totals.streamed_bytes, [0; bio_abi::MAX_ENDPOINTS]);
            assert_eq!(totals.overruns, [0; bio_abi::MAX_ENDPOINTS]);
            assert_eq!(totals.lost_bytes, [0; bio_abi::MAX_ENDPOINTS]);
        }

        fn record_qpc(bytes: &[u8]) -> u64 {
            u64::from_le_bytes(bytes[QPC_AT..QPC_AT + 8].try_into().unwrap())
        }

        #[test]
        fn paced_dump_releases_records_at_their_pace_and_restamps_them() {
            // 10 MHz dump clock: a stream start at 0, data at 10 and 20 ms.
            let mut dump = dump_header_bytes(10_000_000);
            let mut buf = vec![0_u8; 4096];
            let mut writer = RecordWriter::new(&mut buf);
            assert!(writer.stream_start(1, 0));
            assert!(writer.data(1, 0, 100_000, &[1; 16]));
            assert!(writer.data(1, 16, 200_000, &[2; 16]));
            let len = writer.len();
            dump.extend_from_slice(&buf[..len]);
            let ended = Arc::new(AtomicBool::new(false));
            let mut paced = PacedDump::new(&dump, ended.clone(), false).unwrap();
            let frequency = qpc::frequency();

            let mut out = vec![0_u8; 4096];
            let before = qpc::now();
            let n = paced.read(&mut out, Duration::ZERO).unwrap();
            assert_eq!(n, bio_abi::record_size(0), "only the stream start is due");
            assert!(record_qpc(&out) >= before);

            // A record is stamped when it is released, never before it is due
            // (counted from the first read, which is after `before`). A late
            // release delays only its own stamp, so each one is checked
            // against its due time rather than against the one before it.
            for due_ms in [10, 20] {
                let n = paced.read(&mut out, Duration::from_millis(500)).unwrap();
                assert_eq!(n, bio_abi::record_size(16));
                assert!(
                    record_qpc(&out) - before >= due(frequency, due_ms),
                    "released before {due_ms} ms"
                );
            }
            assert_eq!(out[HEADER_BYTES], 2);

            assert!(!ended.load(Ordering::Acquire));
            assert_eq!(
                paced.read(&mut out, Duration::ZERO),
                Err(CaptureError::Ended)
            );
            assert!(ended.load(Ordering::Acquire));
        }

        /// QPC ticks from the origin to a record due `ms` later, rounded down
        /// as the pacing does.
        fn due(frequency: u64, ms: u64) -> u64 {
            frequency * ms / 1000
        }

        fn record_pos(bytes: &[u8]) -> u64 {
            u64::from_le_bytes(bytes[POS_AT..POS_AT + 8].try_into().unwrap())
        }

        #[test]
        fn paced_dump_loops_with_continuous_positions_and_times() {
            // Data at 10 and 20 ms, 16 bytes each: one pass spans 20 ms and
            // 32 bytes, so the repeat is due at 30 and 40 ms at 32 and 48.
            let mut dump = dump_header_bytes(10_000_000);
            let mut buf = vec![0_u8; 4096];
            let mut writer = RecordWriter::new(&mut buf);
            assert!(writer.stream_start(1, 0));
            assert!(writer.data(1, 0, 100_000, &[1; 16]));
            assert!(writer.data(1, 16, 200_000, &[2; 16]));
            assert!(writer.stream_stop(1, 250_000));
            let len = writer.len();
            dump.extend_from_slice(&buf[..len]);
            let ended = Arc::new(AtomicBool::new(false));
            let mut paced = PacedDump::new(&dump, ended.clone(), true).unwrap();
            let frequency = qpc::frequency();

            let mut out = vec![0_u8; 4096];
            let before = qpc::now();
            assert_eq!(
                paced.read(&mut out, Duration::ZERO).unwrap(),
                bio_abi::record_size(0)
            );
            for (pass, payload, pos, due_ms) in [
                (0, 1, 0, 10),
                (0, 2, 16, 20),
                (1, 1, 32, 30),
                (1, 2, 48, 40),
                (2, 1, 64, 50),
            ] {
                let n = paced.read(&mut out, Duration::from_millis(500)).unwrap();
                assert_eq!(n, bio_abi::record_size(16), "pass {pass}");
                assert_eq!(out[HEADER_BYTES], payload, "pass {pass}");
                assert_eq!(record_pos(&out), pos, "pass {pass}");
                assert!(
                    record_qpc(&out) - before >= due(frequency, due_ms),
                    "released before {due_ms} ms in pass {pass}"
                );
            }
            // The stop record after the span never plays while looping.
            assert!(!ended.load(Ordering::Acquire));
        }

        #[test]
        fn a_dump_that_ends_on_data_keeps_looping() {
            // The fixtures end on their last data record, with no stop.
            let mut dump = dump_header_bytes(10_000_000);
            let mut buf = vec![0_u8; 4096];
            let mut writer = RecordWriter::new(&mut buf);
            assert!(writer.data(1, 0, 100_000, &[1; 16]));
            assert!(writer.data(1, 16, 200_000, &[2; 16]));
            let len = writer.len();
            dump.extend_from_slice(&buf[..len]);
            let ended = Arc::new(AtomicBool::new(false));
            let mut paced = PacedDump::new(&dump, ended.clone(), true).unwrap();
            let mut out = vec![0_u8; 4096];
            for pos in [0, 16, 32, 48, 64] {
                let n = paced.read(&mut out, Duration::from_millis(500)).unwrap();
                assert_eq!(n, bio_abi::record_size(16));
                assert_eq!(record_pos(&out), pos);
            }
            assert!(!ended.load(Ordering::Acquire));
        }

        #[test]
        fn looping_needs_two_data_records() {
            let mut dump = dump_header_bytes(10_000_000);
            let mut buf = vec![0_u8; 256];
            let mut writer = RecordWriter::new(&mut buf);
            assert!(writer.data(1, 0, 100_000, &[1; 16]));
            let len = writer.len();
            dump.extend_from_slice(&buf[..len]);
            let ended = Arc::new(AtomicBool::new(false));
            assert!(PacedDump::new(&dump, ended, true).is_err());
        }
    }
}

#[cfg(not(windows))]
mod platform {
    use std::path::{Path, PathBuf};

    use super::{CaptureKind, Clock};

    #[allow(dead_code)]
    pub struct PlayOptions {
        pub source: CaptureKind,
        pub endpoints: Option<String>,
        pub profile: PathBuf,
        pub output: Option<String>,
        pub shared: bool,
        pub allow_shared_fallback: bool,
        pub rate: u32,
        pub period_ms: Option<f64>,
        pub target_ms: f32,
        pub clock: Clock,
        pub record: Option<PathBuf>,
        pub seconds: Option<f64>,
        pub from_dump: Option<PathBuf>,
        pub repeat: bool,
        pub mute: bool,
        pub report: Option<PathBuf>,
        pub direct_layouts: Vec<engine::DirectLayoutOverride>,
    }

    pub fn record(_source: CaptureKind, _out: &Path, _seconds: f64) -> Result<(), String> {
        Err("bio-live runs on Windows only".to_owned())
    }

    pub fn play(_options: PlayOptions) -> Result<(), String> {
        Err("bio-live runs on Windows only".to_owned())
    }

    pub fn devices(_formats: bool, _json: bool) -> Result<(), String> {
        Err("bio-live runs on Windows only".to_owned())
    }
}

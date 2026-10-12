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
#[command(version, about = "BIO Atmos live tool for the SAL development PC")]
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

#[derive(Subcommand)]
enum Command {
    /// Record the SAL capture ring to a dump file (run as administrator).
    Record {
        /// Output file (`.biodump`).
        #[arg(long)]
        out: PathBuf,
        /// Seconds to record.
        #[arg(long, default_value_t = 60.0)]
        seconds: f64,
    },
    /// Play the SAL ring through a BRIR profile (run as administrator).
    Play {
        /// Profile TOML (see crates/brir).
        #[arg(long)]
        profile: PathBuf,
        /// Part of the output device's name; the default device when omitted.
        #[arg(long)]
        output: Option<String>,
        /// Use WASAPI shared mode instead of exclusive.
        #[arg(long)]
        shared: bool,
        /// Device sample rate.
        #[arg(long, default_value_t = 48_000)]
        rate: u32,
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
    },
    /// List render endpoints (name, default flag, id).
    Devices,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Record { out, seconds } => platform::record(&out, seconds),
        Command::Play {
            profile,
            output,
            shared,
            rate,
            target_ms,
            clock,
            record,
            seconds,
            from_dump,
            repeat,
            mute,
            report,
        } => platform::play(platform::PlayOptions {
            profile,
            output,
            shared,
            rate,
            target_ms,
            clock,
            record,
            seconds,
            from_dump,
            repeat,
            mute,
            report,
        }),
        Command::Devices => platform::devices(),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("bio-live: {message}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(windows)]
mod platform {
    use std::cell::RefCell;
    use std::collections::{BTreeMap, HashMap};
    use std::fs::File;
    use std::io::{BufRead, BufWriter, Write};
    use std::mem::offset_of;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::thread;
    use std::time::{Duration, Instant};

    use bio_abi::{Record, RecordHeader, dump_header_bytes, records};
    use bio_sys::mmcss::{self, MmcssGuard};
    use bio_sys::output::WasapiOpener;
    use bio_sys::{devices, qpc, sal::SalRing};
    use bio_types::{CaptureError, CaptureSource, TrimUnsupported};
    use brir::PreparedBrir;
    use engine::{
        ClockPreference, EngineConfig, EngineEvent, LiveChange, Prepared, RealtimeThread,
        partition_for, scene_shape,
    };

    use super::Clock;

    pub struct PlayOptions {
        pub profile: PathBuf,
        pub output: Option<String>,
        pub shared: bool,
        pub rate: u32,
        pub target_ms: f32,
        pub clock: Clock,
        pub record: Option<PathBuf>,
        pub seconds: Option<f64>,
        pub from_dump: Option<PathBuf>,
        pub repeat: bool,
        pub mute: bool,
        pub report: Option<PathBuf>,
    }

    fn open_ring() -> Result<SalRing, String> {
        SalRing::open().map_err(|e| match e {
            CaptureError::NotPresent => {
                "the SAL capture device is not present (is the SAL test-signed driver installed?)"
                    .to_owned()
            }
            CaptureError::AccessDenied => {
                "access to the SAL capture device was denied; run from an administrator prompt"
                    .to_owned()
            }
            other => other.to_string(),
        })
    }

    fn create_dump(path: &Path) -> Result<BufWriter<File>, String> {
        let file = File::create(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut writer = BufWriter::with_capacity(1 << 20, file);
        writer
            .write_all(&dump_header_bytes(qpc::frequency()))
            .map_err(|e| e.to_string())?;
        Ok(writer)
    }

    pub fn record(out: &Path, seconds: f64) -> Result<(), String> {
        let mut ring = open_ring()?;
        let mut writer = create_dump(out)?;
        let mut buf = vec![0_u8; 1 << 20];
        let mut bytes = 0_u64;
        let mut endpoints = BTreeMap::<u16, (u64, u64, u64)>::new();
        let started = Instant::now();
        let mut next_report = Duration::from_secs(1);
        eprintln!(
            "recording the SAL ring to {} for {seconds} s; start the source now",
            out.display()
        );
        while started.elapsed().as_secs_f64() < seconds {
            let n = ring
                .read(&mut buf, Duration::from_millis(100))
                .map_err(|e| e.to_string())?;
            if n > 0 {
                writer.write_all(&buf[..n]).map_err(|e| e.to_string())?;
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

    /// Copies every chunk the capture thread reads into a byte ring that a
    /// writer thread drains to the dump file, so the real-time thread never
    /// touches the file. A chunk that does not fit is dropped whole (records
    /// stay intact) and counted.
    struct TeeCapture<C> {
        inner: C,
        tap: Option<rtrb::Producer<u8>>,
        dropped: Arc<AtomicU64>,
    }

    impl<C: CaptureSource> CaptureSource for TeeCapture<C> {
        fn read(&mut self, buf: &mut [u8], wait: Duration) -> Result<usize, CaptureError> {
            let n = self.inner.read(buf, wait)?;
            if let Some(tap) = self.tap.as_mut()
                && n > 0
            {
                match tap.write_chunk_uninit(n) {
                    Ok(chunk) => {
                        chunk.fill_from_iter(buf[..n].iter().copied());
                    }
                    Err(_) => {
                        self.dropped.fetch_add(n as u64, Ordering::Relaxed);
                    }
                }
            }
            Ok(n)
        }

        fn set_rate_trim(&mut self, ppm: f64) -> Result<(), TrimUnsupported> {
            self.inner.set_rate_trim(ppm)
        }
    }

    enum Source {
        Sal(SalRing),
        Dump(PacedDump),
    }

    impl CaptureSource for Source {
        fn read(&mut self, buf: &mut [u8], wait: Duration) -> Result<usize, CaptureError> {
            match self {
                Self::Sal(ring) => ring.read(buf, wait),
                Self::Dump(dump) => dump.read(buf, wait),
            }
        }

        fn set_rate_trim(&mut self, ppm: f64) -> Result<(), TrimUnsupported> {
            match self {
                Self::Sal(ring) => ring.set_rate_trim(ppm),
                Self::Dump(dump) => dump.set_rate_trim(ppm),
            }
        }
    }

    const HEADER_BYTES: usize = size_of::<RecordHeader>();
    const KIND_AT: usize = offset_of!(RecordHeader, kind);
    const POS_AT: usize = offset_of!(RecordHeader, byte_pos);
    const LEN_AT: usize = offset_of!(RecordHeader, len);
    const QPC_AT: usize = offset_of!(RecordHeader, qpc);
    const QPC_WRITTEN_AT: usize = offset_of!(RecordHeader, qpc_written);

    /// Replays a dump at the pace its QPC stamps give, restamping every record
    /// with the QPC at which it was handed out, as a driver read would. The
    /// whole live path (real-time threads, WASAPI, clock correction, latency)
    /// then runs on a PC without SAL; the source clock is the QPC.
    struct PacedDump {
        stream: Vec<u8>,
        offset: usize,
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

    /// The data records `--loop` repeats, and how far one pass moves the
    /// dump's clock and byte position.
    #[derive(Clone, Copy, Debug)]
    struct LoopSpan {
        start: usize,
        end: usize,
        qpc: u64,
        bytes: u64,
    }

    /// Size (padding included, clamped to the stream) and kind, byte position,
    /// payload length and dump QPC of the record at `at`.
    fn record_at(stream: &[u8], at: usize) -> (usize, u16, u64, u32, u64) {
        let header = &stream[at..at + HEADER_BYTES];
        let kind = u16::from_le_bytes(header[KIND_AT..KIND_AT + 2].try_into().expect("2 bytes"));
        let pos = u64::from_le_bytes(header[POS_AT..POS_AT + 8].try_into().expect("8 bytes"));
        let len = u32::from_le_bytes(header[LEN_AT..LEN_AT + 4].try_into().expect("4 bytes"));
        let qpc = u64::from_le_bytes(header[QPC_AT..QPC_AT + 8].try_into().expect("8 bytes"));
        let size = bio_abi::record_size(len as usize).min(stream.len() - at);
        (size, kind, pos, len, qpc)
    }

    /// The span from the first to the end of the last data record. One pass
    /// advances the clock by the first-to-last distance plus one record
    /// spacing, so the repeat lands where the next record would have.
    fn loop_span(stream: &[u8]) -> Result<LoopSpan, String> {
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
        Ok(LoopSpan {
            start: first.0,
            end: last.0 + last.1,
            qpc: last.4.saturating_sub(first.4) + spacing,
            bytes: (last.2 + u64::from(last.3)).saturating_sub(first.2),
        })
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
            let repeat = if repeat {
                Some(loop_span(stream)?)
            } else {
                None
            };
            Ok(Self {
                stream: stream.to_vec(),
                offset: 0,
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

    impl CaptureSource for PacedDump {
        fn read(&mut self, buf: &mut [u8], wait: Duration) -> Result<usize, CaptureError> {
            let deadline = qpc::now() + (wait.as_secs_f64() * self.frequency as f64) as u64;
            loop {
                if self.offset == self.stream.len() {
                    self.ended.store(true, Ordering::Release);
                    return Err(CaptureError::Ended);
                }
                let now = qpc::now();
                let mut written = 0;
                while self.offset < self.stream.len() {
                    if let Some(span) = self.repeat
                        && self.offset == span.end
                    {
                        self.offset = span.start;
                        self.qpc_shift += span.qpc;
                        self.pos_shift += span.bytes;
                    }
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
                }
                if written > 0 || now >= deadline {
                    return Ok(written);
                }
                thread::sleep(Duration::from_millis(1));
            }
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

    pub fn play(options: PlayOptions) -> Result<(), String> {
        let profile = brir::load_profile(&options.profile).map_err(|e| e.to_string())?;
        let endpoint = devices::find(options.output.as_deref()).map_err(|e| e.to_string())?;
        eprintln!("output: {}", endpoint.name);
        let ended = Arc::new(AtomicBool::new(false));
        let source = match &options.from_dump {
            Some(path) => {
                let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
                Source::Dump(PacedDump::new(&bytes, ended.clone(), options.repeat)?)
            }
            None => Source::Sal(open_ring()?),
        };

        let dropped = Arc::new(AtomicU64::new(0));
        let stop_writer = Arc::new(AtomicBool::new(false));
        let (tap, writer_thread) = match &options.record {
            Some(path) => {
                let mut writer = create_dump(path)?;
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
        };

        let config = EngineConfig {
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
            period: None,
        };
        let mut engine = engine::start_with_clock(config, capture, output, qpc::frequency())
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

        let mut prepared: HashMap<(u32, usize), Arc<PreparedBrir>> = HashMap::new();
        let mut wanted: Option<(u32, usize)> = None;
        let mut applied: Option<(u32, usize)> = None;
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
                    EngineEvent::FormatChanged { epoch, format } => {
                        eprintln!("epoch {epoch}: {format:?}");
                        if let Some(shape) = scene_shape(&format) {
                            wanted = Some(shape);
                        }
                    }
                    EngineEvent::NeedsBrir { rate, block } => wanted = Some((rate, block)),
                    EngineEvent::Overrun { lost_bytes } => {
                        eprintln!("overrun: {lost_bytes} bytes lost");
                        figures.overruns += 1;
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
                let brir = match prepared.get(&(rate, block)) {
                    Some(brir) => brir.clone(),
                    None => {
                        let t = Instant::now();
                        let brir = brir::prepare(&profile, rate, partition_for(block))
                            .map_err(|e| e.to_string())?;
                        eprintln!(
                            "prepared {} directions at {rate} Hz, partition {} ({} taps) in {:.0} ms",
                            brir.layout.len(),
                            brir.partition,
                            brir.ir_frames,
                            t.elapsed().as_secs_f64() * 1000.0
                        );
                        let brir = Arc::new(brir);
                        prepared.insert((rate, block), brir.clone());
                        brir
                    }
                };
                let next = Box::new(Prepared::new(brir, block).map_err(|e| e.to_string())?);
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
            "stopped: {} blocks, {} underruns, {} device restarts, {} bytes lost, latency p50 {:.1} ms p99 {:.1} ms",
            report.blocks,
            report.underruns,
            report.final_status.output_restarts,
            report.lost_bytes,
            report.final_status.latency.p50_ms,
            report.final_status.latency.p99_ms
        );
        if let Some(path) = &options.report {
            let mut faults = figures.faults;
            if let Some(fault) = &report.final_status.fault {
                faults.push(format!("{fault:?}"));
            }
            let json = serde_json::json!({
                "mode": if options.shared { "shared" } else { "exclusive" },
                "seconds": seconds,
                "blocks": report.blocks,
                "underruns": report.underruns,
                "underruns_while_streaming": underruns_while_streaming,
                "output_restarts": report.final_status.output_restarts,
                "lost_bytes": report.lost_bytes,
                "overruns": figures.overruns,
                "faults": faults,
                "latency_p50_ms": report.final_status.latency.p50_ms,
                "latency_p99_ms": report.final_status.latency.p99_ms,
                "ring_trough_min_ms": figures.ring_trough_min_ms,
                "clock_ppm": report.final_status.clock_ppm,
                "clock_ppm_max_abs": figures.clock_ppm_max_abs,
                "tee_dropped_bytes": dropped.load(Ordering::Relaxed),
            });
            let text = serde_json::to_string_pretty(&json).map_err(|e| e.to_string())?;
            std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))?;
        }
        Ok(())
    }

    /// What `--report` writes besides the engine's own stop report.
    #[derive(Default)]
    struct RunFigures {
        overruns: u64,
        faults: Vec<String>,
        /// Underruns counted when the dump ended; after that the crossing
        /// ring drains and every period underruns.
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

    pub fn devices() -> Result<(), String> {
        for endpoint in devices::render_endpoints().map_err(|e| e.to_string())? {
            let mark = if endpoint.is_default { "*" } else { " " };
            println!("{mark} {}  [{}]", endpoint.name, endpoint.id);
        }
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use bio_abi::RecordWriter;

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

            let n = paced.read(&mut out, Duration::from_millis(500)).unwrap();
            assert_eq!(n, bio_abi::record_size(16));
            let first = record_qpc(&out);
            assert!(first - before >= frequency * 9 / 1000, "released early");
            let n = paced.read(&mut out, Duration::from_millis(500)).unwrap();
            assert_eq!(n, bio_abi::record_size(16));
            let second = record_qpc(&out);
            assert!(second - first >= frequency * 9 / 1000, "released early");
            assert_eq!(out[HEADER_BYTES], 2);

            assert!(!ended.load(Ordering::Acquire));
            assert_eq!(
                paced.read(&mut out, Duration::ZERO),
                Err(CaptureError::Ended)
            );
            assert!(ended.load(Ordering::Acquire));
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
            let mut previous = before;
            for (pass, payload, pos) in [(0, 1, 0), (0, 2, 16), (1, 1, 32), (1, 2, 48), (2, 1, 64)]
            {
                let n = paced.read(&mut out, Duration::from_millis(500)).unwrap();
                assert_eq!(n, bio_abi::record_size(16), "pass {pass}");
                assert_eq!(out[HEADER_BYTES], payload, "pass {pass}");
                assert_eq!(record_pos(&out), pos, "pass {pass}");
                let at = record_qpc(&out);
                assert!(
                    at - previous >= frequency * 9 / 1000,
                    "released early in pass {pass}"
                );
                previous = at;
            }
            // The stop record after the span never plays while looping.
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

    use super::Clock;

    #[allow(dead_code)]
    pub struct PlayOptions {
        pub profile: PathBuf,
        pub output: Option<String>,
        pub shared: bool,
        pub rate: u32,
        pub target_ms: f32,
        pub clock: Clock,
        pub record: Option<PathBuf>,
        pub seconds: Option<f64>,
        pub from_dump: Option<PathBuf>,
        pub repeat: bool,
        pub mute: bool,
        pub report: Option<PathBuf>,
    }

    pub fn record(_out: &Path, _seconds: f64) -> Result<(), String> {
        Err("bio-live runs on Windows only".to_owned())
    }

    pub fn play(_options: PlayOptions) -> Result<(), String> {
        Err("bio-live runs on Windows only".to_owned())
    }

    pub fn devices() -> Result<(), String> {
        Err("bio-live runs on Windows only".to_owned())
    }
}

#![forbid(unsafe_code)]

//! bio-replay: reads a diagnostic recording (driver record dump) offline.
//!
//! `inspect` summarises a dump as JSON. `render` runs the dump through the same
//! decode, render and convolver chain as the live app and writes the binaural
//! WAV; it links no Windows code, so it runs in Linux CI.

use std::collections::BTreeMap;
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::mem::{offset_of, size_of};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use bio_abi::{
    ENDPOINT_PRIMARY, ENDPOINT_SECOND, RECORD_KIND_READ_END, RECORD_KIND_RECORDER_GAP, Record,
    RecordHeader, reads, records, split_dump,
};
use clap::{Parser, Subcommand, ValueEnum};
use engine::{
    Chain, ChainConfig, ChainEvent, EndpointFilter, OutputKind, OutputProfile, Prepared,
    load_output_profile, partition_for, scene_shape,
};
use serde::Serialize;
use sha2::{Digest, Sha256};

/// The release tag's version when release.yml builds, the crate version else.
const VERSION: &str = match option_env!("BIO_RELEASE_VERSION") {
    Some(v) => v,
    None => env!("CARGO_PKG_VERSION"),
};

#[derive(Parser)]
#[command(version = VERSION, about = "Offline reader for Spatial SoftReceiver driver record dumps")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
enum EndpointArg {
    #[default]
    All,
    #[value(name = "0")]
    Primary,
    #[value(name = "1")]
    Second,
    #[value(name = "2")]
    Passthrough,
    #[value(name = "3")]
    Reserved,
}

impl EndpointArg {
    fn filter(self) -> EndpointFilter {
        match self {
            Self::All => EndpointFilter::All,
            Self::Primary => EndpointFilter::Only(ENDPOINT_PRIMARY),
            Self::Second => EndpointFilter::Only(ENDPOINT_SECOND),
            Self::Passthrough => EndpointFilter::Only(2),
            Self::Reserved => EndpointFilter::Only(3),
        }
    }

    fn includes(self, endpoint: u16) -> bool {
        self.filter().includes(endpoint)
    }
}

#[derive(Subcommand)]
enum Command {
    /// Print a JSON summary of a dump: formats per endpoint and epoch, record counts, losses.
    Inspect { dump: PathBuf },
    /// Render a diagnostic dump to a deterministic 32-bit float WAV.
    Render {
        dump: PathBuf,
        #[arg(long)]
        profile: PathBuf,
        #[arg(long)]
        out: PathBuf,
        #[arg(long)]
        report: Option<PathBuf>,
        /// Override a zero-mask PCM order, as CHANNELS=PRESET or CHANNELS=SPEAKER,SPEAKER,...
        #[arg(long = "direct-layout")]
        direct_layouts: Vec<engine::DirectLayoutOverride>,
        /// Render all endpoints or only endpoint 0, 1, 2, or 3.
        #[arg(long, value_enum, default_value_t = EndpointArg::All)]
        endpoint: EndpointArg,
        /// Time to retain a completed interval for late entrance data.
        #[arg(long, default_value = "2")]
        arrival_slack_ms: f64,
    },
}

#[derive(Serialize, Default)]
struct EndpointSummary {
    records: u64,
    data_bytes: u64,
}

#[derive(Serialize, Default)]
struct Summary {
    abi_version: u32,
    dump_format_version: u32,
    qpc_frequency: u64,
    records: BTreeMap<&'static str, u64>,
    data_bytes: u64,
    lost_bytes: u64,
    read_boundaries: bool,
    reads: u64,
    read_end_markers: u64,
    recorder_gap_markers: u64,
    recorder_lost_bytes: u64,
    epochs: BTreeMap<String, String>,
    endpoints: BTreeMap<u16, EndpointSummary>,
    errors: Vec<String>,
}

#[derive(Debug, Serialize, Default)]
struct EndpointVolume {
    gain: f32,
    muted: bool,
}

#[derive(Debug, Serialize, Default)]
struct RenderReport {
    epochs: BTreeMap<String, String>,
    blocks: u64,
    frames_written: u64,
    errors: Vec<String>,
    needs_brir: u64,
    lane_underrun_frames: u64,
    dropped_frames: u64,
    suppressed_errors: u64,
    endpoint_volumes: BTreeMap<u16, EndpointVolume>,
    direct_layouts: BTreeMap<u16, Vec<&'static str>>,
    read_boundaries: bool,
    recorder_lost_bytes: u64,
    output_kind: &'static str,
    output_channels: usize,
    output_sha256: String,
}

enum OutputWriter {
    Headphones(hound::WavWriter<BufWriter<std::fs::File>>),
    Speakers(ExtensibleWavWriter),
}

impl OutputWriter {
    fn create(
        path: &Path,
        kind: OutputKind,
        channels: usize,
        rate: u32,
        channel_mask: u32,
    ) -> Result<Self, String> {
        match kind {
            OutputKind::Headphones => {
                let channels = u16::try_from(channels)
                    .map_err(|_| format!("output has too many channels: {channels}"))?;
                hound::WavWriter::create(
                    path,
                    hound::WavSpec {
                        channels,
                        sample_rate: rate,
                        bits_per_sample: 32,
                        sample_format: hound::SampleFormat::Float,
                    },
                )
                .map(Self::Headphones)
                .map_err(|error| format!("{}: {error}", path.display()))
            }
            OutputKind::Speakers => {
                ExtensibleWavWriter::create(path, channels, rate, channel_mask).map(Self::Speakers)
            }
        }
    }

    fn write_sample(&mut self, sample: f32) -> Result<(), hound::Error> {
        match self {
            Self::Headphones(writer) => writer.write_sample(sample),
            Self::Speakers(writer) => writer.write_sample(sample).map_err(hound::Error::from),
        }
    }

    fn finalize(self, path: &Path) -> Result<(), String> {
        match self {
            Self::Headphones(writer) => writer
                .finalize()
                .map_err(|error| format!("{}: {error}", path.display())),
            Self::Speakers(writer) => writer.finalize(path),
        }
    }
}

struct ExtensibleWavWriter {
    writer: BufWriter<std::fs::File>,
    data_bytes: u64,
}

impl ExtensibleWavWriter {
    fn create(path: &Path, channels: usize, rate: u32, channel_mask: u32) -> Result<Self, String> {
        let channels = u16::try_from(channels)
            .map_err(|_| format!("output has too many channels: {channels}"))?;
        if channels == 0 {
            return Err("output has no channels".into());
        }
        let block_align = channels
            .checked_mul(4)
            .ok_or_else(|| "output block alignment overflows WAVE".to_string())?;
        let byte_rate = rate
            .checked_mul(u32::from(block_align))
            .ok_or_else(|| "output byte rate overflows WAVE".to_string())?;
        let file =
            std::fs::File::create(path).map_err(|error| format!("{}: {error}", path.display()))?;
        let mut writer = BufWriter::new(file);
        writer
            .write_all(b"RIFF\0\0\0\0WAVEfmt ")
            .and_then(|()| writer.write_all(&40_u32.to_le_bytes()))
            .and_then(|()| writer.write_all(&0xfffe_u16.to_le_bytes()))
            .and_then(|()| writer.write_all(&channels.to_le_bytes()))
            .and_then(|()| writer.write_all(&rate.to_le_bytes()))
            .and_then(|()| writer.write_all(&byte_rate.to_le_bytes()))
            .and_then(|()| writer.write_all(&block_align.to_le_bytes()))
            .and_then(|()| writer.write_all(&32_u16.to_le_bytes()))
            .and_then(|()| writer.write_all(&22_u16.to_le_bytes()))
            .and_then(|()| writer.write_all(&32_u16.to_le_bytes()))
            .and_then(|()| writer.write_all(&channel_mask.to_le_bytes()))
            .and_then(|()| {
                writer.write_all(&[
                    0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xaa, 0x00,
                    0x38, 0x9b, 0x71,
                ])
            })
            .and_then(|()| writer.write_all(b"data\0\0\0\0"))
            .map_err(|error| format!("{}: {error}", path.display()))?;
        Ok(Self {
            writer,
            data_bytes: 0,
        })
    }

    fn write_sample(&mut self, sample: f32) -> std::io::Result<()> {
        self.writer.write_all(&sample.to_le_bytes())?;
        self.data_bytes = self.data_bytes.saturating_add(4);
        Ok(())
    }

    fn finalize(mut self, path: &Path) -> Result<(), String> {
        let data_bytes = u32::try_from(self.data_bytes)
            .map_err(|_| "rendered output exceeds the 4 GiB RIFF limit".to_string())?;
        let riff_size = 60_u32
            .checked_add(data_bytes)
            .ok_or_else(|| "rendered output exceeds the 4 GiB RIFF limit".to_string())?;
        self.writer
            .flush()
            .and_then(|()| self.writer.seek(SeekFrom::Start(4)).map(|_| ()))
            .and_then(|()| self.writer.write_all(&riff_size.to_le_bytes()))
            .and_then(|()| self.writer.seek(SeekFrom::Start(64)).map(|_| ()))
            .and_then(|()| self.writer.write_all(&data_bytes.to_le_bytes()))
            .and_then(|()| self.writer.flush())
            .map_err(|error| format!("{}: {error}", path.display()))
    }
}

fn epoch_key(endpoint: u16, epoch: u32) -> String {
    format!("{endpoint}:{epoch}")
}

fn inspect(bytes: &[u8]) -> Result<Summary, String> {
    let (header, stream) = split_dump(bytes).map_err(|error| error.to_string())?;
    let mut summary = Summary {
        abi_version: header.abi_version,
        dump_format_version: header.format_version,
        qpc_frequency: header.qpc_frequency,
        read_boundaries: header.format_version >= 2,
        reads: reads(&header, stream).count() as u64,
        ..Summary::default()
    };
    let mut at = 0;
    while at + size_of::<RecordHeader>() <= stream.len() {
        let kind = u16::from_le_bytes(stream[at..at + 2].try_into().expect("two bytes"));
        let len_at = at + offset_of!(RecordHeader, len);
        let len = u32::from_le_bytes(stream[len_at..len_at + 4].try_into().expect("four bytes"));
        let size = bio_abi::record_size(len as usize);
        if at + size > stream.len() {
            break;
        }
        match kind {
            RECORD_KIND_READ_END => summary.read_end_markers += 1,
            RECORD_KIND_RECORDER_GAP => {
                summary.recorder_gap_markers += 1;
                if len == 8 {
                    summary.recorder_lost_bytes =
                        summary
                            .recorder_lost_bytes
                            .saturating_add(u64::from_le_bytes(
                                stream[at + size_of::<RecordHeader>()
                                    ..at + size_of::<RecordHeader>() + 8]
                                    .try_into()
                                    .expect("eight bytes"),
                            ));
                }
            }
            _ => {}
        }
        at += size;
    }
    for record in records(stream) {
        let (kind, endpoint, data_bytes) = match record {
            Ok(Record::Data {
                endpoint, bytes, ..
            }) => {
                summary.data_bytes += bytes.len() as u64;
                ("data", Some(endpoint), bytes.len() as u64)
            }
            Ok(Record::FormatChange {
                endpoint,
                epoch,
                format,
                ..
            }) => {
                summary
                    .epochs
                    .insert(epoch_key(endpoint, epoch), format!("{format:?}"));
                ("format_change", Some(endpoint), 0)
            }
            Ok(Record::StreamStart { endpoint, .. }) => ("stream_start", Some(endpoint), 0),
            Ok(Record::StreamStop { endpoint, .. }) => ("stream_stop", Some(endpoint), 0),
            Ok(Record::Volume { endpoint, .. }) => ("volume", Some(endpoint), 0),
            Ok(Record::Overrun {
                endpoint,
                lost_bytes,
                ..
            }) => {
                summary.lost_bytes += lost_bytes;
                ("overrun", Some(endpoint), 0)
            }
            Err(error) => {
                summary.errors.push(error.to_string());
                ("invalid", None, 0)
            }
        };
        *summary.records.entry(kind).or_default() += 1;
        if let Some(endpoint) = endpoint {
            let endpoint = summary.endpoints.entry(endpoint).or_default();
            endpoint.records += 1;
            endpoint.data_bytes += data_bytes;
        }
    }
    Ok(summary)
}

#[allow(clippy::too_many_arguments)]
fn write_output_block(
    block: engine::BlockOut<'_>,
    output_path: &Path,
    output_kind: OutputKind,
    channel_mask: u32,
    writer: &mut Option<OutputWriter>,
    output_rate: &mut Option<u32>,
    report: &mut RenderReport,
    hash: &mut Sha256,
) -> Result<(), String> {
    if output_rate.is_some_and(|rate| rate != block.rate) {
        return Err(format!(
            "scene rate changed from {} Hz to {} Hz during the dump",
            output_rate.expect("rate was checked"),
            block.rate
        ));
    }
    if block.channels != report.output_channels {
        return Err(format!(
            "output channel count changed from {} to {} during the dump",
            report.output_channels, block.channels
        ));
    }
    if writer.is_none() {
        *output_rate = Some(block.rate);
        *writer = Some(OutputWriter::create(
            output_path,
            output_kind,
            block.channels,
            block.rate,
            channel_mask,
        )?);
    }
    let writer = writer.as_mut().expect("writer was just created");
    for frame in 0..block.frames {
        for channel in 0..block.channels {
            let sample = block.samples[channel * block.frames + frame];
            hash.update(sample.to_le_bytes());
            writer
                .write_sample(sample)
                .map_err(|error| format!("{}: {error}", output_path.display()))?;
        }
    }
    report.blocks += 1;
    report.frames_written += block.frames as u64;
    Ok(())
}

fn render_dump(
    bytes: &[u8],
    profile_path: &Path,
    output_path: &Path,
    endpoint: EndpointArg,
    arrival_slack_ms: f64,
    overrides: &[engine::DirectLayoutOverride],
) -> Result<RenderReport, String> {
    let (header, stream) = split_dump(bytes).map_err(|error| error.to_string())?;
    if !arrival_slack_ms.is_finite() || arrival_slack_ms < 0.0 {
        return Err("--arrival-slack-ms must be a finite non-negative number".into());
    }
    let arrival_slack_frames = arrival_slack_ms * f64::from(engine::MIX_RATE) / 1_000.0;
    if arrival_slack_frames > engine::MAX_ARRIVAL_SLACK_FRAMES as f64 {
        return Err(format!(
            "--arrival-slack-ms must not exceed {} ms",
            engine::MAX_ARRIVAL_SLACK_FRAMES as f64 * 1_000.0 / f64::from(engine::MIX_RATE)
        ));
    }
    let profile = load_output_profile(profile_path).map_err(|error| error.to_string())?;
    let mut direct = engine::DirectLayouts::default();
    for value in overrides {
        direct
            .set(value.channels, value.layout)
            .map_err(str::to_owned)?;
    }
    let config = ChainConfig {
        direct,
        endpoints: endpoint.filter(),
        arrival_slack_frames: arrival_slack_frames.round() as u64,
        ..ChainConfig::default()
    };
    let mut chain = Chain::with_qpc_frequency(config, header.qpc_frequency);
    let mut installed_shape = None;
    let mut writer = None;
    let mut output_rate = None;
    let output_kind = match &profile {
        OutputProfile::Headphones(_) => OutputKind::Headphones,
        OutputProfile::Speakers(_) => OutputKind::Speakers,
    };
    let mut report = RenderReport {
        read_boundaries: header.format_version >= 2,
        recorder_lost_bytes: reads(&header, stream).map(|chunk| chunk.lost_bytes).sum(),
        output_kind: output_kind.as_str(),
        direct_layouts: overrides
            .iter()
            .map(|value| {
                (
                    value.channels,
                    value
                        .layout
                        .speakers()
                        .iter()
                        .copied()
                        .map(engine::speaker_name)
                        .collect(),
                )
            })
            .collect(),
        ..RenderReport::default()
    };
    let mut output_channel_mask = 0;
    let mut hash = Sha256::new();

    // Metadata and the common BRIR shape are prepared before rendering. Parser
    // errors are reported by push_records below so malformed records appear once.
    for record in records(stream).filter_map(Result::ok) {
        if let Record::FormatChange {
            endpoint: record_endpoint,
            epoch,
            format,
            ..
        } = record
        {
            report
                .epochs
                .insert(epoch_key(record_endpoint, epoch), format!("{format:?}"));
            if endpoint.includes(record_endpoint)
                && let Some(shape) = scene_shape(&format)
                && installed_shape != Some(shape)
            {
                let prepared = match &profile {
                    OutputProfile::Headphones(profile) => {
                        let prepared = brir::prepare(profile, shape.0, partition_for(shape.1))
                            .map_err(|error| error.to_string())?;
                        Prepared::new(Arc::new(prepared), shape.1)
                            .map_err(|error| error.to_string())?
                    }
                    OutputProfile::Speakers(profile) => {
                        let prepared = speakers::prepare(profile, shape.0, partition_for(shape.1))
                            .map_err(|error| error.to_string())?;
                        Prepared::speakers(Arc::new(prepared), shape.1)
                            .map_err(|error| error.to_string())?
                    }
                };
                output_channel_mask = prepared.channel_mask();
                report.output_channels = prepared.channels();
                drop(chain.set_prepared(Box::new(prepared)));
                installed_shape = Some(shape);
            }
        }
    }

    let mut event_error = None;
    let mut handle = |event: ChainEvent<'_>| match event {
        ChainEvent::Block(block) => {
            if let Err(error) = write_output_block(
                block,
                output_path,
                output_kind,
                output_channel_mask,
                &mut writer,
                &mut output_rate,
                &mut report,
                &mut hash,
            ) {
                event_error = Some(error);
            }
        }
        ChainEvent::Error { error, .. } => report.errors.push(error.to_string()),
        ChainEvent::Volume {
            endpoint,
            gain,
            muted,
        } => {
            report
                .endpoint_volumes
                .insert(endpoint, EndpointVolume { gain, muted });
        }
        ChainEvent::NeedsBrir { .. } => report.needs_brir += 1,
        ChainEvent::OutputChannels { .. }
        | ChainEvent::FormatChanged { .. }
        | ChainEvent::Overrun { .. } => {}
    };
    for chunk in reads(&header, stream) {
        chain.push_records(chunk.bytes, &mut handle);
        if header.format_version >= 2
            && let Some(end_qpc) = chunk.end_qpc
        {
            chain.idle(end_qpc, &mut handle);
        }
    }
    drop(handle);
    if let Some(error) = event_error {
        return Err(error);
    }

    let mut finish_error = None;
    chain.finish(&mut |event| match event {
        ChainEvent::Block(block) => {
            if let Err(error) = write_output_block(
                block,
                output_path,
                output_kind,
                output_channel_mask,
                &mut writer,
                &mut output_rate,
                &mut report,
                &mut hash,
            ) {
                finish_error = Some(error);
            }
        }
        ChainEvent::Error { error, .. } => report.errors.push(error.to_string()),
        ChainEvent::Volume {
            endpoint,
            gain,
            muted,
        } => {
            report
                .endpoint_volumes
                .insert(endpoint, EndpointVolume { gain, muted });
        }
        ChainEvent::NeedsBrir { .. } => report.needs_brir += 1,
        ChainEvent::OutputChannels { .. }
        | ChainEvent::FormatChanged { .. }
        | ChainEvent::Overrun { .. } => {}
    });
    if let Some(error) = finish_error {
        return Err(error);
    }
    report.lane_underrun_frames = chain.lane_underrun_frames();
    report.dropped_frames = chain.dropped_frames();
    report.suppressed_errors = chain.suppressed_errors();

    let Some(writer) = writer else {
        return Err("the dump produced no audio blocks".into());
    };
    writer.finalize(output_path)?;
    report.output_sha256 = hash
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    Ok(report)
}

fn run() -> Result<(), String> {
    let cli = Cli::parse();
    match cli.command {
        Command::Inspect { dump } => {
            let bytes =
                std::fs::read(&dump).map_err(|error| format!("{}: {error}", dump.display()))?;
            let summary = inspect(&bytes)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&summary).expect("summary serializes")
            );
        }
        Command::Render {
            dump,
            profile,
            out,
            report,
            direct_layouts,
            endpoint,
            arrival_slack_ms,
        } => {
            let bytes =
                std::fs::read(&dump).map_err(|error| format!("{}: {error}", dump.display()))?;
            let rendered = render_dump(
                &bytes,
                &profile,
                &out,
                endpoint,
                arrival_slack_ms,
                &direct_layouts,
            )?;
            if let Some(report_path) = report {
                let json = serde_json::to_vec_pretty(&rendered).expect("render report serializes");
                std::fs::write(&report_path, json)
                    .map_err(|error| format!("{}: {error}", report_path.display()))?;
            }
            eprintln!(
                "bio-replay: wrote {} blocks, {} frames to {} (sha256 {})",
                rendered.blocks,
                rendered.frames_written,
                out.display(),
                rendered.output_sha256
            );
        }
    }
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("bio-replay: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use bio_abi::{
        ENDPOINT_PRIMARY, ENDPOINT_SECOND, RecordWriter, dump_header_bytes, wave_format_bytes,
    };
    use bio_types::{PcmFormat, SampleType, SourceFormat};

    use super::*;

    #[test]
    fn render_direct_layouts_are_repeatable_and_validate_before_startup() {
        let cli = Cli::try_parse_from([
            "bio-replay",
            "render",
            "capture.biodump",
            "--profile",
            "profile.toml",
            "--out",
            "output.wav",
            "--direct-layout",
            "2=L,R",
            "--direct-layout",
            "12=7.1.4",
        ])
        .unwrap();
        let Command::Render { direct_layouts, .. } = cli.command else {
            panic!("expected render command");
        };
        assert_eq!(direct_layouts.len(), 2);
        assert_eq!(direct_layouts[0].channels, 2);
        assert_eq!(direct_layouts[1].channels, 12);

        assert!(
            Cli::try_parse_from([
                "bio-replay",
                "render",
                "capture.biodump",
                "--profile",
                "profile.toml",
                "--out",
                "output.wav",
                "--direct-layout",
                "2=L,L",
            ])
            .is_err()
        );
    }

    #[test]
    fn endpoint_argument_filter_and_preparation_cover_the_same_four_slots() {
        for endpoint in 0..bio_abi::MAX_ENDPOINTS as u16 {
            assert!(EndpointArg::All.includes(endpoint));
        }
        assert!(!EndpointArg::All.includes(bio_abi::MAX_ENDPOINTS as u16));
        for (argument, endpoint) in [
            (EndpointArg::Primary, ENDPOINT_PRIMARY),
            (EndpointArg::Second, ENDPOINT_SECOND),
            (EndpointArg::Passthrough, 2),
            (EndpointArg::Reserved, 3),
        ] {
            assert!(argument.includes(endpoint));
            assert!(matches!(argument.filter(), EndpointFilter::Only(value) if value == endpoint));
        }
    }

    #[test]
    fn arrival_slack_above_two_hundred_ms_is_rejected_before_file_creation() {
        let temp = std::env::temp_dir().join(format!(
            "bio-replay-slack-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let profile = temp.join("profile.toml");
        let output = temp.join("out.wav");
        std::fs::create_dir_all(&temp).unwrap();
        std::fs::write(
            &profile,
            r#"
name = "slack-test"
gain_db = 0
limiter_ceiling_dbfs = 0

[brir]
kind = "sofa"
path = "missing.sofa"
"#,
        )
        .unwrap();
        let dump = dump_header_bytes(10_000_000);

        let error =
            render_dump(&dump, &profile, &output, EndpointArg::All, 200.001, &[]).unwrap_err();
        assert!(error.contains("must not exceed 200 ms"), "{error}");
        assert!(!output.exists());
        std::fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn inspect_counts_records_and_names_each_epoch() {
        let pcm = SourceFormat::Pcm(PcmFormat {
            channels: 2,
            sample_rate: 48_000,
            bits_per_sample: 16,
            valid_bits: 16,
            sample_type: SampleType::Int,
            channel_mask: 3,
        });
        let mut stream = vec![0; 1024];
        let mut writer = RecordWriter::new(&mut stream);
        assert!(writer.stream_start(1, 0));
        assert!(writer.format_change(1, 0, &wave_format_bytes(&pcm)));
        assert!(writer.data(1, 0, 1, &[0; 16]));
        writer.set_endpoint(ENDPOINT_SECOND);
        assert!(writer.data(3, 0, 1, &[0; 8]));
        assert!(writer.overrun(8, 2, 64));
        let length = writer.len();
        let mut file = dump_header_bytes(10_000_000);
        file.extend_from_slice(&stream[..length]);

        let summary = inspect(&file).unwrap();
        assert_eq!(summary.dump_format_version, bio_abi::DUMP_FORMAT_VERSION);
        assert_eq!(summary.records["data"], 2);
        assert_eq!(summary.records["format_change"], 1);
        assert_eq!((summary.data_bytes, summary.lost_bytes), (24, 64));
        assert!(summary.read_boundaries);
        assert_eq!(summary.reads, 1);
        assert_eq!(summary.recorder_lost_bytes, 0);
        assert!(summary.epochs["0:1"].contains("Pcm"));
        assert_eq!(summary.endpoints[&ENDPOINT_PRIMARY].records, 3);
        assert_eq!(summary.endpoints[&ENDPOINT_PRIMARY].data_bytes, 16);
        assert_eq!(summary.endpoints[&ENDPOINT_SECOND].records, 2);
        assert_eq!(summary.endpoints[&ENDPOINT_SECOND].data_bytes, 8);
        assert!(summary.errors.is_empty());
        assert!(inspect(b"not a dump").is_err());
    }
}

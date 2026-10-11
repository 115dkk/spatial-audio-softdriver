#![forbid(unsafe_code)]

//! bio-replay: reads a diagnostic recording (driver record dump) offline.
//!
//! `inspect` summarises a dump as JSON. `render` runs the dump through the same
//! decode, render and convolver chain as the live app and writes the binaural
//! WAV; it links no Windows code, so it runs in Linux CI.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use bio_abi::{Record, records, split_dump};
use clap::{Parser, Subcommand};
use engine::{Chain, ChainConfig, ChainEvent, Prepared, partition_for, scene_shape};
use serde::Serialize;
use sha2::{Digest, Sha256};

/// The release tag's version when release.yml builds, the crate version else.
const VERSION: &str = match option_env!("BIO_RELEASE_VERSION") {
    Some(v) => v,
    None => env!("CARGO_PKG_VERSION"),
};

#[derive(Parser)]
#[command(version = VERSION, about = "Offline reader for BIO Atmos driver record dumps")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Print a JSON summary of a dump: formats per epoch, record counts, losses.
    Inspect { dump: PathBuf },
    /// Render a diagnostic dump to a deterministic 32-bit float stereo WAV.
    Render {
        dump: PathBuf,
        #[arg(long)]
        profile: PathBuf,
        #[arg(long)]
        out: PathBuf,
        #[arg(long)]
        report: Option<PathBuf>,
    },
}

#[derive(Serialize, Default)]
struct Summary {
    abi_version: u32,
    qpc_frequency: u64,
    records: BTreeMap<&'static str, u64>,
    data_bytes: u64,
    lost_bytes: u64,
    epochs: BTreeMap<u32, String>,
    errors: Vec<String>,
}

#[derive(Debug, Serialize, Default)]
struct RenderReport {
    epochs: BTreeMap<u32, String>,
    blocks: u64,
    frames_written: u64,
    errors: Vec<String>,
    needs_brir: u64,
    output_sha256: String,
}

fn inspect(bytes: &[u8]) -> Result<Summary, String> {
    let (header, stream) = split_dump(bytes).map_err(|error| error.to_string())?;
    let mut summary = Summary {
        abi_version: header.abi_version,
        qpc_frequency: header.qpc_frequency,
        ..Summary::default()
    };
    for record in records(stream) {
        let kind = match record {
            Ok(Record::Data { bytes, .. }) => {
                summary.data_bytes += bytes.len() as u64;
                "data"
            }
            Ok(Record::FormatChange { epoch, format, .. }) => {
                summary.epochs.insert(epoch, format!("{format:?}"));
                "format_change"
            }
            Ok(Record::StreamStart { .. }) => "stream_start",
            Ok(Record::StreamStop { .. }) => "stream_stop",
            Ok(Record::Overrun { lost_bytes, .. }) => {
                summary.lost_bytes += lost_bytes;
                "overrun"
            }
            Err(error) => {
                summary.errors.push(error.to_string());
                "invalid"
            }
        };
        *summary.records.entry(kind).or_default() += 1;
    }
    Ok(summary)
}

fn render_dump(
    bytes: &[u8],
    profile_path: &Path,
    output_path: &Path,
) -> Result<RenderReport, String> {
    let (_, stream) = split_dump(bytes).map_err(|error| error.to_string())?;
    let profile = brir::load_profile(profile_path).map_err(|error| error.to_string())?;
    let config = ChainConfig::default();
    let mut chain = Chain::new(config);
    let mut installed_shape = None;
    let mut writer: Option<hound::WavWriter<std::io::BufWriter<std::fs::File>>> = None;
    let mut output_rate = None;
    let mut report = RenderReport::default();
    let mut hash = Sha256::new();

    for parsed in records(stream) {
        let record = match parsed {
            Ok(record) => record,
            Err(error) => {
                report.errors.push(error.to_string());
                continue;
            }
        };
        if let Record::FormatChange { epoch, format, .. } = record {
            report.epochs.insert(epoch, format!("{format:?}"));
            if let Some(shape) = scene_shape(&format)
                && installed_shape != Some(shape)
            {
                let prepared = brir::prepare(&profile, shape.0, partition_for(shape.1))
                    .map_err(|error| error.to_string())?;
                // The preferred 480-frame PCM partition does not divide the
                // chain's 1536-frame decoder capacity. Prepared only needs to
                // cover this format's emitted block, while Chain keeps the
                // larger decoder and output buffers.
                let prepared = Prepared::new(Arc::new(prepared), shape.1)
                    .map_err(|error| error.to_string())?;
                drop(chain.set_prepared(Box::new(prepared)));
                installed_shape = Some(shape);
            }
        }

        let mut event_error = None;
        chain.push_record(record, &mut |event| match event {
            ChainEvent::Block(block) => {
                if output_rate.is_some_and(|rate| rate != block.rate) {
                    event_error = Some(format!(
                        "scene rate changed from {} Hz to {} Hz during the dump",
                        output_rate.expect("rate was checked"),
                        block.rate
                    ));
                    return;
                }
                if writer.is_none() {
                    output_rate = Some(block.rate);
                    match hound::WavWriter::create(
                        output_path,
                        hound::WavSpec {
                            channels: 2,
                            sample_rate: block.rate,
                            bits_per_sample: 32,
                            sample_format: hound::SampleFormat::Float,
                        },
                    ) {
                        Ok(created) => writer = Some(created),
                        Err(error) => {
                            event_error = Some(format!("{}: {error}", output_path.display()));
                            return;
                        }
                    }
                }
                let Some(writer) = writer.as_mut() else {
                    return;
                };
                for (&left, &right) in block.left.iter().zip(block.right) {
                    for sample in [left, right] {
                        let bytes = sample.to_le_bytes();
                        hash.update(bytes);
                        if let Err(error) = writer.write_sample(sample) {
                            event_error = Some(format!("{}: {error}", output_path.display()));
                            return;
                        }
                    }
                }
                report.blocks += 1;
                report.frames_written += block.left.len() as u64;
            }
            ChainEvent::Error(error) => report.errors.push(error.to_string()),
            ChainEvent::NeedsBrir { .. } => report.needs_brir += 1,
            ChainEvent::FormatChanged { .. } | ChainEvent::Overrun { .. } => {}
        });
        if let Some(error) = event_error {
            return Err(error);
        }
    }

    let Some(writer) = writer else {
        return Err("the dump produced no audio blocks".into());
    };
    writer
        .finalize()
        .map_err(|error| format!("{}: {error}", output_path.display()))?;
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
        } => {
            let bytes =
                std::fs::read(&dump).map_err(|error| format!("{}: {error}", dump.display()))?;
            let rendered = render_dump(&bytes, &profile, &out)?;
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
    use bio_abi::{RecordWriter, dump_header_bytes, wave_format_bytes};
    use bio_types::{PcmFormat, SampleType, SourceFormat};

    use super::*;

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
        assert!(writer.overrun(16, 2, 64));
        let length = writer.len();
        let mut file = dump_header_bytes(10_000_000);
        file.extend_from_slice(&stream[..length]);

        let summary = inspect(&file).unwrap();
        assert_eq!(summary.records["data"], 1);
        assert_eq!(summary.records["format_change"], 1);
        assert_eq!((summary.data_bytes, summary.lost_bytes), (16, 64));
        assert!(summary.epochs[&1].contains("Pcm"));
        assert!(summary.errors.is_empty());
        assert!(inspect(b"not a dump").is_err());
    }
}

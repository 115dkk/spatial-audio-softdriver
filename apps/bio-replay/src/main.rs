#![forbid(unsafe_code)]

//! bio-replay: reads a diagnostic recording (driver record dump) offline.
//!
//! `inspect` summarises a dump as JSON. `render` (M1) runs the dump through the
//! same decode, render and convolver chain as the live app and writes the
//! binaural WAV; it links no Windows code, so it runs in Linux CI.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::ExitCode;

use bio_abi::{Record, records, split_dump};
use clap::{Parser, Subcommand};
use serde::Serialize;

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

fn inspect(bytes: &[u8]) -> Result<Summary, String> {
    let (header, stream) = split_dump(bytes).map_err(|e| e.to_string())?;
    let mut s = Summary {
        abi_version: header.abi_version,
        qpc_frequency: header.qpc_frequency,
        ..Summary::default()
    };
    for record in records(stream) {
        let kind = match record {
            Ok(Record::Data { bytes, .. }) => {
                s.data_bytes += bytes.len() as u64;
                "data"
            }
            Ok(Record::FormatChange { epoch, format, .. }) => {
                s.epochs.insert(epoch, format!("{format:?}"));
                "format_change"
            }
            Ok(Record::StreamStart { .. }) => "stream_start",
            Ok(Record::StreamStop { .. }) => "stream_stop",
            Ok(Record::Overrun { lost_bytes, .. }) => {
                s.lost_bytes += lost_bytes;
                "overrun"
            }
            Err(e) => {
                s.errors.push(e.to_string());
                "invalid"
            }
        };
        *s.records.entry(kind).or_default() += 1;
    }
    Ok(s)
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::Inspect { dump } => {
            let result = std::fs::read(&dump)
                .map_err(|e| format!("{}: {e}", dump.display()))
                .and_then(|bytes| inspect(&bytes));
            match result {
                Ok(summary) => {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&summary).expect("summary serializes")
                    );
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("bio-replay: {e}");
                    ExitCode::FAILURE
                }
            }
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
        let mut w = RecordWriter::new(&mut stream);
        assert!(w.stream_start(1, 0));
        assert!(w.format_change(1, 0, &wave_format_bytes(&pcm)));
        assert!(w.data(1, 0, 1, &[0; 16]));
        assert!(w.overrun(16, 2, 64));
        let n = w.len();
        let mut file = dump_header_bytes(10_000_000);
        file.extend_from_slice(&stream[..n]);

        let s = inspect(&file).unwrap();
        assert_eq!(s.records["data"], 1);
        assert_eq!(s.records["format_change"], 1);
        assert_eq!((s.data_bytes, s.lost_bytes), (16, 64));
        assert!(s.epochs[&1].contains("Pcm"));
        assert!(s.errors.is_empty());
        assert!(inspect(b"not a dump").is_err());
    }
}

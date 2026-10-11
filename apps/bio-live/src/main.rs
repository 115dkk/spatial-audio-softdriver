#![forbid(unsafe_code)]

//! bio-live: development tool for the SpatialAudioLab test PC (ADR 0018).
//!
//! * `record` writes the SAL ring's record stream to a dump file, the input
//!   the MAT decoder and the black-box goldens are built from (ADR 0005).
//! * `devices` lists render endpoints.
//!
//! The SAL device opens only from an administrator prompt.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(version, about = "BIO Atmos live tool for the SAL development PC")]
struct Cli {
    #[command(subcommand)]
    command: Command,
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
    /// List render endpoints (name, default flag, id).
    Devices,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Record { out, seconds } => platform::record(&out, seconds),
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
    use std::fs::File;
    use std::io::{BufWriter, Write};
    use std::path::Path;
    use std::time::{Duration, Instant};

    use bio_abi::{Record, dump_header_bytes, records};
    use bio_sys::{devices, qpc, sal::SalRing};
    use bio_types::{CaptureError, CaptureSource};

    pub fn record(out: &Path, seconds: f64) -> Result<(), String> {
        let mut ring = SalRing::open().map_err(|e| match e {
            CaptureError::NotPresent => {
                "the SAL capture device is not present (is the SAL test-signed driver installed?)"
                    .to_owned()
            }
            CaptureError::AccessDenied => {
                "access to the SAL capture device was denied; run from an administrator prompt"
                    .to_owned()
            }
            other => other.to_string(),
        })?;
        let file = File::create(out).map_err(|e| format!("{}: {e}", out.display()))?;
        let mut writer = BufWriter::with_capacity(1 << 20, file);
        writer
            .write_all(&dump_header_bytes(qpc::frequency()))
            .map_err(|e| e.to_string())?;

        let mut buf = vec![0_u8; 1 << 20];
        let (mut bytes, mut data, mut formats, mut lost) = (0_u64, 0_u64, 0_u64, 0_u64);
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
                    match record {
                        Record::Data { bytes: payload, .. } => data += payload.len() as u64,
                        Record::FormatChange { epoch, format, .. } => {
                            formats += 1;
                            eprintln!("epoch {epoch}: {format:?}");
                        }
                        Record::Overrun { lost_bytes, .. } => lost += lost_bytes,
                        _ => {}
                    }
                }
            }
            if started.elapsed() >= next_report {
                eprintln!(
                    "{:>4} s  {data} payload bytes, {formats} format changes, {lost} bytes lost",
                    next_report.as_secs()
                );
                next_report += Duration::from_secs(1);
            }
        }
        writer.flush().map_err(|e| e.to_string())?;
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

    pub fn devices() -> Result<(), String> {
        for endpoint in devices::render_endpoints().map_err(|e| e.to_string())? {
            let mark = if endpoint.is_default { "*" } else { " " };
            println!("{mark} {}  [{}]", endpoint.name, endpoint.id);
        }
        Ok(())
    }
}

#[cfg(not(windows))]
mod platform {
    use std::path::Path;

    pub fn record(_out: &Path, _seconds: f64) -> Result<(), String> {
        Err("bio-live runs on Windows only".to_owned())
    }

    pub fn devices() -> Result<(), String> {
        Err("bio-live runs on Windows only".to_owned())
    }
}

#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use bio_abi::{ENDPOINT_SECOND, RecordWriter, dump_header_bytes, wave_format_bytes};
use bio_types::{PcmFormat, SampleType, SourceFormat};
use serde_json::Value;

struct TempDir(PathBuf);

static NEXT_TEMP_DIR: AtomicU64 = AtomicU64::new(0);

impl TempDir {
    fn new() -> Self {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock follows Unix epoch")
            .as_nanos();
        let sequence = NEXT_TEMP_DIR.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "bio-replay-render-{}-{unique}-{sequence}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn write_ir(path: &Path) {
    let mut writer = hound::WavWriter::create(
        path,
        hound::WavSpec {
            channels: 2,
            sample_rate: 48_000,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        },
    )
    .unwrap();
    for frame in 0..16 {
        writer
            .write_sample(if frame == 0 { 1.0_f32 } else { 0.0 })
            .unwrap();
        writer
            .write_sample(if frame == 1 { 0.5_f32 } else { 0.0 })
            .unwrap();
    }
    writer.finalize().unwrap();
}

fn write_dump(path: &Path) {
    let format = SourceFormat::Pcm(PcmFormat {
        channels: 2,
        sample_rate: 48_000,
        bits_per_sample: 32,
        valid_bits: 32,
        sample_type: SampleType::Float,
        channel_mask: 3,
    });
    let mut pcm = vec![0_u8; 480 * 2 * 4];
    for (frame, samples) in pcm.as_chunks_mut::<8>().0.iter_mut().enumerate() {
        let left = (frame as f32 * 0.01).sin() * 0.1;
        let right = (frame as f32 * 0.013).cos() * 0.1;
        samples[..4].copy_from_slice(&left.to_le_bytes());
        samples[4..].copy_from_slice(&right.to_le_bytes());
    }
    let mut records = vec![0; bio_abi::record_size(52) + bio_abi::record_size(pcm.len()) + 64];
    let mut writer = RecordWriter::new(&mut records);
    assert!(writer.format_change(5, 1, &wave_format_bytes(&format)));
    assert!(writer.data(5, 0, 2, &pcm));
    let length = writer.len();
    let mut dump = dump_header_bytes(10_000_000);
    dump.extend_from_slice(&records[..length]);
    std::fs::write(path, dump).unwrap();
}

fn write_mixed_dump(path: &Path) {
    let format = SourceFormat::Pcm(PcmFormat {
        channels: 2,
        sample_rate: 48_000,
        bits_per_sample: 32,
        valid_bits: 32,
        sample_type: SampleType::Float,
        channel_mask: 3,
    });
    let mut primary = vec![0_u8; 480 * 2 * 4];
    for (frame, samples) in primary.as_chunks_mut::<8>().0.iter_mut().enumerate() {
        let left = (frame as f32 * 0.01).sin() * 0.1;
        let right = (frame as f32 * 0.013).cos() * 0.1;
        samples[..4].copy_from_slice(&left.to_le_bytes());
        samples[4..].copy_from_slice(&right.to_le_bytes());
    }
    let second = vec![1_u8; 480 * 2 * 4];
    let mut records = vec![0; 16_000];
    let mut writer = RecordWriter::new(&mut records);
    assert!(writer.format_change(5, 1, &wave_format_bytes(&format)));
    assert!(writer.data(5, 0, 2, &primary));
    writer.set_endpoint(ENDPOINT_SECOND);
    assert!(writer.format_change(9, 3, &wave_format_bytes(&format)));
    assert!(writer.data(9, 0, 4, &second));
    let length = writer.len();
    let mut dump = dump_header_bytes(10_000_000);
    dump.extend_from_slice(&records[..length]);
    std::fs::write(path, dump).unwrap();
}

fn write_profile(path: &Path) {
    std::fs::write(
        path,
        r#"name = "integration"
gain_db = 0.0
limiter_ceiling_dbfs = 0.0

[brir]
directions = [
  { name = "FL", azimuth = 30.0, elevation = 0.0, file = "FL.wav" },
  { name = "FR", azimuth = -30.0, elevation = 0.0, file = "FR.wav" },
]
"#,
    )
    .unwrap();
}

#[test]
fn render_is_deterministic_and_inspect_still_works() {
    let temp = TempDir::new();
    write_ir(&temp.0.join("FL.wav"));
    write_ir(&temp.0.join("FR.wav"));
    let dump = temp.0.join("input.dump");
    let profile = temp.0.join("profile.toml");
    let out_a = temp.0.join("a.wav");
    let out_b = temp.0.join("b.wav");
    let report_a = temp.0.join("a.json");
    let report_b = temp.0.join("b.json");
    write_dump(&dump);
    write_profile(&profile);

    for (out, report) in [(&out_a, &report_a), (&out_b, &report_b)] {
        let result = Command::new(env!("CARGO_BIN_EXE_bio-replay"))
            .args(["render"])
            .arg(&dump)
            .args(["--profile"])
            .arg(&profile)
            .args(["--out"])
            .arg(out)
            .args(["--report"])
            .arg(report)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "render failed: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(String::from_utf8_lossy(&result.stderr).contains("480 frames"));
    }

    assert_eq!(
        std::fs::read(&out_a).unwrap(),
        std::fs::read(&out_b).unwrap()
    );
    let first: Value = serde_json::from_slice(&std::fs::read(&report_a).unwrap()).unwrap();
    let second: Value = serde_json::from_slice(&std::fs::read(&report_b).unwrap()).unwrap();
    assert_eq!(first["output_sha256"], second["output_sha256"]);
    assert_eq!(first["blocks"], 1);
    assert_eq!(first["frames_written"], 480);
    assert!(first["epochs"]["0:5"].as_str().unwrap().contains("Pcm"));

    let inspected = Command::new(env!("CARGO_BIN_EXE_bio-replay"))
        .arg("inspect")
        .arg(&dump)
        .output()
        .unwrap();
    assert!(inspected.status.success());
    let summary: Value = serde_json::from_slice(&inspected.stdout).unwrap();
    assert!(summary["epochs"]["0:5"].as_str().unwrap().contains("Pcm"));
}

#[test]
fn default_render_ignores_second_endpoint_records() {
    let temp = TempDir::new();
    write_ir(&temp.0.join("FL.wav"));
    write_ir(&temp.0.join("FR.wav"));
    let mixed = temp.0.join("mixed.dump");
    let primary = temp.0.join("primary.dump");
    let profile = temp.0.join("profile.toml");
    write_mixed_dump(&mixed);
    write_dump(&primary);
    write_profile(&profile);

    let mut hashes = Vec::new();
    for (dump, stem) in [(&mixed, "mixed"), (&primary, "primary")] {
        let report = temp.0.join(format!("{stem}.json"));
        let result = Command::new(env!("CARGO_BIN_EXE_bio-replay"))
            .arg("render")
            .arg(dump)
            .args(["--profile"])
            .arg(&profile)
            .args(["--out"])
            .arg(temp.0.join(format!("{stem}.wav")))
            .args(["--report"])
            .arg(&report)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let report: Value = serde_json::from_slice(&std::fs::read(report).unwrap()).unwrap();
        hashes.push(report["output_sha256"].as_str().unwrap().to_owned());
    }
    assert_eq!(hashes[0], hashes[1]);

    let second = Command::new(env!("CARGO_BIN_EXE_bio-replay"))
        .arg("render")
        .arg(&mixed)
        .args(["--profile"])
        .arg(&profile)
        .args(["--out"])
        .arg(temp.0.join("second.wav"))
        .args(["--endpoint", &ENDPOINT_SECOND.to_string()])
        .output()
        .unwrap();
    assert!(
        second.status.success(),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
}

#[test]
fn missing_profile_fails_with_a_readable_message() {
    let temp = TempDir::new();
    let dump = temp.0.join("input.dump");
    write_dump(&dump);
    let missing = temp.0.join("missing.toml");
    let result = Command::new(env!("CARGO_BIN_EXE_bio-replay"))
        .arg("render")
        .arg(&dump)
        .args(["--profile"])
        .arg(&missing)
        .args(["--out"])
        .arg(temp.0.join("out.wav"))
        .output()
        .unwrap();
    assert!(!result.status.success());
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(stderr.contains("missing.toml"), "{stderr}");
    assert!(stderr.contains("bio-replay:"), "{stderr}");
}

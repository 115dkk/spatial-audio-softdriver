#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use brir::{AnglePreset, BrirSource, DirectionFile, LfePolicy, Profile};
use engine::{Chain, ChainConfig, ChainEvent, OutputProfile, Prepared};
use serde_json::Value;
use ssr_abi::{RecordWriter, dump_header, dump_header_bytes, reads, wave_format_bytes};
use ssr_types::{PcmFormat, SampleType, SourceFormat};

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
            "ssr-replay-render-{}-{unique}-{sequence}",
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

fn write_timed_dump(path: &Path, qpc_frequency: u64, second_qpc: u64) {
    let format = SourceFormat::Pcm(PcmFormat {
        channels: 2,
        sample_rate: 48_000,
        bits_per_sample: 32,
        valid_bits: 32,
        sample_type: SampleType::Float,
        channel_mask: 3,
    });
    let mut primary = vec![0_u8; 480 * 2 * 4];
    let mut second = vec![0_u8; 5 * 480 * 2 * 4];
    for samples in primary.as_chunks_mut::<8>().0 {
        samples[..4].copy_from_slice(&0.1_f32.to_le_bytes());
        samples[4..].copy_from_slice(&0.1_f32.to_le_bytes());
    }
    for samples in second.as_chunks_mut::<8>().0 {
        samples[..4].copy_from_slice(&0.2_f32.to_le_bytes());
        samples[4..].copy_from_slice(&0.2_f32.to_le_bytes());
    }
    let mut records = vec![0; 32_000];
    let mut writer = RecordWriter::new(&mut records);
    assert!(writer.format_change(1, 0, &wave_format_bytes(&format)));
    assert!(writer.data(1, 0, 0, &primary));
    writer.set_endpoint(1);
    assert!(writer.format_change(2, 0, &wave_format_bytes(&format)));
    for (index, block) in second.as_chunks::<{ 480 * 2 * 4 }>().0.iter().enumerate() {
        assert!(writer.data(
            2,
            (index * block.len()) as u64,
            second_qpc * index as u64,
            block
        ));
    }
    let length = writer.len();
    let mut dump = dump_header_bytes(qpc_frequency);
    dump.extend_from_slice(&records[..length]);
    std::fs::write(path, dump).unwrap();
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
    let mut records = vec![0; ssr_abi::record_size(52) + ssr_abi::record_size(pcm.len()) + 64];
    let mut writer = RecordWriter::new(&mut records);
    assert!(writer.format_change(5, 1, &wave_format_bytes(&format)));
    assert!(writer.data(5, 0, 2, &pcm));
    let length = writer.len();
    let mut dump = dump_header_bytes(10_000_000);
    dump.extend_from_slice(&records[..length]);
    std::fs::write(path, dump).unwrap();
}

fn write_primary_only_dump(path: &Path) {
    let format = SourceFormat::Pcm(PcmFormat {
        channels: 2,
        sample_rate: 48_000,
        bits_per_sample: 32,
        valid_bits: 32,
        sample_type: SampleType::Float,
        channel_mask: 3,
    });
    let mut primary = vec![0_u8; 960 * 2 * 4];
    for (frame, samples) in primary.as_chunks_mut::<8>().0.iter_mut().enumerate() {
        let left = (frame as f32 * 0.01).sin() * 0.1;
        let right = (frame as f32 * 0.013).cos() * 0.1;
        samples[..4].copy_from_slice(&left.to_le_bytes());
        samples[4..].copy_from_slice(&right.to_le_bytes());
    }
    let mut records = vec![0; 12_000];
    let mut writer = RecordWriter::new(&mut records);
    assert!(writer.format_change(5, 1, &wave_format_bytes(&format)));
    let split = primary.len() / 2;
    // QPC is each record's last-byte DMA time at 10 MHz: 480 frames per 10 ms.
    assert!(writer.data(5, 0, 100_000, &primary[..split]));
    assert!(writer.data(5, split as u64, 200_000, &primary[split..]));
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
    let mut primary = vec![0_u8; 960 * 2 * 4];
    for (frame, samples) in primary.as_chunks_mut::<8>().0.iter_mut().enumerate() {
        let left = (frame as f32 * 0.01).sin() * 0.1;
        let right = (frame as f32 * 0.013).cos() * 0.1;
        samples[..4].copy_from_slice(&left.to_le_bytes());
        samples[4..].copy_from_slice(&right.to_le_bytes());
    }
    let mut second = vec![0_u8; 960 * 2 * 4];
    for (frame, samples) in second.as_chunks_mut::<8>().0.iter_mut().enumerate() {
        let left = (frame as f32 * 0.01).sin() * 0.1;
        let right = (frame as f32 * 0.013).cos() * 0.1;
        samples[..4].copy_from_slice(&left.to_le_bytes());
        samples[4..].copy_from_slice(&right.to_le_bytes());
    }
    let mut records = vec![0; 20_000];
    let mut writer = RecordWriter::new(&mut records);
    assert!(writer.format_change(5, 1, &wave_format_bytes(&format)));
    let split = primary.len() / 2;
    // The second endpoint's 20 ms share A's DMA time and arrive between A's two
    // 10 ms records: A's first record, B's record, then A's second.
    assert!(writer.data(5, 0, 100_000, &primary[..split]));
    writer.set_endpoint(1);
    assert!(writer.format_change(9, 3, &wave_format_bytes(&format)));
    assert!(writer.data(9, 0, 200_000, &second));
    writer.set_endpoint(0);
    assert!(writer.data(5, split as u64, 200_000, &primary[split..]));
    let length = writer.len();
    let mut dump = dump_header_bytes(10_000_000);
    dump.extend_from_slice(&records[..length]);
    std::fs::write(path, dump).unwrap();
}

/// A late DPC drains A's whole 100 ms as one record before B's ten 10 ms
/// records for the same DMA interval.
fn write_large_and_small_records_dump(path: &Path) {
    let format = SourceFormat::Pcm(PcmFormat {
        channels: 2,
        sample_rate: 48_000,
        bits_per_sample: 32,
        valid_bits: 32,
        sample_type: SampleType::Float,
        channel_mask: 3,
    });
    let stereo = |frames: usize, step: f32| {
        let mut bytes = vec![0_u8; frames * 2 * 4];
        for (frame, samples) in bytes.as_chunks_mut::<8>().0.iter_mut().enumerate() {
            let left = (frame as f32 * step).sin() * 0.1;
            let right = (frame as f32 * step * 1.3).cos() * 0.1;
            samples[..4].copy_from_slice(&left.to_le_bytes());
            samples[4..].copy_from_slice(&right.to_le_bytes());
        }
        bytes
    };
    let primary = stereo(4_800, 0.01);
    let second = stereo(4_800, 0.023);
    let mut records = vec![0; 100_000];
    let mut writer = RecordWriter::new(&mut records);
    assert!(writer.format_change(5, 1, &wave_format_bytes(&format)));
    writer.set_endpoint(1);
    assert!(writer.format_change(9, 3, &wave_format_bytes(&format)));
    writer.set_endpoint(0);
    assert!(writer.data(5, 0, 1_000_000, &primary));
    writer.set_endpoint(1);
    for (index, chunk) in second.chunks(480 * 8).enumerate() {
        let pos = (index * 480 * 8) as u64;
        assert!(writer.data(9, pos, (index as u64 + 1) * 100_000, chunk));
    }
    let length = writer.len();
    let mut dump = dump_header_bytes(10_000_000);
    dump.extend_from_slice(&records[..length]);
    std::fs::write(path, dump).unwrap();
}

fn wav_samples(path: &Path) -> Vec<f32> {
    hound::WavReader::open(path)
        .unwrap()
        .samples::<f32>()
        .map(Result::unwrap)
        .collect()
}

fn wav_rms(path: &Path) -> f64 {
    let mut reader = hound::WavReader::open(path).unwrap();
    let samples = reader
        .samples::<f32>()
        .map(Result::unwrap)
        .collect::<Vec<_>>();
    let energy = samples
        .iter()
        .map(|sample| f64::from(*sample) * f64::from(*sample))
        .sum::<f64>();
    (energy / samples.len() as f64).sqrt()
}

fn prepared_profile(directory: &Path) -> Box<Prepared> {
    let fl = directory.join("FL.wav");
    let fr = directory.join("FR.wav");
    write_ir(&fl);
    write_ir(&fr);
    let profile = Profile {
        name: "read-boundary".into(),
        brir: BrirSource::Directions(vec![
            DirectionFile {
                name: "FL".into(),
                azimuth_deg: 30.0,
                elevation_deg: 0.0,
                path: fl,
            },
            DirectionFile {
                name: "FR".into(),
                azimuth_deg: -30.0,
                elevation_deg: 0.0,
                path: fr,
            },
        ]),
        gain_db: 0.0,
        limiter_ceiling_dbfs: 0.0,
        lfe: LfePolicy::Mute,
        angles: AnglePreset::Standard,
        angle_overrides: Vec::new(),
        truncate: None,
        trim_leading_delay: false,
    };
    Box::new(OutputProfile::Headphones(profile).prepare().unwrap())
}

fn render_reads(directory: &Path, stream: &[u8], boundaries: Option<&[(usize, u64)]>) -> Vec<f32> {
    let mut chain = Chain::with_qpc_frequency(
        ChainConfig {
            arrival_slack_frames: 0,
            ..ChainConfig::default()
        },
        1_000_000,
    );
    chain.set_prepared(prepared_profile(directory));
    let mut output = Vec::new();
    let mut collect = |event: ChainEvent<'_>| {
        if let ChainEvent::Block(block) = event {
            for (&left, &right) in block.channel(0).iter().zip(block.channel(1)) {
                output.extend_from_slice(&[left, right]);
            }
        }
    };
    match boundaries {
        Some(reads) => {
            let mut start = 0;
            for &(end, end_qpc) in reads {
                chain.push_records(&stream[start..end], &mut collect);
                chain.idle(end_qpc, &mut collect);
                start = end;
            }
        }
        None => chain.push_records(stream, &mut collect),
    }
    chain.finish(&mut collect);
    output
}

fn write_speaker_profile(path: &Path, channel_mask: &str) {
    std::fs::write(
        path,
        format!(
            r#"kind = "speakers"
name = "integration speakers"
device_channels = 6
channel_mask = "{channel_mask}"
gain_db = 0.0
limiter_ceiling_dbfs = 0.0
crossover_hz = 80.0

[[speaker]]
name = "L"
channel = 0
range = "small"

[[speaker]]
name = "R"
channel = 1
range = "small"

[[speaker]]
name = "C"
channel = 2
range = "small"

[[speaker]]
name = "Ls"
channel = 4
range = "small"

[[speaker]]
name = "Rs"
channel = 5
range = "small"

[[subwoofer]]
channel = 3
"#
        ),
    )
    .unwrap();
}

fn wave_extensible_fields(path: &Path) -> (u16, u16, u32) {
    let bytes = std::fs::read(path).unwrap();
    assert_eq!(&bytes[..4], b"RIFF");
    assert_eq!(&bytes[8..16], b"WAVEfmt ");
    assert_eq!(u32::from_le_bytes(bytes[16..20].try_into().unwrap()), 40);
    (
        u16::from_le_bytes(bytes[20..22].try_into().unwrap()),
        u16::from_le_bytes(bytes[22..24].try_into().unwrap()),
        u32::from_le_bytes(bytes[40..44].try_into().unwrap()),
    )
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
        let result = Command::new(env!("CARGO_BIN_EXE_ssr-replay"))
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
        assert!(String::from_utf8_lossy(&result.stderr).contains("frames"));
    }

    assert_eq!(
        std::fs::read(&out_a).unwrap(),
        std::fs::read(&out_b).unwrap()
    );
    let first: Value = serde_json::from_slice(&std::fs::read(&report_a).unwrap()).unwrap();
    let second: Value = serde_json::from_slice(&std::fs::read(&report_b).unwrap()).unwrap();
    assert_eq!(first["output_sha256"], second["output_sha256"]);
    let blocks = first["blocks"].as_u64().unwrap();
    let frames = first["frames_written"].as_u64().unwrap();
    assert_eq!(frames, blocks * 480);
    assert!(frames > 480, "output tail was not drained");
    assert_eq!(first["lane_underrun_frames"], 0);
    assert_eq!(first["dropped_frames"], 0);
    assert!(first["epochs"]["0:5"].as_str().unwrap().contains("Pcm"));

    let inspected = Command::new(env!("CARGO_BIN_EXE_ssr-replay"))
        .arg("inspect")
        .arg(&dump)
        .output()
        .unwrap();
    assert!(inspected.status.success());
    let summary: Value = serde_json::from_slice(&inspected.stdout).unwrap();
    assert!(summary["epochs"]["0:5"].as_str().unwrap().contains("Pcm"));
}

#[test]
fn speaker_render_writes_extensible_channels_mask_and_report() {
    let temp = TempDir::new();
    let dump = temp.0.join("input.dump");
    let profile = temp.0.join("speakers.toml");
    let output = temp.0.join("speakers.wav");
    let report = temp.0.join("speakers.json");
    write_dump(&dump);
    write_speaker_profile(&profile, "5.1");

    let result = Command::new(env!("CARGO_BIN_EXE_ssr-replay"))
        .arg("render")
        .arg(&dump)
        .args(["--profile"])
        .arg(&profile)
        .args(["--out"])
        .arg(&output)
        .args(["--report"])
        .arg(&report)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(wave_extensible_fields(&output), (0xfffe, 6, 0x003f));
    let report: Value = serde_json::from_slice(&std::fs::read(report).unwrap()).unwrap();
    assert_eq!(report["output_kind"], "speakers");
    assert_eq!(report["output_channels"], 6);
    assert!(
        report["frames_written"].as_u64().unwrap() > 480,
        "speaker crossover tail was not drained"
    );
}

#[test]
fn direct_speaker_render_writes_zero_channel_mask() {
    let temp = TempDir::new();
    let dump = temp.0.join("input.dump");
    let profile = temp.0.join("speakers.toml");
    let output = temp.0.join("direct.wav");
    write_dump(&dump);
    write_speaker_profile(&profile, "direct");

    let result = Command::new(env!("CARGO_BIN_EXE_ssr-replay"))
        .arg("render")
        .arg(&dump)
        .args(["--profile"])
        .arg(&profile)
        .args(["--out"])
        .arg(&output)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(wave_extensible_fields(&output), (0xfffe, 6, 0));
}

#[test]
fn endpoint_all_mixes_both_and_endpoint_zero_matches_primary_only() {
    let temp = TempDir::new();
    write_ir(&temp.0.join("FL.wav"));
    write_ir(&temp.0.join("FR.wav"));
    let mixed = temp.0.join("mixed.dump");
    let primary_only = temp.0.join("primary.dump");
    let profile = temp.0.join("profile.toml");
    write_mixed_dump(&mixed);
    write_primary_only_dump(&primary_only);
    write_profile(&profile);

    let mut rms = Vec::new();
    let mut samples = Vec::new();
    let mut rendered = Vec::new();
    for (dump, stem, endpoint) in [
        (&mixed, "all", "all"),
        (&mixed, "mixed-primary", "0"),
        (&mixed, "second", "1"),
        (&primary_only, "primary", "0"),
    ] {
        let report = temp.0.join(format!("{stem}.json"));
        let output = temp.0.join(format!("{stem}.wav"));
        let result = Command::new(env!("CARGO_BIN_EXE_ssr-replay"))
            .arg("render")
            .arg(dump)
            .args(["--profile"])
            .arg(&profile)
            .args(["--out"])
            .arg(&output)
            .args(["--report"])
            .arg(&report)
            .args(["--endpoint", endpoint])
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let report: Value = serde_json::from_slice(&std::fs::read(report).unwrap()).unwrap();
        assert!(report["output_sha256"].as_str().is_some());
        rms.push(wav_rms(&output));
        samples.push(wav_samples(&output));
        rendered.push(std::fs::read(&output).unwrap());
    }
    assert_eq!(rendered[1], rendered[3]);
    assert_eq!(rms[1], rms[3]);
    assert!(rms[0] > rms[1], "all {} <= primary {}", rms[0], rms[1]);
    assert!(rms[0] > rms[2], "all {} <= second {}", rms[0], rms[2]);
    // Below the limiter ceiling the mix is the sum of the two solo renders,
    // sample for sample: B overlaps A instead of following A. Both endpoints
    // span the same 20 ms, and finish appends the same deterministic BRIR tail.
    let (all, primary, second) = (&samples[0], &samples[1], &samples[2]);
    assert!(all.len() > 2 * 960, "BRIR tail was not appended");
    assert_eq!(primary.len(), all.len());
    assert_eq!(second.len(), all.len());
    for (index, mixed) in all.iter().enumerate() {
        let sum = primary[index] + second[index];
        assert!(
            (mixed - sum).abs() <= 2.0e-6,
            "sample {index}: {mixed} != {sum}"
        );
    }
}

#[test]
fn replay_merges_a_large_record_with_small_records_by_dma_time() {
    let temp = TempDir::new();
    write_ir(&temp.0.join("FL.wav"));
    write_ir(&temp.0.join("FR.wav"));
    let dump = temp.0.join("large-small.dump");
    let profile = temp.0.join("profile.toml");
    write_large_and_small_records_dump(&dump);
    write_profile(&profile);

    let mut samples = Vec::new();
    for endpoint in ["all", "0", "1"] {
        let output = temp.0.join(format!("{endpoint}.wav"));
        let report = temp.0.join(format!("{endpoint}.json"));
        let result = Command::new(env!("CARGO_BIN_EXE_ssr-replay"))
            .arg("render")
            .arg(&dump)
            .args(["--profile"])
            .arg(&profile)
            .args(["--out"])
            .arg(&output)
            .args(["--report"])
            .arg(&report)
            .args(["--endpoint", endpoint])
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let report: Value = serde_json::from_slice(&std::fs::read(report).unwrap()).unwrap();
        assert_eq!(report["dropped_frames"], 0, "{endpoint}");
        assert_eq!(report["lane_underrun_frames"], 0, "{endpoint}");
        samples.push(wav_samples(&output));
    }
    // Both endpoints cover the same 100 ms of DMA time. Replay must merge the
    // records by that time, as the live READ path does, so B overlaps A rather
    // than starting after A's whole record.
    let (all, primary, second) = (&samples[0], &samples[1], &samples[2]);
    assert!(all.len() > 2 * 4_800, "BRIR tail was not appended");
    assert_eq!(primary.len(), all.len());
    assert_eq!(second.len(), all.len());
    for (index, mixed) in all.iter().enumerate() {
        let sum = primary[index] + second[index];
        assert!(
            (mixed - sum).abs() <= 2.0e-6,
            "sample {index}: {mixed} != {sum}"
        );
    }
}

#[test]
fn replay_feeds_each_recorded_read_as_one_read() {
    let temp = TempDir::new();
    let format = SourceFormat::Pcm(PcmFormat {
        channels: 1,
        sample_rate: 48_000,
        bits_per_sample: 32,
        valid_bits: 32,
        sample_type: SampleType::Float,
        channel_mask: 0x1,
    });
    let mut stream = vec![0; 64_000];
    let mut writer = RecordWriter::new(&mut stream);
    writer.set_endpoint(0);
    assert!(writer.format_change(1, 0, &wave_format_bytes(&format)));
    assert!(writer.data(1, 0, 100_000, &0.125_f32.to_le_bytes().repeat(4_800)));
    assert!(writer.stream_stop(1, 110_000));
    let first_end = writer.len();
    writer.set_endpoint(1);
    assert!(writer.format_change(2, 110_000, &wave_format_bytes(&format)));
    assert!(writer.stream_start(2, 120_000));
    assert!(writer.data(2, 0, 130_000, &0.25_f32.to_le_bytes().repeat(480)));
    let second_end = writer.len();
    stream.truncate(second_end);

    let direct = render_reads(
        &temp.0,
        &stream,
        Some(&[(first_end, 111_000), (second_end, 131_000)]),
    );

    let mut marked = Vec::new();
    marked.extend_from_slice(&stream[..first_end]);
    let mut marker = [0; 64];
    let mut marker_writer = RecordWriter::new(&mut marker);
    assert!(marker_writer.read_end(111_000));
    let marker_len = marker_writer.len();
    marked.extend_from_slice(&marker[..marker_len]);
    marked.extend_from_slice(&stream[first_end..second_end]);
    let mut marker_writer = RecordWriter::new(&mut marker);
    assert!(marker_writer.read_end(131_000));
    let marker_len = marker_writer.len();
    marked.extend_from_slice(&marker[..marker_len]);
    let header = dump_header(1_000_000);
    let format_two_ends = reads(&header, &marked)
        .scan(0, |offset, chunk| {
            *offset += chunk.bytes.len();
            Some((*offset, chunk.end_qpc.unwrap()))
        })
        .collect::<Vec<_>>();
    let format_two_stream = reads(&header, &marked)
        .flat_map(|chunk| chunk.bytes.iter().copied())
        .collect::<Vec<_>>();
    let replayed = render_reads(&temp.0, &format_two_stream, Some(&format_two_ends));
    assert_eq!(replayed, direct);

    let format_two_dump = temp.0.join("format-two.dump");
    let profile = temp.0.join("profile.toml");
    let replay_wav = temp.0.join("replay.wav");
    let mut file = dump_header_bytes(1_000_000);
    file.extend_from_slice(&marked);
    std::fs::write(&format_two_dump, file).unwrap();
    write_profile(&profile);
    let result = Command::new(env!("CARGO_BIN_EXE_ssr-replay"))
        .arg("render")
        .arg(&format_two_dump)
        .args(["--profile"])
        .arg(&profile)
        .args(["--out"])
        .arg(&replay_wav)
        .args(["--arrival-slack-ms", "0"])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(wav_samples(&replay_wav), direct);

    let legacy = render_reads(&temp.0, &stream, None);
    let first_audible = |samples: &[f32]| {
        samples
            .as_chunks::<2>()
            .0
            .iter()
            .position(|frame| frame[1].abs() > 1.0e-4)
    };
    let replay_start = first_audible(&replayed);
    let legacy_start = first_audible(&legacy);
    // Legacy dumps have no READ_END clock and retain their old deterministic
    // render. Format 2 may advance idle time between reads, but both paths must
    // still contain the later endpoint's audible signal.
    assert!(replay_start.is_some());
    assert!(legacy_start.is_some());
}

#[test]
fn equivalent_qpc_frequencies_render_identically() {
    let temp = TempDir::new();
    write_ir(&temp.0.join("FL.wav"));
    write_ir(&temp.0.join("FR.wav"));
    let ten_mhz = temp.0.join("ten-mhz.dump");
    let one_mhz = temp.0.join("one-mhz.dump");
    let profile = temp.0.join("profile.toml");
    write_timed_dump(&ten_mhz, 10_000_000, 100_000);
    write_timed_dump(&one_mhz, 1_000_000, 10_000);
    write_profile(&profile);

    let mut rendered = Vec::new();
    for (dump, stem) in [(&ten_mhz, "ten"), (&one_mhz, "one")] {
        let output = temp.0.join(format!("{stem}.wav"));
        let result = Command::new(env!("CARGO_BIN_EXE_ssr-replay"))
            .arg("render")
            .arg(dump)
            .args(["--profile"])
            .arg(&profile)
            .args(["--out"])
            .arg(&output)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        rendered.push(std::fs::read(output).unwrap());
    }
    assert_eq!(rendered[0], rendered[1]);
}

#[test]
fn arrival_slack_ms_accepts_zero_and_rejects_negative_values() {
    let temp = TempDir::new();
    write_ir(&temp.0.join("FL.wav"));
    write_ir(&temp.0.join("FR.wav"));
    let dump = temp.0.join("input.dump");
    let profile = temp.0.join("profile.toml");
    write_dump(&dump);
    write_profile(&profile);

    let accepted = Command::new(env!("CARGO_BIN_EXE_ssr-replay"))
        .arg("render")
        .arg(&dump)
        .args(["--profile"])
        .arg(&profile)
        .args(["--out"])
        .arg(temp.0.join("zero.wav"))
        .args(["--arrival-slack-ms", "0"])
        .output()
        .unwrap();
    assert!(
        accepted.status.success(),
        "{}",
        String::from_utf8_lossy(&accepted.stderr)
    );

    let rejected = Command::new(env!("CARGO_BIN_EXE_ssr-replay"))
        .arg("render")
        .arg(&dump)
        .args(["--profile"])
        .arg(&profile)
        .args(["--out"])
        .arg(temp.0.join("negative.wav"))
        .arg("--arrival-slack-ms=-1")
        .output()
        .unwrap();
    assert!(!rejected.status.success());
    assert!(
        String::from_utf8_lossy(&rejected.stderr).contains("finite non-negative"),
        "{}",
        String::from_utf8_lossy(&rejected.stderr)
    );
}

#[test]
fn missing_profile_fails_with_a_readable_message() {
    let temp = TempDir::new();
    let dump = temp.0.join("input.dump");
    write_dump(&dump);
    let missing = temp.0.join("missing.toml");
    let result = Command::new(env!("CARGO_BIN_EXE_ssr-replay"))
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
    assert!(stderr.contains("ssr-replay:"), "{stderr}");
}

#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use bio_abi::{RECORD_FLAG_DISCONTINUITY, Record, RecordWriter, wave_format_bytes};
use bio_types::{BitstreamCodec, BitstreamFormat, PcmFormat, SampleType, SourceFormat};
use brir::{AnglePreset, BrirSource, DirectionFile, LfePolicy, Profile};
use engine::{Chain, ChainConfig, ChainEvent, Prepared, partition_for};

struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock follows Unix epoch")
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("engine-{label}-{}-{unique}", std::process::id()));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn write_stereo_ir(
    path: &Path,
    frames: usize,
    left_delay: usize,
    left_gain: f32,
    right_delay: usize,
    right_gain: f32,
) {
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
    for frame in 0..frames {
        writer
            .write_sample(if frame == left_delay { left_gain } else { 0.0 })
            .unwrap();
        writer
            .write_sample(if frame == right_delay {
                right_gain
            } else {
                0.0
            })
            .unwrap();
    }
    writer.finalize().unwrap();
}

fn profile(files: Vec<DirectionFile>, gain_db: f32, ceiling_dbfs: f32) -> Profile {
    Profile {
        name: "test".into(),
        brir: BrirSource::Directions(files),
        headphone_fir: None,
        gain_db,
        limiter_ceiling_dbfs: ceiling_dbfs,
        lfe: LfePolicy::Mute,
        angles: AnglePreset::Standard,
        angle_overrides: Vec::new(),
        truncate: None,
    }
}

fn stereo_pcm_format() -> SourceFormat {
    SourceFormat::Pcm(PcmFormat {
        channels: 2,
        sample_rate: 48_000,
        bits_per_sample: 32,
        valid_bits: 32,
        sample_type: SampleType::Float,
        channel_mask: 3,
    })
}

fn pcm_stream(blocks: usize) -> Vec<u8> {
    let format = stereo_pcm_format();
    let mut samples = vec![0_u8; blocks * 480 * 2 * 4];
    for frame in samples.as_chunks_mut::<8>().0 {
        frame[..4].copy_from_slice(&0.0_f32.to_le_bytes());
        frame[4..].copy_from_slice(&0.0_f32.to_le_bytes());
    }
    samples[..4].copy_from_slice(&0.2_f32.to_le_bytes());
    samples[4..8].copy_from_slice(&0.4_f32.to_le_bytes());
    let mut stream = vec![0; bio_abi::record_size(52) + bio_abi::record_size(samples.len()) + 64];
    let mut writer = RecordWriter::new(&mut stream);
    assert!(writer.format_change(1, 0, &wave_format_bytes(&format)));
    assert!(writer.data(1, 0, 10, &samples));
    let length = writer.len();
    stream.truncate(length);
    stream
}

#[test]
fn pcm_dump_renders_pans_convolves_and_applies_gain() {
    let temp = TempDir::new("pcm-chain");
    let fl = temp.0.join("FL.wav");
    let fr = temp.0.join("FR.wav");
    write_stereo_ir(&fl, 16, 0, 0.5, 1, 0.25);
    write_stereo_ir(&fr, 16, 2, 0.125, 3, 0.0625);
    let profile = profile(
        vec![
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
        ],
        6.020_600_3,
        0.0,
    );
    let prepared = brir::prepare(&profile, 48_000, 480).unwrap();
    let mut chain = Chain::new(ChainConfig::default());
    chain.set_prepared(Box::new(Prepared::new(Arc::new(prepared), 480).unwrap()));

    let mut blocks = 0;
    chain.push_records(&pcm_stream(1), &mut |event| {
        if let ChainEvent::Block(block) = event {
            blocks += 1;
            assert_eq!((block.epoch, block.rate, block.source_qpc), (1, 48_000, 10));
            assert_eq!(block.left.len(), 480);
            let expected_left = [(0, 0.2), (2, 0.1)];
            let expected_right = [(1, 0.1), (3, 0.05)];
            for (frame, &sample) in block.left.iter().enumerate() {
                let expected = expected_left
                    .iter()
                    .find(|(at, _)| *at == frame)
                    .map_or(0.0, |(_, value)| *value);
                assert!(
                    (sample - expected).abs() < 2.0e-5,
                    "left frame {frame}: {sample} != {expected}"
                );
            }
            for (frame, &sample) in block.right.iter().enumerate() {
                let expected = expected_right
                    .iter()
                    .find(|(at, _)| *at == frame)
                    .map_or(0.0, |(_, value)| *value);
                assert!(
                    (sample - expected).abs() < 2.0e-5,
                    "right frame {frame}: {sample} != {expected}"
                );
            }
        }
    });
    assert_eq!(blocks, 1);
}

#[test]
fn ddp_burst_produces_a_nonzero_binaural_block() {
    let temp = TempDir::new("ddp-chain");
    let directions = [
        ("FL", 30.0),
        ("FR", -30.0),
        ("FC", 0.0),
        ("SL", 90.0),
        ("SR", -90.0),
        ("BL", 150.0),
        ("BR", -150.0),
        ("BC", 180.0),
    ];
    let mut files = Vec::new();
    for (index, (name, azimuth)) in directions.into_iter().enumerate() {
        let path = temp.0.join(format!("{name}.wav"));
        write_stereo_ir(&path, 32, index, 0.5, 15 - index, 0.5);
        files.push(DirectionFile {
            name: name.into(),
            azimuth_deg: azimuth,
            elevation_deg: 0.0,
            path,
        });
    }
    let profile = profile(files, 0.0, 0.0);
    let prepared = brir::prepare(&profile, 48_000, partition_for(1536)).unwrap();
    let mut chain = Chain::new(ChainConfig::default());
    chain.set_prepared(Box::new(Prepared::new(Arc::new(prepared), 1536).unwrap()));

    let fixture = std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../decode/tests/fixtures/ddp/tones-5.1.eac3"),
    )
    .unwrap();
    let payload = &fixture[..2560];
    let mut burst = Vec::new();
    bitstream::frame_burst(bitstream::DataType::Eac3, payload, 24_576, &mut burst);
    let format = SourceFormat::Bitstream(BitstreamFormat {
        codec: BitstreamCodec::Eac3,
        container_rate: 192_000,
        container_channels: 2,
        encoded_rate: 48_000,
        encoded_channels: 6,
    });
    let mut stream = vec![0; bio_abi::record_size(52) + bio_abi::record_size(burst.len()) + 64];
    let mut writer = RecordWriter::new(&mut stream);
    assert!(writer.format_change(7, 0, &wave_format_bytes(&format)));
    assert!(writer.data(7, 0, 123, &burst));
    let length = writer.len();
    stream.truncate(length);

    let mut blocks = 0;
    let mut errors = 0;
    chain.push_records(&stream, &mut |event| match event {
        ChainEvent::Block(block) => {
            blocks += 1;
            assert_eq!(block.left.len(), 1536);
            assert!(block.left.iter().map(|sample| sample.abs()).sum::<f32>() > 0.01);
            assert!(block.right.iter().map(|sample| sample.abs()).sum::<f32>() > 0.01);
        }
        ChainEvent::Error(_) => errors += 1,
        _ => {}
    });
    assert_eq!(blocks, 1);
    assert_eq!(errors, 0);
}

#[test]
fn overrun_and_discontinuity_reset_without_panicking() {
    let mut chain = Chain::new(ChainConfig::default());
    chain.push_record(
        Record::FormatChange {
            epoch: 1,
            qpc: 0,
            format: stereo_pcm_format(),
        },
        &mut |_| {},
    );
    chain.push_record(
        Record::Data {
            epoch: 1,
            flags: 0,
            pos: 0,
            qpc: 1,
            qpc_written: 0,
            bytes: &[0; 7],
        },
        &mut |_| {},
    );
    let mut overruns = 0;
    chain.push_record(
        Record::Overrun {
            pos: 7,
            qpc: 2,
            lost_bytes: 99,
        },
        &mut |event| {
            if matches!(event, ChainEvent::Overrun { lost_bytes: 99 }) {
                overruns += 1;
            }
        },
    );
    chain.push_record(
        Record::Data {
            epoch: 1,
            flags: RECORD_FLAG_DISCONTINUITY,
            pos: 106,
            qpc: 3,
            qpc_written: 0,
            bytes: &[0; 13],
        },
        &mut |_| {},
    );
    assert_eq!(overruns, 1);
}

#[test]
fn needs_brir_is_emitted_once_for_repeated_rate_mismatch() {
    let temp = TempDir::new("needs-brir");
    let ir = temp.0.join("FL.wav");
    write_stereo_ir(&ir, 16, 0, 1.0, 0, 1.0);
    let profile = profile(
        vec![DirectionFile {
            name: "FL".into(),
            azimuth_deg: 30.0,
            elevation_deg: 0.0,
            path: ir,
        }],
        0.0,
        0.0,
    );
    let prepared = brir::prepare(&profile, 96_000, 480).unwrap();
    let mut chain = Chain::new(ChainConfig::default());
    chain.set_prepared(Box::new(Prepared::new(Arc::new(prepared), 480).unwrap()));

    let mut needs = 0;
    chain.push_records(&pcm_stream(2), &mut |event| {
        if matches!(
            event,
            ChainEvent::NeedsBrir {
                rate: 48_000,
                block: 480
            }
        ) {
            needs += 1;
        }
    });
    assert_eq!(needs, 1);
}

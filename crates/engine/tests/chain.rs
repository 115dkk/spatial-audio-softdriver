#![forbid(unsafe_code)]

use std::mem::size_of;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use bio_abi::{
    AbiError, ENDPOINT_PASSTHROUGH, ENDPOINT_PRIMARY, ENDPOINT_SECOND, MAX_ENDPOINTS,
    RECORD_FLAG_DISCONTINUITY, RECORD_KIND_FORMAT_CHANGE, RECORD_KIND_STREAM_STOP,
    RECORD_KIND_VOLUME, Record, RecordHeader, RecordWriter, wave_format_bytes,
};
use bio_types::{
    BitstreamCodec, BitstreamFormat, Direction, Layout, PcmFormat, SampleType, SourceFormat,
};
use brir::{AnglePreset, BrirSource, DirectionFile, LfePolicy, Profile};
use engine::{
    Chain, ChainConfig, ChainEvent, EndpointFilter, MAX_ARRIVAL_SLACK_FRAMES, MIX_FRAMES, Prepared,
    partition_for,
};
use speakers::{PreparedSpeaker, PreparedSpeakers};

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
    write_stereo_ir_at_rate(
        path,
        48_000,
        frames,
        left_delay,
        left_gain,
        right_delay,
        right_gain,
    );
}

fn write_stereo_ir_at_rate(
    path: &Path,
    rate: u32,
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
            sample_rate: rate,
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
        trim_leading_delay: false,
    }
}

fn bitstream_format(codec: BitstreamCodec, encoded_rate: u32) -> SourceFormat {
    SourceFormat::Bitstream(BitstreamFormat {
        codec,
        container_rate: 192_000,
        container_channels: 2,
        encoded_rate,
        encoded_channels: 6,
    })
}

fn stereo_pcm_format() -> SourceFormat {
    pcm_format(48_000, 2, 3)
}

fn pcm_format(sample_rate: u32, channels: u16, channel_mask: u32) -> SourceFormat {
    SourceFormat::Pcm(PcmFormat {
        channels,
        sample_rate,
        bits_per_sample: 32,
        valid_bits: 32,
        sample_type: SampleType::Float,
        channel_mask,
    })
}

fn pcm_bytes(value: f32, frames: usize) -> Vec<u8> {
    let mut samples = vec![0_u8; frames * 2 * 4];
    for frame in samples.as_chunks_mut::<8>().0 {
        frame[..4].copy_from_slice(&value.to_le_bytes());
        frame[4..].copy_from_slice(&value.to_le_bytes());
    }
    samples
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
    // The ABI timestamps the last payload byte; 100,000 ticks are the 10 ms
    // record duration at the default 10 MHz QPC frequency.
    assert!(writer.data(1, 0, 100_010, &samples));
    let length = writer.len();
    stream.truncate(length);
    stream
}

fn identity_prepared(label: &str) -> Box<Prepared> {
    delayed_prepared(label, 1, 0)
}

fn identity_speaker_prepared() -> Box<Prepared> {
    let directions = vec![
        Direction::new("FL", 30.0, 0.0),
        Direction::new("FR", -30.0, 0.0),
    ];
    Box::new(
        Prepared::speakers(
            Arc::new(PreparedSpeakers {
                layout: Layout::new(directions).unwrap(),
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
}

fn delayed_prepared(label: &str, ir_frames: usize, delayed_tap: usize) -> Box<Prepared> {
    let temp = TempDir::new(label);
    let ir = temp.0.join("FC.wav");
    write_stereo_ir(&ir, ir_frames, delayed_tap, 1.0, delayed_tap, 1.0);
    let prepared = brir::prepare(
        &profile(
            vec![DirectionFile {
                name: "FC".into(),
                azimuth_deg: 0.0,
                elevation_deg: 0.0,
                path: ir,
            }],
            0.0,
            0.0,
        ),
        48_000,
        480,
    )
    .unwrap();
    Box::new(Prepared::new(Arc::new(prepared), 480).unwrap())
}

#[test]
fn repeated_format_initializes_once_and_subsequent_restatements_are_noops() {
    let samples = pcm_bytes(0.25, MIX_FRAMES);
    let format = stereo_pcm_format();
    let run = |format_records: usize| {
        let mut chain = Chain::new(ChainConfig {
            arrival_slack_frames: 0,
            ..ChainConfig::default()
        });
        chain.set_prepared(identity_prepared("format-restatement"));
        let mut formats = 0;
        for _ in 0..format_records {
            chain.push_record(
                Record::FormatChange {
                    endpoint: ENDPOINT_PRIMARY,
                    epoch: 7,
                    repeat: true,
                    qpc: 0,
                    format,
                },
                &mut |event| {
                    if matches!(event, ChainEvent::FormatChanged { .. }) {
                        formats += 1;
                    }
                },
            );
        }
        let mut blocks = Vec::new();
        chain.push_record(
            Record::Data {
                endpoint: ENDPOINT_PRIMARY,
                epoch: 7,
                flags: 0,
                pos: 0,
                qpc: MIX_FRAMES as u64,
                qpc_written: 0,
                bytes: &samples,
            },
            &mut |event| {
                if let ChainEvent::Block(block) = event {
                    blocks.push((block.channel(0).to_vec(), block.channel(1).to_vec()));
                }
            },
        );
        chain.finish(&mut |event| {
            if let ChainEvent::Block(block) = event {
                blocks.push((block.channel(0).to_vec(), block.channel(1).to_vec()));
            }
        });
        (formats, blocks)
    };

    let once = run(1);
    let twice = run(2);
    assert_eq!(once.0, 1);
    assert_eq!(twice.0, 1);
    assert_eq!(once.1, twice.1);
    assert!(
        once.1
            .iter()
            .flat_map(|(left, right)| left.iter().chain(right))
            .any(|sample| *sample != 0.0)
    );
}

#[test]
fn repeated_unsupported_format_is_reported_only_once() {
    let format = pcm_format(12_345, 2, 3);
    let mut chain = Chain::new(ChainConfig::default());
    let mut errors = 0;
    let mut formats = 0;
    for _ in 0..2 {
        chain.push_record(
            Record::FormatChange {
                endpoint: ENDPOINT_PRIMARY,
                epoch: 11,
                repeat: true,
                qpc: 0,
                format,
            },
            &mut |event| match event {
                ChainEvent::Error { .. } => errors += 1,
                ChainEvent::FormatChanged { .. } => formats += 1,
                _ => {}
            },
        );
    }
    assert_eq!((errors, formats), (1, 1));
}

#[test]
fn repeated_stream_start_is_state_dependent_and_sample_exact() {
    let samples = pcm_bytes(0.25, MIX_FRAMES);
    let split = samples.len() / 2;
    let run = |first_repeat: bool, repeat_while_active: bool| {
        let mut chain = Chain::new(ChainConfig {
            arrival_slack_frames: 0,
            ..ChainConfig::default()
        });
        chain.set_prepared(identity_prepared("stream-start-restatement"));
        let mut output = [Vec::new(), Vec::new()];
        let mut collect = |event: ChainEvent<'_>| {
            if let ChainEvent::Block(block) = event {
                output[0].extend_from_slice(block.channel(0));
                output[1].extend_from_slice(block.channel(1));
            }
        };
        chain.push_record(
            Record::FormatChange {
                endpoint: ENDPOINT_PRIMARY,
                epoch: 7,
                repeat: true,
                qpc: 0,
                format: stereo_pcm_format(),
            },
            &mut collect,
        );
        chain.push_record(
            Record::StreamStart {
                endpoint: ENDPOINT_PRIMARY,
                epoch: 7,
                repeat: first_repeat,
                qpc: 0,
            },
            &mut collect,
        );
        chain.push_record(
            Record::Data {
                endpoint: ENDPOINT_PRIMARY,
                epoch: 7,
                flags: 0,
                pos: 0,
                qpc: 50_000,
                qpc_written: 0,
                bytes: &samples[..split],
            },
            &mut collect,
        );
        if repeat_while_active {
            chain.push_record(
                Record::StreamStart {
                    endpoint: ENDPOINT_PRIMARY,
                    epoch: 7,
                    repeat: true,
                    qpc: 50_000,
                },
                &mut collect,
            );
        }
        chain.push_record(
            Record::Data {
                endpoint: ENDPOINT_PRIMARY,
                epoch: 7,
                flags: 0,
                pos: split as u64,
                qpc: 100_000,
                qpc_written: 0,
                bytes: &samples[split..],
            },
            &mut collect,
        );
        chain.finish(&mut collect);
        drop(collect);
        (output, chain.lane_underrun_frames(), chain.dropped_frames())
    };

    let normal = run(false, false);
    let repeated = run(true, true);
    assert_eq!(normal, repeated);
    assert!(normal.0.iter().flatten().any(|sample| *sample != 0.0));
}

#[test]
fn repeated_stream_stop_is_state_dependent_and_sample_exact() {
    let samples = pcm_bytes(0.25, MIX_FRAMES);
    let run = |active_repeat: bool, idle_repeat: bool| {
        let mut chain = Chain::new(ChainConfig {
            arrival_slack_frames: 0,
            ..ChainConfig::default()
        });
        chain.set_prepared(identity_prepared("stream-stop-restatement"));
        let mut output = [Vec::new(), Vec::new()];
        let mut collect = |event: ChainEvent<'_>| {
            if let ChainEvent::Block(block) = event {
                output[0].extend_from_slice(block.channel(0));
                output[1].extend_from_slice(block.channel(1));
            }
        };
        chain.push_record(
            Record::FormatChange {
                endpoint: ENDPOINT_PRIMARY,
                epoch: 7,
                repeat: true,
                qpc: 0,
                format: stereo_pcm_format(),
            },
            &mut collect,
        );
        chain.push_record(
            Record::Data {
                endpoint: ENDPOINT_PRIMARY,
                epoch: 7,
                flags: 0,
                pos: 0,
                qpc: 100_000,
                qpc_written: 0,
                bytes: &samples,
            },
            &mut collect,
        );
        chain.push_record(
            Record::StreamStop {
                endpoint: ENDPOINT_PRIMARY,
                epoch: 7,
                repeat: active_repeat,
                qpc: 100_000,
            },
            &mut collect,
        );
        if idle_repeat {
            chain.push_record(
                Record::StreamStop {
                    endpoint: ENDPOINT_PRIMARY,
                    epoch: 7,
                    repeat: true,
                    qpc: 100_000,
                },
                &mut collect,
            );
        }
        chain.push_record(
            Record::Data {
                endpoint: ENDPOINT_PRIMARY,
                epoch: 7,
                flags: RECORD_FLAG_DISCONTINUITY,
                pos: samples.len() as u64,
                qpc: 200_000,
                qpc_written: 0,
                bytes: &samples,
            },
            &mut collect,
        );
        chain.finish(&mut collect);
        drop(collect);
        (output, chain.lane_underrun_frames(), chain.dropped_frames())
    };

    let normal = run(false, false);
    assert_eq!(normal, run(true, false));
    assert_eq!(normal, run(false, true));
    assert!(normal.0.iter().flatten().any(|sample| *sample != 0.0));
}

#[test]
fn stopped_tail_survives_overrun_restatement_and_idle_sample_exactly() {
    let mut active = mono_pcm_bytes(0.0, MIX_FRAMES);
    active[..size_of::<f32>()].copy_from_slice(&1.0_f32.to_le_bytes());
    let run = |restate: bool| {
        let mut chain = Chain::with_qpc_frequency(ChainConfig::default(), 1_000_000);
        chain.set_prepared(delayed_prepared("stopped-overrun-tail", 20_000, 19_999));
        let mut output = Vec::new();
        let mut overruns = 0;
        let mut collect = |event: ChainEvent<'_>| match event {
            ChainEvent::Block(block) => output.push((
                block.idle,
                block.channel(0).to_vec(),
                block.channel(1).to_vec(),
            )),
            ChainEvent::Overrun { lost_bytes: 99, .. } => overruns += 1,
            _ => {}
        };
        chain.push_record(
            Record::FormatChange {
                endpoint: ENDPOINT_PRIMARY,
                epoch: 7,
                repeat: true,
                qpc: 0,
                format: pcm_format(48_000, 1, 0x4),
            },
            &mut collect,
        );
        chain.push_record(
            Record::Data {
                endpoint: ENDPOINT_PRIMARY,
                epoch: 7,
                flags: 0,
                pos: 0,
                qpc: 10_000,
                qpc_written: 0,
                bytes: &active,
            },
            &mut collect,
        );
        chain.push_record(
            Record::StreamStop {
                endpoint: ENDPOINT_PRIMARY,
                epoch: 7,
                repeat: false,
                qpc: 10_000,
            },
            &mut collect,
        );
        if restate {
            chain.push_record(
                Record::Overrun {
                    endpoint: ENDPOINT_PRIMARY,
                    pos: active.len() as u64,
                    qpc: 10_000,
                    lost_bytes: 99,
                },
                &mut collect,
            );
            chain.push_record(
                Record::FormatChange {
                    endpoint: ENDPOINT_PRIMARY,
                    epoch: 7,
                    repeat: true,
                    qpc: 10_000,
                    format: pcm_format(48_000, 1, 0x4),
                },
                &mut collect,
            );
            chain.push_record(
                Record::StreamStop {
                    endpoint: ENDPOINT_PRIMARY,
                    epoch: 7,
                    repeat: true,
                    qpc: 10_000,
                },
                &mut collect,
            );
        }
        chain.idle(600_000, &mut collect);
        drop(collect);
        (output, overruns)
    };

    let baseline = run(false);
    let restated = run(true);
    assert_eq!(restated.1, 1);
    assert_eq!(baseline.0, restated.0);
    assert!(baseline.0.iter().any(|(idle, left, right)| {
        *idle && left.iter().chain(right).any(|sample| *sample != 0.0)
    }));
}

fn render_pcm_chunks(chunks: &[&[u8]], stream_stop: bool) -> Vec<(Vec<f32>, Vec<f32>)> {
    let mut chain = Chain::new(ChainConfig {
        arrival_slack_frames: 0,
        ..ChainConfig::default()
    });
    chain.set_prepared(identity_prepared("pcm-chunks"));
    chain.push_record(
        Record::FormatChange {
            endpoint: ENDPOINT_PRIMARY,
            epoch: 1,
            repeat: false,
            qpc: 0,
            format: stereo_pcm_format(),
        },
        &mut |_| {},
    );
    let mut blocks = Vec::new();
    for (index, bytes) in chunks.iter().enumerate() {
        chain.push_record(
            Record::Data {
                endpoint: ENDPOINT_PRIMARY,
                epoch: 1,
                flags: 0,
                pos: index as u64,
                qpc: index as u64 * 1_000,
                qpc_written: 0,
                bytes,
            },
            &mut |event| {
                if let ChainEvent::Block(block) = event {
                    blocks.push((block.channel(0).to_vec(), block.channel(1).to_vec()));
                }
            },
        );
    }
    if stream_stop {
        chain.push_record(
            Record::StreamStop {
                endpoint: ENDPOINT_PRIMARY,
                epoch: 1,
                repeat: false,
                qpc: 1_000_000,
            },
            &mut |event| {
                if let ChainEvent::Block(block) = event {
                    blocks.push((block.channel(0).to_vec(), block.channel(1).to_vec()));
                }
            },
        );
    } else {
        chain.finish(&mut |event| {
            if let ChainEvent::Block(block) = event {
                blocks.push((block.channel(0).to_vec(), block.channel(1).to_vec()));
            }
        });
    }
    blocks
}

#[test]
fn one_4800_frame_pcm_record_matches_one_ms_records() {
    let input = pcm_bytes(0.25, 4_800);
    let whole = render_pcm_chunks(&[&input], false);
    let small = render_pcm_chunks(&input.chunks(48 * 2 * 4).collect::<Vec<_>>(), false);
    assert_eq!(whole, small);
    assert!(whole.len() > 10, "output tail was not appended");
}

fn separated_prepared(label: &str) -> Box<Prepared> {
    let temp = TempDir::new(label);
    let fl = temp.0.join("FL.wav");
    let fr = temp.0.join("FR.wav");
    write_stereo_ir(&fl, 1, 0, 1.0, 0, 0.0);
    write_stereo_ir(&fr, 1, 0, 0.0, 0, 1.0);
    let mut profile = profile(
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
        0.0,
        0.0,
    );
    profile.lfe = LfePolicy::Direction {
        name: "FL".into(),
        gain_db: 0.0,
    };
    let prepared = brir::prepare(&profile, 48_000, 480).unwrap();
    Box::new(Prepared::new(Arc::new(prepared), 480).unwrap())
}

#[derive(Debug, PartialEq)]
struct RenderedStream {
    output: [Vec<f32>; 2],
    source_qpcs: Vec<u64>,
    errors: usize,
    underruns: u64,
    drops: u64,
}

fn render_push_records_stream(stream: &[u8]) -> RenderedStream {
    render_push_records_stream_with(stream, ChainConfig::default())
}

fn render_push_records_stream_with(stream: &[u8], config: ChainConfig) -> RenderedStream {
    let mut chain = Chain::with_qpc_frequency(config, 1_000_000);
    chain.set_prepared(separated_prepared("push-records-order"));
    let mut output = [Vec::new(), Vec::new()];
    let mut source_qpcs = Vec::new();
    let mut errors = 0;
    let mut collect = |event: ChainEvent<'_>| match event {
        ChainEvent::Block(block) => {
            output[0].extend_from_slice(block.channel(0));
            output[1].extend_from_slice(block.channel(1));
            source_qpcs.push(block.source_qpc);
        }
        ChainEvent::Error { .. } => errors += 1,
        _ => {}
    };
    chain.push_records(stream, &mut collect);
    chain.finish(&mut collect);
    drop(collect);
    RenderedStream {
        output,
        source_qpcs,
        errors,
        underruns: chain.lane_underrun_frames(),
        drops: chain.dropped_frames(),
    }
}

fn mono_pcm_bytes(value: f32, frames: usize) -> Vec<u8> {
    value.to_le_bytes().repeat(frames)
}

#[test]
fn plain_stereo_ignores_direct_override_but_zero_mask_extensible_uses_it() {
    let mut direct = decode::direct::DirectLayouts::default();
    direct
        .set(
            2,
            decode::direct::DirectLayout::new(&[bio_types::Speaker::R, bio_types::Speaker::L])
                .unwrap(),
        )
        .unwrap();
    let render = |format_bytes: Vec<u8>| {
        let left = 0.25_f32;
        let mut samples = vec![0; 480 * 2 * size_of::<f32>()];
        for frame in samples.as_chunks_mut::<8>().0 {
            frame[..4].copy_from_slice(&left.to_le_bytes());
        }
        let mut stream =
            vec![0; bio_abi::record_size(format_bytes.len()) + bio_abi::record_size(samples.len())];
        let mut writer = RecordWriter::new(&mut stream);
        assert!(writer.format_change(1, 0, &format_bytes));
        assert!(writer.data(1, 0, 10_000, &samples));
        let length = writer.len();
        stream.truncate(length);
        render_push_records_stream_with(
            &stream,
            ChainConfig {
                direct,
                ..ChainConfig::default()
            },
        )
    };

    let mut plain = Vec::new();
    plain.extend_from_slice(&1_u16.to_le_bytes());
    plain.extend_from_slice(&2_u16.to_le_bytes());
    plain.extend_from_slice(&48_000_u32.to_le_bytes());
    plain.extend_from_slice(&384_000_u32.to_le_bytes());
    plain.extend_from_slice(&8_u16.to_le_bytes());
    plain.extend_from_slice(&32_u16.to_le_bytes());
    plain.extend_from_slice(&0_u16.to_le_bytes());
    let plain = render(plain);
    assert!(plain.output[0].iter().any(|sample| *sample != 0.0));
    assert!(plain.output[1].iter().all(|sample| *sample == 0.0));

    let direct = render(wave_format_bytes(&pcm_format(48_000, 2, 0)));
    assert!(direct.output[0].iter().all(|sample| *sample == 0.0));
    assert!(direct.output[1].iter().any(|sample| *sample != 0.0));
}

#[test]
fn legacy_volume_record_attenuates_rendered_output() {
    let format = pcm_format(48_000, 2, 3);
    let samples = pcm_bytes(0.25, 480);
    let mut legacy_volume = [0_u8; 72];
    legacy_volume[..4].copy_from_slice(&2_u32.to_le_bytes());
    for level in legacy_volume[8..16].as_chunks_mut::<4>().0 {
        *level = (-20_i32 * 65_536).to_le_bytes();
    }
    let mut stream = vec![
        0;
        bio_abi::record_size(40)
            + bio_abi::record_size(legacy_volume.len())
            + bio_abi::record_size(samples.len())
    ];
    let mut writer = RecordWriter::new(&mut stream);
    assert!(writer.format_change(1, 0, &wave_format_bytes(&format)));
    assert!(writer.push(
        RecordHeader {
            kind: RECORD_KIND_VOLUME,
            flags: 0,
            epoch: 0,
            byte_pos: 0,
            qpc: 0,
            qpc_written: 0,
            len: 0,
            endpoint: 0,
            reserved: 0,
        },
        &legacy_volume,
    ));
    assert!(writer.data(1, 0, 10_000, &samples));
    let length = writer.len();
    stream.truncate(length);

    let rendered = render_push_records_stream(&stream);
    assert_eq!(rendered.errors, 0);
    let settled_peak = rendered
        .output
        .iter()
        .flat_map(|channel| channel.iter().skip(64))
        .map(|sample| sample.abs())
        .fold(0.0_f32, f32::max);
    assert!(settled_peak > 0.0);
    assert!(
        settled_peak < 0.05,
        "legacy -20 dB volume was not applied after its ramp: {settled_peak}"
    );
}

#[test]
fn default_chain_decodes_all_six_pcm_rates_through_record_stream() {
    for rate in [44_100, 48_000, 88_200, 96_000, 176_400, 192_000] {
        let format = pcm_format(rate, 2, 3);
        let frames = rate as usize / 100;
        let samples = pcm_bytes(0.25, frames);
        let mut stream = vec![0; bio_abi::record_size(40) + bio_abi::record_size(samples.len())];
        let mut writer = RecordWriter::new(&mut stream);
        assert!(writer.format_change(1, 0, &wave_format_bytes(&format)));
        assert!(writer.data(1, 0, rate as u64 / 100, &samples));
        let length = writer.len();
        stream.truncate(length);

        let rendered = render_push_records_stream(&stream);
        assert_eq!(rendered.errors, 0, "{rate} Hz was rejected");
        assert!(
            rendered
                .output
                .iter()
                .flatten()
                .any(|sample| *sample != 0.0),
            "{rate} Hz produced silence"
        );
    }
}

fn render_records_one_at_a_time(stream: &[u8]) -> RenderedStream {
    let mut chain = Chain::with_qpc_frequency(ChainConfig::default(), 1_000_000);
    chain.set_prepared(separated_prepared("push-record-one-at-a-time"));
    let mut output = [Vec::new(), Vec::new()];
    let mut source_qpcs = Vec::new();
    let mut errors = 0;
    let mut collect = |event: ChainEvent<'_>| match event {
        ChainEvent::Block(block) => {
            output[0].extend_from_slice(block.channel(0));
            output[1].extend_from_slice(block.channel(1));
            source_qpcs.push(block.source_qpc);
        }
        ChainEvent::Error { .. } => errors += 1,
        _ => {}
    };
    for record in bio_abi::records(stream) {
        chain.push_record(record.unwrap(), &mut collect);
    }
    chain.finish(&mut collect);
    drop(collect);
    RenderedStream {
        output,
        source_qpcs,
        errors,
        underruns: chain.lane_underrun_frames(),
        drops: chain.dropped_frames(),
    }
}

fn three_channel_pcm_bytes(values: [f32; 3], frames: usize) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(frames * values.len() * size_of::<f32>());
    for _ in 0..frames {
        for value in values {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
    }
    bytes
}

#[derive(Clone, Copy)]
enum PcmRecordLayout {
    PrimaryWholeThenSecondary { secondary_frames: usize },
    RepeatedLateTicks { frames_per_tick: usize },
}

fn timed_pcm_stream(
    layout: PcmRecordLayout,
    explicit_start: bool,
    first_data_flags: u16,
) -> Vec<u8> {
    timed_pcm_stream_with_lfe(layout, explicit_start, first_data_flags, 0.5)
}

fn timed_pcm_stream_with_lfe(
    layout: PcmRecordLayout,
    explicit_start: bool,
    first_data_flags: u16,
    lfe: f32,
) -> Vec<u8> {
    let primary_format = pcm_format(48_000, 1, 0x4);
    // FL, FR and LFE exercise every decoded object plus the separate LFE path.
    let secondary_format = pcm_format(48_000, 3, 0xb);
    let mut stream = vec![0; 100_000];
    let mut writer = RecordWriter::new(&mut stream);
    writer.set_endpoint(ENDPOINT_PRIMARY);
    assert!(writer.format_change(1, 0, &wave_format_bytes(&primary_format)));
    writer.set_endpoint(ENDPOINT_SECOND);
    assert!(writer.format_change(2, 0, &wave_format_bytes(&secondary_format)));
    if explicit_start {
        writer.set_endpoint(ENDPOINT_PRIMARY);
        assert!(writer.stream_start(1, 0));
        writer.set_endpoint(ENDPOINT_SECOND);
        assert!(writer.stream_start(2, 0));
    }

    let mut write_data = |endpoint, epoch, frame_pos: usize, frames: usize, qpc: u64| {
        let (bytes, frame_bytes) = if endpoint == ENDPOINT_PRIMARY {
            (mono_pcm_bytes(0.125, frames), 4)
        } else {
            (three_channel_pcm_bytes([0.25, 0.375, lfe], frames), 12)
        };
        writer.set_endpoint(endpoint);
        // Each endpoint's first data record carries `first_data_flags`, so the
        // same layout covers an Idle lane started by plain data and by data
        // flagged as a discontinuity.
        let flags = if frame_pos == 0 { first_data_flags } else { 0 };
        assert!(writer.push(
            RecordHeader {
                kind: bio_abi::RECORD_KIND_DATA,
                flags,
                epoch,
                byte_pos: (frame_pos * frame_bytes) as u64,
                qpc,
                qpc_written: 0,
                len: 0,
                endpoint,
                reserved: 0,
            },
            &bytes,
        ));
    };
    match layout {
        PcmRecordLayout::PrimaryWholeThenSecondary { secondary_frames } => {
            write_data(ENDPOINT_PRIMARY, 1, 0, 4_800, 100_000);
            for start in (0..4_800).step_by(secondary_frames) {
                let end = start + secondary_frames;
                write_data(
                    ENDPOINT_SECOND,
                    2,
                    start,
                    secondary_frames,
                    end as u64 * 1_000 / 48,
                );
            }
        }
        PcmRecordLayout::RepeatedLateTicks { frames_per_tick } => {
            for start in (0..4_800).step_by(frames_per_tick) {
                let end = start + frames_per_tick;
                let qpc = end as u64 * 1_000 / 48;
                // This is the driver's late-DPC order: A's complete 20 ms record,
                // immediately followed by B's record for the same DMA interval.
                write_data(ENDPOINT_PRIMARY, 1, start, frames_per_tick, qpc);
                write_data(ENDPOINT_SECOND, 2, start, frames_per_tick, qpc);
            }
        }
    }
    let len = writer.len();
    stream.truncate(len);
    stream
}

#[test]
fn endpoints_two_and_three_mix_as_the_sum_of_their_solos() {
    let format = pcm_format(48_000, 1, 0x4);
    let mut stream = vec![0; 16_000];
    let mut writer = RecordWriter::new(&mut stream);
    for (endpoint, epoch, value) in [(ENDPOINT_PASSTHROUGH, 3, 0.125), (3, 4, 0.25)] {
        writer.set_endpoint(endpoint);
        assert!(writer.format_change(epoch, 0, &wave_format_bytes(&format)));
        assert!(writer.data(epoch, 0, 10_000, &mono_pcm_bytes(value, 480)));
    }
    let len = writer.len();
    stream.truncate(len);

    let mixed = render_push_records_stream(&stream);
    let passthrough =
        render_push_records_stream(&retain_endpoint_records(&stream, ENDPOINT_PASSTHROUGH));
    let reserved = render_push_records_stream(&retain_endpoint_records(&stream, 3));
    for channel in 0..2 {
        assert_eq!(
            mixed.output[channel].len(),
            passthrough.output[channel].len()
        );
        assert_eq!(mixed.output[channel].len(), reserved.output[channel].len());
        for (frame, ((mixed, passthrough), reserved)) in mixed.output[channel]
            .iter()
            .zip(&passthrough.output[channel])
            .zip(&reserved.output[channel])
            .enumerate()
        {
            assert!(
                (mixed - passthrough - reserved).abs() <= 2.0e-6,
                "channel={channel} frame={frame}: {mixed} != {passthrough} + {reserved}"
            );
        }
    }
    for rendered in [&mixed, &passthrough, &reserved] {
        assert_eq!(rendered.errors, 0);
        assert_eq!(rendered.underruns, 0);
        assert_eq!(rendered.drops, 0);
    }
}

fn first_nonzero(samples: &[f32]) -> Option<usize> {
    samples.iter().position(|sample| *sample != 0.0)
}

/// FFT convolution leaves ~1e-8 residue in a channel another endpoint excites;
/// an endpoint's own onset is the first sample clearly above that floor.
fn first_audible(samples: &[f32]) -> Option<usize> {
    samples.iter().position(|sample| sample.abs() > 1.0e-4)
}

fn assert_close(mixed: &[f32], solo: &[f32], label: &str) {
    assert_eq!(mixed.len(), solo.len(), "{label} length");
    for (frame, (mixed, solo)) in mixed.iter().zip(solo).enumerate() {
        assert!(
            (mixed - solo).abs() <= 2.0e-6,
            "{label} frame={frame}: {mixed} != {solo}"
        );
    }
}

fn retain_endpoint_records(stream: &[u8], endpoint: u16) -> Vec<u8> {
    const HEADER: usize = std::mem::size_of::<RecordHeader>();
    let mut retained = Vec::with_capacity(stream.len());
    let mut offset = 0;
    while offset < stream.len() {
        assert!(
            stream.len() - offset >= HEADER,
            "fixture header is complete"
        );
        let payload_len =
            u32::from_ne_bytes(stream[offset + 32..offset + 36].try_into().unwrap()) as usize;
        let owner = u16::from_ne_bytes(stream[offset + 36..offset + 38].try_into().unwrap());
        let record_len = bio_abi::record_size(payload_len);
        let end = offset.saturating_add(record_len).min(stream.len());
        assert!(
            end - offset >= HEADER + payload_len,
            "fixture payload is complete"
        );
        if owner == endpoint {
            retained.extend_from_slice(&stream[offset..end]);
        }
        offset = end;
    }
    retained
}

fn assert_separated_endpoints_match_solo(
    layout: PcmRecordLayout,
    explicit_start: bool,
    first_data_flags: u16,
) {
    const ENDPOINT_FRAMES: usize = 4_800;
    const MIXED_FRAMES: usize = 5_280;
    const RENDER_EPSILON: f32 = 2.0e-6;
    let stream = timed_pcm_stream(layout, explicit_start, first_data_flags);
    let mixed = render_push_records_stream(&stream);
    let primary = render_push_records_stream(&retain_endpoint_records(&stream, ENDPOINT_PRIMARY));
    let secondary = render_push_records_stream(&retain_endpoint_records(&stream, ENDPOINT_SECOND));

    // A is FC while B is FL/FR/LFE. Subtracting the true solo for the other
    // endpoint isolates each complete endpoint contribution in the mixed run.
    for (solo, other, label) in [
        (&primary, &secondary, "primary"),
        (&secondary, &primary, "secondary"),
    ] {
        for channel in 0..2 {
            assert!(mixed.output[channel].len() > MIXED_FRAMES);
            assert!(solo.output[channel].len() > MIXED_FRAMES);
            let isolated = mixed.output[channel]
                .iter()
                .enumerate()
                .map(|(index, sample)| {
                    sample - other.output[channel].get(index).copied().unwrap_or(0.0)
                })
                .collect::<Vec<_>>();
            let near_zero = |sample: &&f32| sample.abs() <= RENDER_EPSILON;
            let mixed_start = isolated.iter().position(|sample| !near_zero(&sample));
            let solo_start = solo.output[channel]
                .iter()
                .position(|sample| !near_zero(&sample));
            assert_eq!(
                mixed_start, solo_start,
                "{label} channel={channel} explicit_start={explicit_start} flags={first_data_flags} start"
            );
            let mixed_end = isolated
                .iter()
                .rposition(|sample| !near_zero(&sample))
                .map(|index| index + 1);
            let solo_end = solo.output[channel]
                .iter()
                .rposition(|sample| !near_zero(&sample))
                .map(|index| index + 1);
            assert_eq!(
                mixed_end.zip(mixed_start).map(|(end, start)| end - start),
                solo_end.zip(solo_start).map(|(end, start)| end - start),
                "{label} channel={channel} explicit_start={explicit_start} flags={first_data_flags} length"
            );
            let (Some(mixed_start), Some(solo_start)) = (mixed_start, solo_start) else {
                continue;
            };
            let compared = ENDPOINT_FRAMES.min(isolated.len() - mixed_start);
            for relative_frame in 0..compared {
                let mixed_frame = mixed_start + relative_frame;
                let solo_frame = solo_start + relative_frame;
                assert!(
                    (isolated[mixed_frame] - solo.output[channel][solo_frame]).abs()
                        <= RENDER_EPSILON,
                    "{label} channel={channel} frame={mixed_frame}: {} != {}",
                    isolated[mixed_frame],
                    solo.output[channel][solo_frame]
                );
            }
        }
    }
    for rendered in [&mixed, &primary, &secondary] {
        assert_eq!(rendered.errors, 0);
        assert_eq!(rendered.underruns, 0);
        assert_eq!(rendered.drops, 0);
    }
}

#[test]
fn push_records_interleaves_large_and_one_ms_pcm_by_dma_time() {
    for explicit_start in [false, true] {
        for first_data_flags in [0, RECORD_FLAG_DISCONTINUITY] {
            assert_separated_endpoints_match_solo(
                PcmRecordLayout::PrimaryWholeThenSecondary {
                    secondary_frames: 48,
                },
                explicit_start,
                first_data_flags,
            );
        }
    }
}

#[test]
fn separated_endpoint_comparison_observes_lfe() {
    let layout = PcmRecordLayout::PrimaryWholeThenSecondary {
        secondary_frames: 48,
    };
    let with_lfe = render_push_records_stream(&timed_pcm_stream_with_lfe(layout, false, 0, 0.5));
    let without_lfe = render_push_records_stream(&timed_pcm_stream_with_lfe(layout, false, 0, 0.0));
    assert_ne!(with_lfe.output, without_lfe.output);
}

#[test]
fn push_record_matches_one_record_push_records_for_fixed_seed_records() {
    let mut seed = 0x4d59_2df4_d0f3_3173_u64;
    for case in 0..24_u64 {
        let mut stream = vec![0; 100_000];
        let mut writer = RecordWriter::new(&mut stream);
        let rate = [44_100, 48_000, 96_000][case as usize % 3];
        let frames = (rate as usize / 100) * (1 + case as usize % 3);
        writer.set_endpoint(ENDPOINT_PRIMARY);
        assert!(writer.format_change(1, 0, &wave_format_bytes(&pcm_format(rate, 1, 0x1))));
        for record in 0..8_u64 {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let value = ((seed >> 40) as i32 - (1 << 23)) as f32 / (1 << 24) as f32;
            let qpc = (record + 1) * frames as u64 * 1_000_000 / u64::from(rate);
            assert!(writer.data(
                1,
                record * frames as u64 * 4,
                qpc,
                &mono_pcm_bytes(value, frames)
            ));
            if record == 3 && case % 2 == 0 {
                assert!(writer.stream_stop(1, qpc));
                assert!(writer.stream_start(1, qpc));
            }
        }
        let len = writer.len();
        stream.truncate(len);
        assert_eq!(
            render_records_one_at_a_time(&stream),
            render_push_records_stream(&stream),
            "case={case} rate={rate} frames={frames}"
        );
    }
}

#[test]
fn repeated_twenty_ms_records_interleave_by_dma_time() {
    for first_data_flags in [0, RECORD_FLAG_DISCONTINUITY] {
        assert_separated_endpoints_match_solo(
            PcmRecordLayout::RepeatedLateTicks {
                frames_per_tick: 960,
            },
            false,
            first_data_flags,
        );
    }
}

#[test]
fn same_read_stop_and_first_data_match_second_endpoint_solo() {
    for first_data_flags in [0, RECORD_FLAG_DISCONTINUITY] {
        let mut stream = vec![0; 16_000];
        let mut writer = RecordWriter::new(&mut stream);
        writer.set_endpoint(ENDPOINT_PRIMARY);
        assert!(writer.format_change(1, 0, &wave_format_bytes(&pcm_format(48_000, 1, 0x1))));
        writer.set_endpoint(ENDPOINT_SECOND);
        assert!(writer.format_change(2, 0, &wave_format_bytes(&pcm_format(48_000, 1, 0x2))));
        writer.set_endpoint(ENDPOINT_PRIMARY);
        assert!(writer.data(1, 0, 2_000, &mono_pcm_bytes(0.125, 96)));
        assert!(writer.stream_stop(1, 2_000));
        writer.set_endpoint(ENDPOINT_SECOND);
        assert!(writer.push(
            RecordHeader {
                kind: bio_abi::RECORD_KIND_DATA,
                flags: first_data_flags,
                epoch: 2,
                byte_pos: 0,
                qpc: 10_000,
                qpc_written: 0,
                len: 0,
                endpoint: ENDPOINT_SECOND,
                reserved: 0,
            },
            &mono_pcm_bytes(0.25, 480),
        ));
        let len = writer.len();
        stream.truncate(len);

        let mixed = render_push_records_stream(&stream);
        let solo = render_push_records_stream(&retain_endpoint_records(&stream, ENDPOINT_SECOND));
        assert_eq!(mixed.output[1], solo.output[1], "flags={first_data_flags}");
        assert_eq!(
            first_nonzero(&mixed.output[1]),
            first_nonzero(&solo.output[1]),
            "flags={first_data_flags}"
        );
        assert_eq!(mixed.underruns, 0, "flags={first_data_flags}");
        assert_eq!(mixed.drops, 0, "flags={first_data_flags}");
    }
}

#[test]
fn last_lane_stop_then_late_start_matches_solo() {
    for (start_qpc, expected_shift, label) in [
        (10_000, 0, "overlapping"),
        (20_000, 480, "ten-ms"),
        (90_000, 480, "long-gap"),
    ] {
        let mut stream = vec![0; 16_000];
        let mut writer = RecordWriter::new(&mut stream);
        writer.set_endpoint(ENDPOINT_PRIMARY);
        assert!(writer.format_change(1, 0, &wave_format_bytes(&pcm_format(48_000, 1, 0x1))));
        writer.set_endpoint(ENDPOINT_SECOND);
        assert!(writer.format_change(2, 0, &wave_format_bytes(&pcm_format(48_000, 1, 0x2))));
        writer.set_endpoint(ENDPOINT_PRIMARY);
        assert!(writer.data(1, 0, 2_000, &mono_pcm_bytes(0.125, 96)));
        assert!(writer.stream_stop(1, 2_000));
        writer.set_endpoint(ENDPOINT_SECOND);
        assert!(writer.data(2, 0, start_qpc, &mono_pcm_bytes(0.25, 480)));
        let len = writer.len();
        stream.truncate(len);

        let mixed = render_push_records_stream(&stream);
        let solo = render_push_records_stream(&retain_endpoint_records(&stream, ENDPOINT_SECOND));
        let mixed_start = first_audible(&mixed.output[1]).expect("B is audible");
        let solo_start = first_audible(&solo.output[1]).expect("solo B is audible");
        assert_eq!(
            &mixed.output[1][mixed_start..],
            &solo.output[1][solo_start..],
            "{label}"
        );
        // B's first byte at 0 ms overlaps A in DMA time and lands where its solo
        // run does. At 10 ms or 80 ms A has stopped and drained, so B starts at
        // the drained boundary instead of reproducing the real-time gap (callers
        // that drive the idle clock get the real-time gap; see
        // idle_progress_places_a_late_start_at_the_anchor_position).
        assert_eq!(mixed_start, solo_start + expected_shift, "{label}");
        assert_eq!(mixed.underruns, 0, "{label}");
        assert_eq!(mixed.drops, 0, "{label}");
    }
}

fn idle_stream() -> Vec<u8> {
    let mut stream = vec![0; 16_000];
    let mut writer = RecordWriter::new(&mut stream);
    writer.set_endpoint(ENDPOINT_PRIMARY);
    assert!(writer.format_change(1, 0, &wave_format_bytes(&pcm_format(48_000, 1, 0x1))));
    writer.set_endpoint(ENDPOINT_SECOND);
    assert!(writer.format_change(2, 0, &wave_format_bytes(&pcm_format(48_000, 1, 0x2))));
    writer.set_endpoint(ENDPOINT_PRIMARY);
    assert!(writer.data(1, 0, 10_000, &mono_pcm_bytes(0.125, 480)));
    assert!(writer.stream_stop(1, 10_000));
    let len = writer.len();
    stream.truncate(len);
    stream
}

#[test]
fn idle_lets_the_brir_tail_play_out() {
    let mut chain = Chain::with_qpc_frequency(ChainConfig::default(), 1_000_000);
    chain.set_prepared(delayed_prepared("idle-tail", 20_000, 19_999));
    let mut active = mono_pcm_bytes(0.0, MIX_FRAMES);
    active[..size_of::<f32>()].copy_from_slice(&1.0_f32.to_le_bytes());
    let mut idle_blocks = Vec::new();
    let mut collect = |event: ChainEvent<'_>| {
        if let ChainEvent::Block(block) = event
            && block.idle
        {
            idle_blocks.push(block.silent);
        }
    };

    chain.push_record(
        Record::FormatChange {
            endpoint: ENDPOINT_PRIMARY,
            epoch: 1,
            repeat: false,
            qpc: 0,
            format: pcm_format(48_000, 1, 0x4),
        },
        &mut collect,
    );
    chain.push_record(
        Record::Data {
            endpoint: ENDPOINT_PRIMARY,
            epoch: 1,
            flags: 0,
            pos: 0,
            qpc: 10_000,
            qpc_written: 0,
            bytes: &active,
        },
        &mut collect,
    );
    chain.push_record(
        Record::StreamStop {
            endpoint: ENDPOINT_PRIMARY,
            epoch: 1,
            repeat: false,
            qpc: 10_000,
        },
        &mut collect,
    );
    chain.idle(600_000, &mut collect);

    assert!(idle_blocks.iter().any(|&silent| !silent));
    let last_audible = idle_blocks
        .iter()
        .rposition(|&silent| !silent)
        .expect("the delayed BRIR tap is audible during idle");
    assert!(idle_blocks[last_audible + 1..].iter().all(|&silent| silent));
}

#[test]
fn first_idle_only_sets_the_anchor() {
    let mut chain = Chain::with_qpc_frequency(ChainConfig::default(), 1_000_000);
    chain.set_prepared(separated_prepared("first-idle-anchor"));
    let mut blocks = 0;
    chain.idle(100_000, &mut |event| {
        blocks += usize::from(matches!(event, ChainEvent::Block(_)));
    });
    assert_eq!(blocks, 0);
}

#[test]
fn idle_is_a_noop_while_a_lane_is_clocked() {
    let mut chain = Chain::with_qpc_frequency(ChainConfig::default(), 1_000_000);
    chain.set_prepared(separated_prepared("clocked-idle"));
    chain.push_records(&idle_stream(), &mut |_| {});
    chain.push_record(
        Record::StreamStart {
            endpoint: ENDPOINT_SECOND,
            epoch: 2,
            repeat: false,
            qpc: 10_000,
        },
        &mut |_| {},
    );
    chain.push_record(
        Record::Data {
            endpoint: ENDPOINT_SECOND,
            epoch: 2,
            flags: 0,
            pos: 0,
            qpc: 20_000,
            qpc_written: 0,
            bytes: &mono_pcm_bytes(0.25, 480),
        },
        &mut |_| {},
    );
    let mut blocks = 0;
    chain.idle(200_000, &mut |event| {
        blocks += usize::from(matches!(event, ChainEvent::Block(_)));
    });
    assert_eq!(blocks, 0);
}

#[test]
fn idle_progress_places_a_late_start_at_the_anchor_position() {
    let run = |idle: bool| {
        let mut chain = Chain::with_qpc_frequency(ChainConfig::default(), 1_000_000);
        chain.set_prepared(separated_prepared("idle-restart-anchor"));
        let mut right = Vec::new();
        let mut collect = |event: ChainEvent<'_>| {
            if let ChainEvent::Block(block) = event {
                right.extend_from_slice(block.channel(1));
            }
        };
        chain.push_records(&idle_stream(), &mut collect);
        if idle {
            chain.idle(50_000, &mut collect);
        }
        chain.push_record(
            Record::Data {
                endpoint: ENDPOINT_SECOND,
                epoch: 2,
                flags: 0,
                pos: 0,
                qpc: 60_000,
                qpc_written: 0,
                bytes: &mono_pcm_bytes(0.25, 480),
            },
            &mut collect,
        );
        chain.finish(&mut collect);
        right
    };

    let with_idle = run(true);
    let without_idle = run(false);
    let idle_start = first_audible(&with_idle).expect("idle restart is audible");
    let no_idle_start = first_audible(&without_idle).expect("offline restart is audible");
    // The last clocked anchor is (10 ms, frame 480). `idle(50 ms)` advances
    // the frontier to frame 2,400. B's first byte is at 50 ms because the DATA
    // timestamp is its 60 ms end, so it starts exactly at that frontier.
    assert!((2_399..=2_401).contains(&idle_start));
    assert_eq!(no_idle_start, 480);
}

#[test]
fn clocked_progress_invalidates_an_older_idle_anchor() {
    let mut chain = Chain::with_qpc_frequency(ChainConfig::default(), 1_000_000);
    chain.set_prepared(separated_prepared("clocked-invalidates-idle"));
    let mut output = [Vec::new(), Vec::new()];
    let mut collect = |event: ChainEvent<'_>| {
        if let ChainEvent::Block(block) = event {
            output[0].extend_from_slice(block.channel(0));
            output[1].extend_from_slice(block.channel(1));
        }
    };

    chain.push_records(&idle_stream(), &mut collect);
    chain.idle(50_000, &mut collect);
    chain.push_record(
        Record::Data {
            endpoint: ENDPOINT_PRIMARY,
            epoch: 1,
            flags: 0,
            pos: 480,
            qpc: 60_000,
            qpc_written: 0,
            bytes: &mono_pcm_bytes(0.125, 480),
        },
        &mut collect,
    );
    chain.push_record(
        Record::StreamStop {
            endpoint: ENDPOINT_PRIMARY,
            epoch: 1,
            repeat: false,
            qpc: 60_000,
        },
        &mut collect,
    );
    chain.push_record(
        Record::Data {
            endpoint: ENDPOINT_SECOND,
            epoch: 2,
            flags: 0,
            pos: 0,
            qpc: 200_000,
            qpc_written: 0,
            bytes: &mono_pcm_bytes(0.25, 480),
        },
        &mut collect,
    );
    chain.finish(&mut collect);

    let right_start = first_audible(&output[1]).expect("B is audible");
    // A's clocked 10 ms update clears the idle-anchor eligibility. B therefore
    // starts at the current six-block frontier, not at its stale 190 ms QPC gap.
    assert_eq!(right_start, 6 * MIX_FRAMES);
}

#[test]
fn stale_start_offset_is_clamped_inside_the_ahead_limit() {
    let mut stream = vec![0; 32_000];
    let mut writer = RecordWriter::new(&mut stream);
    writer.set_endpoint(ENDPOINT_PRIMARY);
    assert!(writer.format_change(1, 0, &wave_format_bytes(&pcm_format(48_000, 1, 0x1))));
    writer.set_endpoint(ENDPOINT_SECOND);
    assert!(writer.format_change(2, 0, &wave_format_bytes(&pcm_format(48_000, 1, 0x2))));
    // A plays 100 ms and stays Active without a stop. B's first byte is 100 ms
    // after A's last DMA time, so the raw QPC offset is 4,800 frames: beyond
    // one chunk plus slack, and beyond the mixer's ahead limit if unclamped.
    writer.set_endpoint(ENDPOINT_PRIMARY);
    assert!(writer.data(1, 0, 100_000, &mono_pcm_bytes(0.125, 4_800)));
    writer.set_endpoint(ENDPOINT_SECOND);
    assert!(writer.data(2, 0, 210_000, &mono_pcm_bytes(0.25, 480)));
    let len = writer.len();
    stream.truncate(len);

    let mixed = render_push_records_stream(&stream);
    let solo = render_push_records_stream(&retain_endpoint_records(&stream, ENDPOINT_SECOND));
    let mixed_start = first_audible(&mixed.output[1]).expect("B is audible");
    let solo_start = first_audible(&solo.output[1]).expect("solo B is audible");
    let solo_end = solo.output[1]
        .iter()
        .rposition(|sample| sample.abs() > 1.0e-4)
        .expect("solo B is audible")
        + 1;
    // `now` is A's 4,800 frames and a forward offset is clamped to one
    // MIX_FRAMES chunk, so B's whole first scene stays on the timeline.
    assert_eq!(mixed_start, solo_start + 4_800 + 480);
    assert_close(
        &mixed.output[1][mixed_start..mixed_start + solo_end - solo_start],
        &solo.output[1][solo_start..solo_end],
        "B",
    );
    assert_eq!(mixed.drops, 0);
    // A stays Active without data, so the interval before B, emitted at its
    // deadline, counts as A's underrun; finish drains B's interval.
    assert_eq!(mixed.underruns, 480);
}

#[test]
fn large_arrival_slack_start_offset_keeps_the_first_scene() {
    let config = ChainConfig {
        arrival_slack_frames: 2_880,
        ..ChainConfig::default()
    };
    let mut stream = vec![0; 16_000];
    let mut writer = RecordWriter::new(&mut stream);
    writer.set_endpoint(ENDPOINT_PRIMARY);
    assert!(writer.format_change(1, 0, &wave_format_bytes(&pcm_format(48_000, 1, 0x1))));
    writer.set_endpoint(ENDPOINT_SECOND);
    assert!(writer.format_change(2, 0, &wave_format_bytes(&pcm_format(48_000, 1, 0x2))));
    // A's 10 ms stays Active. B's first byte is 90 ms later, so its start is
    // clamped to one MIX_FRAMES chunk ahead of `now`, and its first scene then
    // carries the 2,400-frame lane delay this slack gives on top of that.
    writer.set_endpoint(ENDPOINT_PRIMARY);
    assert!(writer.data(1, 0, 10_000, &mono_pcm_bytes(0.125, 480)));
    writer.set_endpoint(ENDPOINT_SECOND);
    assert!(writer.data(2, 0, 110_000, &mono_pcm_bytes(0.25, 480)));
    let len = writer.len();
    stream.truncate(len);

    let mixed = render_push_records_stream_with(&stream, config);
    let solo =
        render_push_records_stream_with(&retain_endpoint_records(&stream, ENDPOINT_SECOND), config);
    assert_eq!(mixed.drops, 0);
    assert_eq!(solo.drops, 0);
    let mixed_start = first_audible(&mixed.output[1]).expect("B is audible");
    let solo_start = first_audible(&solo.output[1]).expect("solo B is audible");
    let solo_end = solo.output[1]
        .iter()
        .rposition(|sample| sample.abs() > 1.0e-4)
        .expect("solo B is audible")
        + 1;
    assert_eq!(mixed_start, solo_start + 480 + 480);
    assert_close(
        &mixed.output[1][mixed_start..mixed_start + solo_end - solo_start],
        &solo.output[1][solo_start..solo_end],
        "B",
    );
}

#[test]
fn restart_after_a_stall_with_the_largest_slack_keeps_its_first_scene() {
    let config = ChainConfig {
        arrival_slack_frames: 9_600,
        ..ChainConfig::default()
    };
    let mut stream = vec![0; 240_000];
    let mut writer = RecordWriter::new(&mut stream);
    writer.set_endpoint(ENDPOINT_PRIMARY);
    assert!(writer.format_change(1, 0, &wave_format_bytes(&pcm_format(48_000, 1, 0x1))));
    writer.set_endpoint(ENDPOINT_SECOND);
    assert!(writer.format_change(2, 0, &wave_format_bytes(&pcm_format(48_000, 1, 0x2))));
    // B sends 10 ms and then nothing, without a stop, while A plays 1 s. B
    // comes back at 1.3 s with a discontinuity, 300 ms past A's last DMA time.
    writer.set_endpoint(ENDPOINT_SECOND);
    assert!(writer.data(2, 0, 10_000, &mono_pcm_bytes(0.25, 480)));
    writer.set_endpoint(ENDPOINT_PRIMARY);
    for tick in 0..100_u64 {
        assert!(writer.data(
            1,
            tick * 1_920,
            (tick + 1) * 10_000,
            &mono_pcm_bytes(0.125, 480)
        ));
    }
    writer.set_endpoint(ENDPOINT_SECOND);
    assert!(writer.push(
        RecordHeader {
            kind: bio_abi::RECORD_KIND_DATA,
            flags: RECORD_FLAG_DISCONTINUITY,
            epoch: 2,
            byte_pos: 1_920,
            qpc: 1_310_000,
            qpc_written: 0,
            len: 0,
            endpoint: ENDPOINT_SECOND,
            reserved: 0,
        },
        &mono_pcm_bytes(0.375, 480),
    ));
    let len = writer.len();
    stream.truncate(len);

    let mixed = render_push_records_stream_with(&stream, config);
    assert_eq!(mixed.drops, 0);
    // The restart is placed one chunk past `now` (48,000) plus the lane delay
    // the 200 ms slack gives (9,600 - 480), and every frame of it is heard.
    let restart = 48_000 + 480 + 9_120;
    let right = &mixed.output[1];
    assert!(right.len() >= restart + 480);
    assert!(
        right[..restart]
            .iter()
            .all(|sample| (sample - 0.375).abs() > 1.0e-3),
        "the restart does not begin early"
    );
    assert_close(&right[restart..restart + 480], &[0.375; 480], "restart");
}

#[test]
fn an_idle_lanes_estimated_origin_does_not_hold_back_its_later_start() {
    let mut stream = vec![0; 32_000];
    let mut writer = RecordWriter::new(&mut stream);
    writer.set_endpoint(ENDPOINT_PRIMARY);
    assert!(writer.format_change(1, 0, &wave_format_bytes(&pcm_format(48_000, 1, 0x1))));
    assert!(writer.data(1, 0, 100_000, &mono_pcm_bytes(0.125, 4_800)));
    // B's format arrives 10 ms after A's last data while A is still Active, so
    // B's idle origin is estimated one chunk ahead before any byte of B.
    writer.set_endpoint(ENDPOINT_SECOND);
    assert!(writer.format_change(2, 110_000, &wave_format_bytes(&pcm_format(48_000, 1, 0x2))));
    // A stops and drains; B then starts alone and continues at the boundary.
    writer.set_endpoint(ENDPOINT_PRIMARY);
    assert!(writer.stream_stop(1, 120_000));
    writer.set_endpoint(ENDPOINT_SECOND);
    assert!(writer.stream_start(2, 130_000));
    assert!(writer.data(2, 0, 140_000, &mono_pcm_bytes(0.25, 480)));
    let len = writer.len();
    stream.truncate(len);

    let mixed = render_push_records_stream(&stream);
    let solo = render_push_records_stream(&retain_endpoint_records(&stream, ENDPOINT_SECOND));
    let mixed_start = first_audible(&mixed.output[1]).expect("B is audible");
    let solo_start = first_audible(&solo.output[1]).expect("solo B is audible");
    assert_eq!(mixed_start, solo_start + 4_800);
    assert_eq!(mixed.drops, 0);
    assert_eq!(mixed.underruns, 0);
}

#[test]
fn a_format_change_after_stop_keeps_the_stopped_streams_end() {
    let mut stream = vec![0; 64_000];
    let mut writer = RecordWriter::new(&mut stream);
    writer.set_endpoint(ENDPOINT_PRIMARY);
    assert!(writer.format_change(1, 0, &wave_format_bytes(&pcm_format(48_000, 1, 0x1))));
    writer.set_endpoint(ENDPOINT_SECOND);
    assert!(writer.format_change(2, 0, &wave_format_bytes(&pcm_format(48_000, 1, 0x2))));
    for tick in 0..10_u64 {
        let end = (tick + 1) * 10_000;
        // B's ten milliseconds reach the chain just before A's, so B's last
        // interval is still waiting for A when B stops and changes format.
        writer.set_endpoint(ENDPOINT_SECOND);
        if tick < 5 {
            assert!(writer.data(2, tick * 1_920, end, &mono_pcm_bytes(0.25, 480)));
            if tick == 4 {
                assert!(writer.stream_stop(2, end));
                assert!(writer.format_change(
                    3,
                    end,
                    &wave_format_bytes(&pcm_format(48_000, 1, 0x2))
                ));
                assert!(writer.stream_start(3, end));
            }
        } else {
            assert!(writer.data(3, (tick - 5) * 1_920, end, &mono_pcm_bytes(0.375, 480)));
        }
        writer.set_endpoint(ENDPOINT_PRIMARY);
        assert!(writer.data(1, tick * 1_920, end + 1, &mono_pcm_bytes(0.125, 480)));
    }
    let len = writer.len();
    stream.truncate(len);

    let mixed = render_push_records_stream(&stream);
    assert_eq!(mixed.drops, 0);
    // B (FR) is alone in the right ear: every frame of both streams is heard.
    let count = |value: f32| {
        mixed.output[1]
            .iter()
            .filter(|sample| (*sample - value).abs() <= 1.0e-4)
            .count()
    };
    assert_eq!(count(0.25), 5 * 480);
    assert_eq!(count(0.375), 5 * 480);
}

#[test]
fn nonempty_discontinuity_does_not_move_other_endpoint() {
    let mut stream = vec![0; 100_000];
    let mut writer = RecordWriter::new(&mut stream);
    writer.set_endpoint(ENDPOINT_PRIMARY);
    assert!(writer.format_change(1, 0, &wave_format_bytes(&pcm_format(48_000, 1, 0x1))));
    assert!(writer.stream_start(1, 0));
    writer.set_endpoint(ENDPOINT_SECOND);
    assert!(writer.format_change(2, 0, &wave_format_bytes(&pcm_format(48_000, 1, 0x2))));
    assert!(writer.stream_start(2, 0));

    for ten_ms in 0..10_u64 {
        writer.set_endpoint(ENDPOINT_PRIMARY);
        assert!(writer.data(
            1,
            ten_ms * 480 * 4,
            (ten_ms + 1) * 10_000,
            &mono_pcm_bytes(0.125, 480),
        ));
        for ms in ten_ms * 10..(ten_ms + 1) * 10 {
            writer.set_endpoint(ENDPOINT_SECOND);
            assert!(writer.data(2, ms * 48 * 4, (ms + 1) * 1_000, &mono_pcm_bytes(0.25, 48),));
        }
    }
    for ms in 100..109_u64 {
        writer.set_endpoint(ENDPOINT_SECOND);
        assert!(writer.data(2, ms * 48 * 4, (ms + 1) * 1_000, &mono_pcm_bytes(0.25, 48),));
    }
    writer.set_endpoint(ENDPOINT_PRIMARY);
    assert!(writer.push(
        RecordHeader {
            kind: bio_abi::RECORD_KIND_DATA,
            flags: RECORD_FLAG_DISCONTINUITY,
            epoch: 1,
            byte_pos: 100 * 48 * 4,
            qpc: 110_000,
            qpc_written: 0,
            len: 0,
            endpoint: ENDPOINT_PRIMARY,
            reserved: 0,
        },
        &mono_pcm_bytes(0.125, 480),
    ));
    writer.set_endpoint(ENDPOINT_SECOND);
    assert!(writer.data(2, 109 * 48 * 4, 110_000, &mono_pcm_bytes(0.25, 48),));
    let len = writer.len();
    stream.truncate(len);

    let mixed = render_push_records_stream(&stream);
    let solo = render_push_records_stream(&retain_endpoint_records(&stream, ENDPOINT_SECOND));
    assert_eq!(mixed.output[1], solo.output[1]);
    assert_eq!(
        first_nonzero(&mixed.output[1]),
        first_nonzero(&solo.output[1])
    );
    assert_eq!(mixed.underruns, 0);
    // The discontinuity keeps the 64-frame fade and counts the filled suffix it
    // cuts from the old generation. B's exact solo equality above is unchanged.
    assert_eq!(mixed.drops, 480 - 64);
}

#[test]
fn filtered_endpoint_does_not_create_a_start_step() {
    let mut stream = vec![0; 16_000];
    let mut writer = RecordWriter::new(&mut stream);
    writer.set_endpoint(ENDPOINT_PRIMARY);
    assert!(writer.format_change(1, 0, &wave_format_bytes(&pcm_format(48_000, 1, 0x1))));
    writer.set_endpoint(ENDPOINT_SECOND);
    assert!(writer.format_change(2, 0, &wave_format_bytes(&pcm_format(48_000, 1, 0x2))));
    assert!(writer.data(2, 0, 1_000, &mono_pcm_bytes(0.5, 48)));
    writer.set_endpoint(ENDPOINT_PRIMARY);
    assert!(writer.data(1, 0, 10_000, &mono_pcm_bytes(0.25, 480)));
    assert!(writer.stream_stop(1, 10_000));
    let len = writer.len();
    stream.truncate(len);

    let render = |stream: &[u8]| {
        let mut chain = Chain::with_qpc_frequency(
            ChainConfig {
                endpoints: engine::EndpointFilter::Only(ENDPOINT_PRIMARY),
                ..ChainConfig::default()
            },
            1_000_000,
        );
        chain.set_prepared(separated_prepared("filtered-start-step"));
        let mut output = Vec::new();
        let mut collect = |event: ChainEvent<'_>| {
            if let ChainEvent::Block(block) = event {
                output.extend_from_slice(block.channel(0));
            }
        };
        chain.push_records(stream, &mut collect);
        output
    };
    let mixed = render(&stream);
    let solo = render(&retain_endpoint_records(&stream, ENDPOINT_PRIMARY));
    assert_eq!(mixed, solo);
}

#[test]
fn low_rate_pcm_chunking_always_makes_progress() {
    // Deliberately unsupported: 50 Hz makes byte_rate / 100 smaller than one
    // PCM frame. The scheduler must still consume the record, not loop forever.
    let format = pcm_format(50, 2, 0x3);
    let bytes = pcm_bytes(0.25, 1);
    let mut stream = vec![0; 512];
    let mut writer = RecordWriter::new(&mut stream);
    writer.set_endpoint(ENDPOINT_PRIMARY);
    assert!(writer.format_change(1, 0, &wave_format_bytes(&format)));
    assert!(writer.data(1, 0, 20_000, &bytes));
    let len = writer.len();
    stream.truncate(len);

    let mut chain = Chain::with_qpc_frequency(ChainConfig::default(), 1_000_000);
    let mut errors = 0;
    chain.push_records(&stream, &mut |event| {
        if let ChainEvent::Error { endpoint, error } = event {
            assert_eq!(endpoint, Some(ENDPOINT_PRIMARY));
            assert_eq!(error, engine::ChainError::UnsupportedSceneRate(50));
            errors += 1;
        }
    });
    assert_eq!(errors, 1);
}

#[test]
fn zero_and_backward_qpc_records_are_deterministic() {
    let primary_format = pcm_format(48_000, 1, 0x1);
    let secondary_format = pcm_format(48_000, 1, 0x2);
    let primary = mono_pcm_bytes(0.125, 960);
    let secondary = mono_pcm_bytes(0.25, 480);
    let mut stream = vec![0; 32_000];
    let mut writer = RecordWriter::new(&mut stream);
    writer.set_endpoint(ENDPOINT_PRIMARY);
    assert!(writer.format_change(1, 0, &wave_format_bytes(&primary_format)));
    writer.set_endpoint(ENDPOINT_SECOND);
    assert!(writer.format_change(2, 0, &wave_format_bytes(&secondary_format)));
    writer.set_endpoint(ENDPOINT_PRIMARY);
    assert!(writer.data(1, 0, 0, &primary));
    writer.set_endpoint(ENDPOINT_SECOND);
    assert!(writer.data(2, 0, 20_000, &secondary));
    assert!(writer.data(2, secondary.len() as u64, 10_000, &secondary));
    let len = writer.len();
    stream.truncate(len);

    let first = render_push_records_stream(&stream);
    let second = render_push_records_stream(&stream);
    assert_eq!(first, second);
    assert!(first.output.iter().all(|channel| !channel.is_empty()));
    assert_eq!(first.errors, 0);
}

#[test]
fn malformed_records_report_once_and_other_endpoint_continues() {
    let format = stereo_pcm_format();
    let valid = pcm_bytes(0.25, 480);
    let header = |endpoint, kind| RecordHeader {
        kind,
        flags: 0,
        epoch: 1,
        byte_pos: 0,
        qpc: 0,
        qpc_written: 0,
        len: 0,
        endpoint,
        reserved: 0,
    };
    let mut stream = vec![0; 16_000];
    let mut writer = RecordWriter::new(&mut stream);
    writer.set_endpoint(ENDPOINT_SECOND);
    assert!(writer.format_change(2, 0, &wave_format_bytes(&format)));
    assert!(writer.push(header(ENDPOINT_PRIMARY, RECORD_KIND_STREAM_STOP), &[1]));
    assert!(writer.push(header(ENDPOINT_PRIMARY, RECORD_KIND_FORMAT_CHANGE), &[0]));
    assert!(writer.push(header(MAX_ENDPOINTS as u16, RECORD_KIND_STREAM_STOP), &[1]));
    writer.set_endpoint(ENDPOINT_SECOND);
    assert!(writer.data(2, 0, 10_000, &valid));
    writer.set_endpoint(ENDPOINT_PRIMARY);
    assert!(writer.data(1, 0, 20_000, &[0; 8]));
    let len = writer.len();
    stream.truncate(len - 1);

    let mut chain = Chain::with_qpc_frequency(ChainConfig::default(), 1_000_000);
    chain.set_prepared(identity_prepared("malformed-records"));
    let mut bad_length = 0;
    let mut bad_format = 0;
    let mut truncated = 0;
    let mut secondary_formats = 0;
    let mut samples = 0;
    let mut collect = |event: ChainEvent<'_>| match event {
        ChainEvent::FormatChanged {
            endpoint: ENDPOINT_SECOND,
            ..
        } => secondary_formats += 1,
        ChainEvent::Block(block) => samples += block.frames,
        ChainEvent::Error {
            error: engine::ChainError::Record(AbiError::BadLength { .. }),
            ..
        } => bad_length += 1,
        ChainEvent::Error {
            error: engine::ChainError::Record(AbiError::BadFormat { .. }),
            ..
        } => bad_format += 1,
        ChainEvent::Error {
            error: engine::ChainError::Record(AbiError::Truncated { .. }),
            ..
        } => truncated += 1,
        _ => {}
    };
    chain.push_records(&stream, &mut collect);
    chain.finish(&mut collect);
    drop(collect);

    assert_eq!(bad_length, 1);
    assert_eq!(bad_format, 1);
    assert_eq!(truncated, 1);
    assert_eq!(chain.skipped_records(), 1);
    assert_eq!(secondary_formats, 1);
    assert!(samples > 480, "output tail was not appended");
}

#[test]
fn unknown_kind_is_not_allowed_to_consume_the_next_record() {
    let format = pcm_format(48_000, 1, 0x1);
    let samples = mono_pcm_bytes(0.25, 480);
    let unknown = RecordHeader {
        kind: u16::MAX,
        flags: 0,
        epoch: 1,
        byte_pos: 0,
        qpc: 0,
        qpc_written: 0,
        len: 0,
        endpoint: ENDPOINT_PRIMARY,
        reserved: 0,
    };
    let mut stream = vec![0; 16_000];
    let mut writer = RecordWriter::new(&mut stream);
    writer.set_endpoint(ENDPOINT_SECOND);
    assert!(writer.format_change(2, 0, &wave_format_bytes(&format)));
    assert!(writer.push(unknown, &[]));
    assert!(writer.data(2, 0, 10_000, &samples));
    let len = writer.len();
    stream.truncate(len);
    let rendered = render_push_records_stream(&stream);
    assert_eq!(rendered.errors, 0);
    assert!(rendered.output[0].len() > 480);
    assert!(rendered.output[0].iter().any(|sample| *sample != 0.0));

    let mut malformed = vec![0; 1_024];
    let mut writer = RecordWriter::new(&mut malformed);
    assert!(writer.push(unknown, &[]));
    writer.set_endpoint(ENDPOINT_SECOND);
    assert!(writer.push(
        RecordHeader {
            kind: RECORD_KIND_FORMAT_CHANGE,
            endpoint: ENDPOINT_SECOND,
            ..unknown
        },
        &[0],
    ));
    let len = writer.len();
    malformed.truncate(len);
    let rendered = render_push_records_stream(&malformed);
    assert_eq!(rendered.errors, 1);
}

#[test]
fn unknown_kind_does_not_end_its_endpoints_records() {
    let unknown = |endpoint| RecordHeader {
        kind: u16::MAX,
        flags: 0,
        epoch: 1,
        byte_pos: 0,
        qpc: 0,
        qpc_written: 0,
        len: 0,
        endpoint,
        reserved: 0,
    };
    let mut stream = vec![0; 16_000];
    let mut writer = RecordWriter::new(&mut stream);
    writer.set_endpoint(ENDPOINT_PRIMARY);
    assert!(writer.format_change(1, 0, &wave_format_bytes(&pcm_format(48_000, 1, 0x1))));
    writer.set_endpoint(ENDPOINT_SECOND);
    assert!(writer.format_change(2, 0, &wave_format_bytes(&pcm_format(48_000, 1, 0x2))));
    // An unknown kind on endpoint 0, then endpoint 0's own data.
    writer.set_endpoint(ENDPOINT_PRIMARY);
    assert!(writer.push(unknown(ENDPOINT_PRIMARY), &[]));
    assert!(writer.data(1, 0, 10_000, &mono_pcm_bytes(0.125, 480)));
    // An unknown kind past the endpoint range shares the last cursor with
    // endpoint 1, then endpoint 1's own data.
    let outside = MAX_ENDPOINTS as u16 + 3;
    writer.set_endpoint(outside);
    assert!(writer.push(unknown(outside), &[]));
    writer.set_endpoint(ENDPOINT_SECOND);
    assert!(writer.data(2, 0, 10_000, &mono_pcm_bytes(0.25, 480)));
    let len = writer.len();
    stream.truncate(len);

    let rendered = render_push_records_stream(&stream);
    assert_eq!(rendered.errors, 0);
    assert!(rendered.output[0].len() > MIX_FRAMES);
    assert_eq!(
        rendered.output[0]
            .iter()
            .filter(|sample| sample.abs() > 0.1)
            .count(),
        MIX_FRAMES,
        "endpoint 0's data after its unknown record is rendered exactly once"
    );
    assert_eq!(
        rendered.output[1]
            .iter()
            .filter(|sample| sample.abs() > 0.2)
            .count(),
        MIX_FRAMES,
        "endpoint 1's data after the out-of-range unknown record is rendered exactly once"
    );
}

#[test]
fn pcm_chunk_source_qpc_is_each_chunks_first_byte_time() {
    let input = pcm_bytes(0.25, 4_800);
    let mut chain = Chain::with_qpc_frequency(
        ChainConfig {
            arrival_slack_frames: 0,
            ..ChainConfig::default()
        },
        1_000_000,
    );
    chain.push_record(
        Record::FormatChange {
            endpoint: ENDPOINT_PRIMARY,
            epoch: 1,
            repeat: false,
            qpc: 0,
            format: stereo_pcm_format(),
        },
        &mut |_| {},
    );
    chain.set_prepared(identity_prepared("chunk-source-qpc"));
    let mut qpcs = Vec::new();
    chain.push_record(
        Record::Data {
            endpoint: ENDPOINT_PRIMARY,
            epoch: 1,
            flags: 0,
            pos: 0,
            qpc: 100_000,
            qpc_written: 0,
            bytes: &input,
        },
        &mut |event| {
            if let ChainEvent::Block(block) = event {
                qpcs.push(block.source_qpc);
            }
        },
    );
    assert_eq!(
        qpcs,
        (0..10).map(|index| index * 10_000).collect::<Vec<_>>()
    );
}

#[test]
fn zero_rate_bitstream_burst_qpc_does_not_panic() {
    let stream = SourceFormat::Bitstream(BitstreamFormat {
        codec: BitstreamCodec::Eac3,
        container_rate: 0,
        container_channels: 2,
        encoded_rate: 48_000,
        encoded_channels: 0,
    });
    let mut burst = Vec::new();
    bitstream::frame_burst(bitstream::DataType::Eac3, &[0; 16], 64, &mut burst);
    let mut chain = Chain::new(ChainConfig::default());
    chain.push_record(
        Record::FormatChange {
            endpoint: ENDPOINT_PRIMARY,
            epoch: 1,
            repeat: false,
            qpc: 1,
            format: stream,
        },
        &mut |_| {},
    );
    chain.push_record(
        Record::Data {
            endpoint: ENDPOINT_PRIMARY,
            epoch: 1,
            flags: 0,
            pos: 0,
            qpc: 2,
            qpc_written: 0,
            bytes: &burst,
        },
        &mut |_| {},
    );
}

#[test]
fn bitstream_source_qpc_reanchors_to_later_record_observations() {
    let fixture = std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../decode/tests/fixtures/ddp/tones-5.1.eac3"),
    )
    .unwrap();
    let mut carrier = Vec::new();
    for payload in fixture[..5120].chunks(2560) {
        let mut burst = Vec::new();
        bitstream::frame_burst(bitstream::DataType::Eac3, payload, 24_576, &mut burst);
        carrier.extend_from_slice(&burst);
    }
    // One QPC tick per carrier byte makes the expected Pa times exact. The
    // second burst straddles a preamble and payload cut; its completion record
    // observes 424 more ticks than extrapolation from the first record predicts.
    let mut stream = vec![0; carrier.len() + 1024];
    let mut writer = RecordWriter::new(&mut stream);
    assert!(writer.format_change(
        1,
        0,
        &wave_format_bytes(&bitstream_format(BitstreamCodec::Eac3, 48_000))
    ));
    for (start, end, correction) in [
        (0, 24_576, 0),
        (24_576, 24_579, 0),
        (24_579, 25_576, 0),
        (25_576, 49_152, 424),
    ] {
        assert!(writer.data(
            1,
            start as u64,
            100_000 + end as u64 + correction,
            &carrier[start..end]
        ));
    }
    let len = writer.len();
    stream.truncate(len);
    let mut chain = Chain::with_qpc_frequency(ChainConfig::default(), 768_000);
    chain.set_prepared(identity_prepared("bitstream-record-qpc"));
    let mut qpcs = Vec::new();
    let mut collect = |event: ChainEvent<'_>| match event {
        ChainEvent::Block(block) => qpcs.push(block.source_qpc),
        ChainEvent::Error { error, .. } => panic!("unexpected error: {error}"),
        _ => {}
    };
    chain.push_records(&stream, &mut collect);
    chain.finish(&mut collect);
    assert_eq!(
        &qpcs[..7],
        [
            100_000, 107_680, 115_360, 123_040, 131_144, 138_824, 146_504
        ]
    );
    assert!(qpcs[7..].iter().all(|qpc| *qpc == 0));
    assert_eq!(chain.lane_underrun_frames(), 0);
    assert_eq!(chain.dropped_frames(), 0);
}

#[test]
fn stream_stop_then_eof_matches_stream_stop_for_fir_pcm() {
    let input = pcm_bytes(0.25, 441);
    let run = |finish_after_stop: bool| {
        let mut chain = Chain::new(ChainConfig::default());
        chain.set_prepared(identity_prepared("fir-stop-eof"));
        chain.push_record(
            Record::FormatChange {
                endpoint: ENDPOINT_PRIMARY,
                epoch: 1,
                repeat: false,
                qpc: 0,
                format: pcm_format(44_100, 2, 3),
            },
            &mut |_| {},
        );
        chain.push_record(
            Record::Data {
                endpoint: ENDPOINT_PRIMARY,
                epoch: 1,
                flags: 0,
                pos: 0,
                qpc: 100_000,
                qpc_written: 0,
                bytes: &input,
            },
            &mut |_| {},
        );
        let mut blocks = Vec::new();
        chain.push_record(
            Record::StreamStop {
                endpoint: ENDPOINT_PRIMARY,
                epoch: 1,
                repeat: false,
                qpc: 100_000,
            },
            &mut |event| {
                if let ChainEvent::Block(block) = event {
                    blocks.push((
                        block.epoch,
                        block.channel(0).to_vec(),
                        block.channel(1).to_vec(),
                    ));
                }
            },
        );
        if finish_after_stop {
            chain.finish(&mut |event| {
                if let ChainEvent::Block(block) = event {
                    blocks.push((
                        block.epoch,
                        block.channel(0).to_vec(),
                        block.channel(1).to_vec(),
                    ));
                }
            });
        }
        blocks
    };

    let stopped = run(false);
    let stopped_then_eof = run(true);
    assert!(!stopped.is_empty());
    assert_eq!(&stopped_then_eof[..stopped.len()], stopped);
    assert!(stopped_then_eof.len() > stopped.len());
}

#[test]
fn pcm_residuals_survive_stream_stop_and_eof() {
    for frames in [96, 479, 481, 576] {
        let input = pcm_bytes(0.25, frames);
        let stopped = render_pcm_chunks(&[&input], true);
        let eof = render_pcm_chunks(&[&input], false);
        assert_eq!(&eof[..stopped.len()], stopped, "frames={frames}");
        assert!(eof.len() > stopped.len(), "frames={frames}");
        let emitted = stopped
            .iter()
            .flat_map(|(left, right)| left.iter().chain(right))
            .filter(|sample| **sample != 0.0)
            .count();
        assert!(emitted >= frames * 2, "frames={frames} emitted={emitted}");
    }
}

#[test]
fn eighty_eight_two_khz_pcm_endpoint_produces_audio() {
    let rate = 88_200;
    let frames = rate as usize / 100;
    let mut chain = Chain::new(ChainConfig {
        arrival_slack_frames: 0,
        ..ChainConfig::default()
    });
    chain.set_prepared(identity_prepared("pcm-88-2"));
    chain.push_record(
        Record::FormatChange {
            endpoint: ENDPOINT_PRIMARY,
            epoch: 1,
            repeat: false,
            qpc: 0,
            format: pcm_format(rate, 2, 3),
        },
        &mut |_| {},
    );
    let bytes = pcm_bytes(0.25, frames);
    let mut audible = false;
    chain.push_record(
        Record::Data {
            endpoint: ENDPOINT_PRIMARY,
            epoch: 1,
            flags: 0,
            pos: 0,
            qpc: 100_000,
            qpc_written: 0,
            bytes: &bytes,
        },
        &mut |event| {
            if let ChainEvent::Block(block) = event {
                audible |= block.channel(0).iter().any(|sample| sample.abs() > 0.01);
            }
        },
    );
    chain.finish(&mut |event| {
        if let ChainEvent::Block(block) = event {
            audible |= block.channel(0).iter().any(|sample| sample.abs() > 0.01);
        }
    });
    assert!(audible);
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
    let mut chain = Chain::new(ChainConfig {
        arrival_slack_frames: 0,
        ..ChainConfig::default()
    });
    chain.set_prepared(Box::new(Prepared::new(Arc::new(prepared), 480).unwrap()));

    let mut blocks = 0;
    chain.push_records(&pcm_stream(1), &mut |event| {
        if let ChainEvent::Block(block) = event {
            blocks += 1;
            assert_eq!((block.epoch, block.rate, block.source_qpc), (1, 48_000, 10));
            assert_eq!(block.frames, 480);
            let expected_left = [(0, 0.2), (2, 0.1)];
            let expected_right = [(1, 0.1), (3, 0.05)];
            for (frame, &sample) in block.channel(0).iter().enumerate() {
                let expected = expected_left
                    .iter()
                    .find(|(at, _)| *at == frame)
                    .map_or(0.0, |(_, value)| *value);
                assert!(
                    (sample - expected).abs() < 2.0e-5,
                    "left frame {frame}: {sample} != {expected}"
                );
            }
            for (frame, &sample) in block.channel(1).iter().enumerate() {
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
    let prepared = brir::prepare(&profile, 48_000, partition_for(480)).unwrap();
    let mut chain = Chain::new(ChainConfig::default());
    chain.set_prepared(Box::new(Prepared::new(Arc::new(prepared), 480).unwrap()));

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
            assert_eq!(block.frames, 480);
            assert!(
                block
                    .channel(0)
                    .iter()
                    .map(|sample| sample.abs())
                    .sum::<f32>()
                    > 0.01
            );
            assert!(
                block
                    .channel(1)
                    .iter()
                    .map(|sample| sample.abs())
                    .sum::<f32>()
                    > 0.01
            );
        }
        ChainEvent::Error { .. } => errors += 1,
        _ => {}
    });
    assert_eq!(blocks, 3);
    assert_eq!(errors, 0);
}

#[test]
fn ddp_and_speaker_paths_apply_the_same_volume_gain() {
    let fixture = std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../decode/tests/fixtures/ddp/tones-5.1.eac3"),
    )
    .unwrap();
    let mut burst = Vec::new();
    bitstream::frame_burst(
        bitstream::DataType::Eac3,
        &fixture[..2560],
        24_576,
        &mut burst,
    );

    let render = |prepared: Box<Prepared>, volume_db: Option<i32>| {
        let mut chain = Chain::new(ChainConfig::default());
        chain.set_prepared(prepared);
        let mut peak = 0.0_f32;
        if let Some(volume_db) = volume_db {
            let mut levels = [0; bio_abi::MAX_VOLUME_CHANNELS];
            levels[..2].fill(volume_db * 65_536);
            chain.push_record(
                Record::Volume {
                    endpoint: ENDPOINT_PRIMARY,
                    repeat: false,
                    qpc: 0,
                    channel_count: 2,
                    mute_mask: 0,
                    levels,
                },
                &mut |_| {},
            );
        }
        chain.push_record(
            Record::FormatChange {
                endpoint: ENDPOINT_PRIMARY,
                epoch: 7,
                repeat: false,
                qpc: 0,
                format: bitstream_format(BitstreamCodec::Eac3, 48_000),
            },
            &mut |_| {},
        );
        chain.push_record(
            Record::Data {
                endpoint: ENDPOINT_PRIMARY,
                epoch: 7,
                flags: 0,
                pos: 0,
                qpc: 123,
                qpc_written: 0,
                bytes: &burst,
            },
            &mut |event| {
                if let ChainEvent::Block(block) = event {
                    peak = peak.max(
                        block
                            .samples
                            .iter()
                            .skip(64)
                            .map(|sample| sample.abs())
                            .fold(0.0_f32, f32::max),
                    );
                }
            },
        );
        peak
    };

    let ddp_unity = render(identity_prepared("ddp-volume-unity"), None);
    let ddp_quiet = render(identity_prepared("ddp-volume-quiet"), Some(-20));
    let speaker_unity = render(identity_speaker_prepared(), None);
    let speaker_quiet = render(identity_speaker_prepared(), Some(-20));
    assert!(ddp_unity > 0.0 && speaker_unity > 0.0);
    let ddp_ratio = ddp_quiet / ddp_unity;
    let speaker_ratio = speaker_quiet / speaker_unity;
    assert!((ddp_ratio - 0.1).abs() < 0.01, "DD+ ratio was {ddp_ratio}");
    assert!(
        (speaker_ratio - 0.1).abs() < 0.01,
        "speaker ratio was {speaker_ratio}"
    );
    assert!(
        (ddp_ratio - speaker_ratio).abs() < 0.001,
        "DD+ ratio {ddp_ratio} != speaker ratio {speaker_ratio}"
    );
}

#[test]
fn iec_192_khz_transport_time_comes_from_carrier_bytes_not_decoded_frames() {
    let fixture = std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../decode/tests/fixtures/ddp/tones-5.1.eac3"),
    )
    .unwrap();
    let mut burst = Vec::new();
    bitstream::frame_burst(
        bitstream::DataType::Eac3,
        &fixture[..2560],
        24_576,
        &mut burst,
    );
    assert_eq!(burst.len(), 24_576);

    let temp = TempDir::new("iec-byte-time");
    let ir = temp.0.join("FC.wav");
    write_stereo_ir(&ir, 1, 0, 1.0, 0, 1.0);
    let prepared = brir::prepare(
        &profile(
            vec![DirectionFile {
                name: "FC".into(),
                azimuth_deg: 0.0,
                elevation_deg: 0.0,
                path: ir,
            }],
            0.0,
            0.0,
        ),
        48_000,
        480,
    )
    .unwrap();
    let mut chain = Chain::new(ChainConfig {
        arrival_slack_frames: 0,
        ..ChainConfig::default()
    });
    chain.set_prepared(Box::new(Prepared::new(Arc::new(prepared), 480).unwrap()));
    chain.push_record(
        Record::FormatChange {
            endpoint: ENDPOINT_PRIMARY,
            epoch: 7,
            repeat: false,
            qpc: 0,
            format: bitstream_format(BitstreamCodec::Eac3, 48_000),
        },
        &mut |_| {},
    );

    let mut blocks = 0;
    for (index, chunk) in burst.chunks(7_680).enumerate() {
        chain.push_record(
            Record::Data {
                endpoint: ENDPOINT_PRIMARY,
                epoch: 7,
                flags: 0,
                pos: (index * 7_680) as u64,
                qpc: index as u64 * 100_000,
                qpc_written: 0,
                bytes: chunk,
            },
            &mut |event| {
                if matches!(event, ChainEvent::Block(_)) {
                    blocks += 1;
                }
            },
        );
    }

    // A 192 kHz stereo 16-bit carrier is 768,000 bytes/s, so this period is
    // exactly 1,536 timeline frames and emits three complete 480-frame slots.
    // Advancing by decoded output as well would emit six before finish.
    assert_eq!(blocks, 3);
    chain.finish(&mut |event| {
        if matches!(event, ChainEvent::Block(_)) {
            blocks += 1;
        }
    });
    assert!(blocks > 4, "output tail was not appended");
}

#[test]
fn real_ddp_and_pcm_match_their_solo_sum_at_twenty_fixed_seed_phases() {
    let temp = TempDir::new("ddp-pcm-phases");
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
    let files = directions
        .into_iter()
        .enumerate()
        .map(|(index, (name, azimuth))| {
            let path = temp.0.join(format!("{name}.wav"));
            write_stereo_ir(&path, 16, index, 0.01, 15 - index, 0.01);
            DirectionFile {
                name: name.into(),
                azimuth_deg: azimuth,
                elevation_deg: 0.0,
                path,
            }
        })
        .collect();
    let mut profile = profile(files, 0.0, 0.0);
    profile.lfe = LfePolicy::Direction {
        name: "FC".into(),
        gain_db: 0.0,
    };
    let prepared = Arc::new(brir::prepare(&profile, 48_000, 480).unwrap());

    let fixture = std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../decode/tests/fixtures/ddp/tones-5.1.eac3"),
    )
    .unwrap();
    let mut burst = Vec::new();
    bitstream::frame_burst(
        bitstream::DataType::Eac3,
        &fixture[..2560],
        24_576,
        &mut burst,
    );
    let pcm = pcm_bytes(0.001, 1_536);

    let render = |include_ddp: bool, include_pcm: bool, carrier_phase: usize| {
        let mut chain = Chain::new(ChainConfig::default());
        chain.set_prepared(Box::new(Prepared::new(Arc::clone(&prepared), 480).unwrap()));
        for (endpoint, epoch, format) in [
            (
                ENDPOINT_PRIMARY,
                1,
                bitstream_format(BitstreamCodec::Eac3, 48_000),
            ),
            (ENDPOINT_SECOND, 2, stereo_pcm_format()),
        ] {
            if (endpoint == ENDPOINT_PRIMARY && !include_ddp)
                || (endpoint == ENDPOINT_SECOND && !include_pcm)
            {
                continue;
            }
            chain.push_record(
                Record::FormatChange {
                    endpoint,
                    epoch,
                    repeat: false,
                    qpc: 0,
                    format,
                },
                &mut |_| {},
            );
            chain.push_record(
                Record::StreamStart {
                    endpoint,
                    epoch,
                    repeat: false,
                    qpc: 0,
                },
                &mut |_| {},
            );
        }

        let mut output = [Vec::new(), Vec::new()];
        let mut collect = |event: ChainEvent<'_>| match event {
            ChainEvent::Block(block) => {
                output[0].extend_from_slice(block.channel(0));
                output[1].extend_from_slice(block.channel(1));
            }
            ChainEvent::Error { error, .. } => panic!("unexpected error: {error}"),
            _ => {}
        };
        let mut ddp_cursor = 0;
        let mut pcm_cursor = 0;
        let first_ddp = carrier_phase.clamp(1, 768);
        while ddp_cursor < burst.len() || pcm_cursor < pcm.len() {
            let ddp_time = ddp_cursor as u128 * 48_000 / 768_000;
            let pcm_time = pcm_cursor as u128 * 48_000 / 384_000;
            let take_ddp =
                ddp_cursor < burst.len() && (pcm_cursor == pcm.len() || ddp_time <= pcm_time);
            if take_ddp {
                let amount =
                    if ddp_cursor == 0 { first_ddp } else { 768 }.min(burst.len() - ddp_cursor);
                if include_ddp {
                    chain.push_record(
                        Record::Data {
                            endpoint: ENDPOINT_PRIMARY,
                            epoch: 1,
                            flags: 0,
                            pos: ddp_cursor as u64,
                            qpc: (ddp_cursor + amount) as u64 * 10_000_000 / 768_000,
                            qpc_written: 0,
                            bytes: &burst[ddp_cursor..ddp_cursor + amount],
                        },
                        &mut collect,
                    );
                }
                ddp_cursor += amount;
            } else {
                let amount = 384.min(pcm.len() - pcm_cursor);
                if include_pcm {
                    chain.push_record(
                        Record::Data {
                            endpoint: ENDPOINT_SECOND,
                            epoch: 2,
                            flags: 0,
                            pos: pcm_cursor as u64,
                            qpc: (pcm_cursor + amount) as u64 * 10_000_000 / 384_000,
                            qpc_written: 0,
                            bytes: &pcm[pcm_cursor..pcm_cursor + amount],
                        },
                        &mut collect,
                    );
                }
                pcm_cursor += amount;
            }
        }
        chain.finish(&mut collect);
        drop(collect);
        assert_eq!(
            chain.lane_underrun_frames(),
            0,
            "ddp={include_ddp} pcm={include_pcm} phase={carrier_phase}"
        );
        assert_eq!(chain.dropped_frames(), 0);
        output
    };

    let mut seed = 0x5eed_0028_u64;
    for case in 0..20 {
        seed = seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let phase = (seed % 768 + 1) as usize;
        let mixed = render(true, true, phase);
        let ddp = render(true, false, phase);
        let pcm = render(false, true, phase);
        for channel in 0..2 {
            // Chain exposes binaural blocks, not individual scene objects.
            // All rendered objects and routed LFE participate in these complete
            // L/R vectors, including leading silence and the final padded block.
            assert!(mixed[channel].len() > 1_920, "case={case}");
            assert_eq!(mixed[channel].len(), ddp[channel].len(), "case={case}");
            assert_eq!(mixed[channel].len(), pcm[channel].len(), "case={case}");
            let expected_start = (0..mixed[channel].len())
                .find(|&frame| ddp[channel][frame] + pcm[channel][frame] != 0.0);
            assert_eq!(
                first_nonzero(&mixed[channel]),
                expected_start,
                "case={case} channel={channel} start"
            );
            for frame in 0..mixed[channel].len() {
                let expected = ddp[channel][frame] + pcm[channel][frame];
                assert!(
                    (mixed[channel][frame] - expected).abs() < 2.0e-5,
                    "case={case} phase={phase} channel={channel} frame={frame}: {} != {expected}",
                    mixed[channel][frame]
                );
            }
        }
    }
}

#[test]
fn unknown_encoded_rates_reach_the_selected_bitstream_decoders() {
    let fixture = std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../decode/tests/fixtures/ddp/tones-5.1.eac3"),
    )
    .unwrap();
    let payload = &fixture[..2560];
    let mut ddp_burst = Vec::new();
    bitstream::frame_burst(bitstream::DataType::Eac3, payload, 24_576, &mut ddp_burst);

    for (codec, data_type, payload, expect_block) in [
        (
            BitstreamCodec::Eac3,
            bitstream::DataType::Eac3,
            payload,
            true,
        ),
        (
            BitstreamCodec::Mat21,
            bitstream::DataType::Mat,
            &[][..],
            false,
        ),
    ] {
        let format = bitstream_format(codec, 0);
        let mut chain = Chain::new(ChainConfig::default());
        let mut events = Vec::new();
        chain.push_record(
            Record::FormatChange {
                endpoint: ENDPOINT_PRIMARY,
                epoch: 7,
                repeat: false,
                qpc: 0,
                format,
            },
            &mut |event| match event {
                ChainEvent::FormatChanged { .. } => events.push("format"),
                ChainEvent::Error { error, .. } => match error {
                    engine::ChainError::UnsupportedSceneRate(_) => events.push("rate"),
                    engine::ChainError::Decode(_) => events.push("decode"),
                    engine::ChainError::Record(_) => events.push("record"),
                },
                _ => {}
            },
        );
        assert_eq!(events, ["format"]);

        if codec == BitstreamCodec::Eac3 {
            let mut needs_brir = 0;
            let mut errors = 0;
            chain.push_record(
                Record::Data {
                    endpoint: ENDPOINT_PRIMARY,
                    epoch: 7,
                    flags: 0,
                    pos: 0,
                    qpc: 123,
                    qpc_written: 0,
                    bytes: &ddp_burst,
                },
                &mut |event| match event {
                    ChainEvent::NeedsBrir {
                        rate: 48_000,
                        block: 480,
                    } => needs_brir += 1,
                    ChainEvent::Error { .. } => errors += 1,
                    _ => {}
                },
            );
            assert!(expect_block);
            assert_eq!(needs_brir, 1);
            assert_eq!(errors, 0);
        } else {
            let mut burst = Vec::new();
            bitstream::frame_burst(data_type, payload, 8, &mut burst);
            let mut saw_decode_error = false;
            chain.push_record(
                Record::Data {
                    endpoint: ENDPOINT_PRIMARY,
                    epoch: 7,
                    flags: 0,
                    pos: 0,
                    qpc: 123,
                    qpc_written: 0,
                    bytes: &burst,
                },
                &mut |event| {
                    if matches!(
                        event,
                        ChainEvent::Error {
                            error: engine::ChainError::Decode(_),
                            ..
                        }
                    ) {
                        saw_decode_error = true;
                    }
                },
            );
            assert!(saw_decode_error, "MAT reached its planned decoder");
        }
    }
}

#[test]
fn decode_error_clears_ddp_recut_remainder_before_recovery() {
    let temp = TempDir::new("ddp-error-reset");
    let ir = temp.0.join("FC.wav");
    write_stereo_ir(&ir, 1, 0, 1.0, 0, 1.0);
    let prepared = Arc::new(
        brir::prepare(
            &profile(
                vec![DirectionFile {
                    name: "FC".into(),
                    azimuth_deg: 0.0,
                    elevation_deg: 0.0,
                    path: ir,
                }],
                0.0,
                0.0,
            ),
            48_000,
            partition_for(480),
        )
        .unwrap(),
    );

    let fixture = std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../decode/tests/fixtures/ddp/tones-5.1.eac3"),
    )
    .unwrap();
    let mut first = Vec::new();
    let mut corrupt = Vec::new();
    let mut second = Vec::new();
    bitstream::frame_burst(
        bitstream::DataType::Eac3,
        &fixture[..2560],
        24_576,
        &mut first,
    );
    bitstream::frame_burst(bitstream::DataType::Eac3, &[0; 7], 24_576, &mut corrupt);
    bitstream::frame_burst(
        bitstream::DataType::Eac3,
        &fixture[2560..5120],
        24_576,
        &mut second,
    );
    let mut combined = Vec::with_capacity(first.len() + corrupt.len() + second.len());
    combined.extend_from_slice(&first);
    combined.extend_from_slice(&corrupt);
    combined.extend_from_slice(&second);

    let render = |records: &[(&[u8], u16)]| {
        let mut chain = Chain::new(ChainConfig::default());
        chain.set_prepared(Box::new(Prepared::new(Arc::clone(&prepared), 480).unwrap()));
        chain.push_record(
            Record::FormatChange {
                endpoint: ENDPOINT_PRIMARY,
                epoch: 7,
                repeat: false,
                qpc: 0,
                format: bitstream_format(BitstreamCodec::Eac3, 48_000),
            },
            &mut |_| {},
        );
        let mut blocks = Vec::new();
        let mut errors = 0;
        for &(bytes, flags) in records {
            chain.push_record(
                Record::Data {
                    endpoint: ENDPOINT_PRIMARY,
                    epoch: 7,
                    flags,
                    pos: 0,
                    qpc: 100,
                    qpc_written: 0,
                    bytes,
                },
                &mut |event| match event {
                    ChainEvent::Block(block) => {
                        blocks.push((
                            block.source_qpc,
                            block.channel(0).to_vec(),
                            block.channel(1).to_vec(),
                        ));
                    }
                    ChainEvent::Error { .. } => errors += 1,
                    _ => {}
                },
            );
        }
        chain.push_record(
            Record::StreamStop {
                endpoint: ENDPOINT_PRIMARY,
                epoch: 7,
                repeat: false,
                qpc: 200,
            },
            &mut |event| {
                if let ChainEvent::Block(block) = event {
                    blocks.push((
                        block.source_qpc,
                        block.channel(0).to_vec(),
                        block.channel(1).to_vec(),
                    ));
                }
            },
        );
        (blocks, errors)
    };

    let (recovered, errors) = render(&[(&combined, 0)]);
    let (first_only, first_errors) = render(&[(&first, 0)]);
    let (second_only, second_errors) = render(&[(&second, RECORD_FLAG_DISCONTINUITY)]);
    assert_eq!(errors, 1);
    assert_eq!(first_errors, 0);
    assert_eq!(second_errors, 0);
    assert!(recovered.len() > first_only.len());
    assert!(recovered.len() >= first_only.len() + second_only.len());
    assert!(
        recovered.iter().take(first_only.len()).any(|block| block
            .1
            .iter()
            .chain(&block.2)
            .any(|sample| *sample != 0.0))
    );
    assert!(
        recovered[first_only.len()..].iter().any(|block| block
            .1
            .iter()
            .chain(&block.2)
            .any(|sample| *sample != 0.0))
    );
}

#[test]
fn overrun_and_discontinuity_reset_without_panicking() {
    let mut chain = Chain::new(ChainConfig::default());
    chain.push_record(
        Record::FormatChange {
            endpoint: ENDPOINT_PRIMARY,
            epoch: 1,
            repeat: false,
            qpc: 0,
            format: stereo_pcm_format(),
        },
        &mut |_| {},
    );
    chain.push_record(
        Record::Data {
            endpoint: ENDPOINT_PRIMARY,
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
            endpoint: ENDPOINT_PRIMARY,
            pos: 7,
            qpc: 2,
            lost_bytes: 99,
        },
        &mut |event| {
            if matches!(event, ChainEvent::Overrun { lost_bytes: 99, .. }) {
                overruns += 1;
            }
        },
    );
    chain.push_record(
        Record::Data {
            endpoint: ENDPOINT_PRIMARY,
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
fn endpoint_format_and_overrun_reset_only_their_front() {
    let temp = TempDir::new("endpoint-isolation");
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
    let prepared = brir::prepare(&profile, 48_000, 480).unwrap();
    let mut chain = Chain::new(ChainConfig {
        arrival_slack_frames: 0,
        ..ChainConfig::default()
    });
    chain.set_prepared(Box::new(Prepared::new(Arc::new(prepared), 480).unwrap()));
    for (endpoint, epoch) in [(ENDPOINT_PRIMARY, 1), (ENDPOINT_SECOND, 2)] {
        chain.push_record(
            Record::FormatChange {
                endpoint,
                epoch,
                repeat: false,
                qpc: 0,
                format: stereo_pcm_format(),
            },
            &mut |_| {},
        );
    }

    let half = pcm_bytes(0.25, 240);
    chain.push_record(
        Record::Data {
            endpoint: ENDPOINT_SECOND,
            epoch: 2,
            flags: 0,
            pos: 0,
            qpc: 1,
            qpc_written: 0,
            bytes: &half,
        },
        &mut |_| {},
    );
    chain.push_record(
        Record::Overrun {
            endpoint: ENDPOINT_PRIMARY,
            pos: 0,
            qpc: 2,
            lost_bytes: 17,
        },
        &mut |_| {},
    );
    let mut blocks = 0;
    chain.push_record(
        Record::Data {
            endpoint: ENDPOINT_SECOND,
            epoch: 2,
            flags: 0,
            pos: half.len() as u64,
            qpc: 3,
            qpc_written: 0,
            bytes: &half,
        },
        &mut |event| {
            if matches!(event, ChainEvent::Block(_)) {
                blocks += 1;
            }
        },
    );
    assert_eq!(blocks, 1, "primary overrun reset the second decoder");

    chain.push_record(
        Record::Data {
            endpoint: ENDPOINT_PRIMARY,
            epoch: 1,
            flags: 0,
            pos: 0,
            qpc: 4,
            qpc_written: 0,
            bytes: &half,
        },
        &mut |_| {},
    );
    chain.push_record(
        Record::FormatChange {
            endpoint: ENDPOINT_SECOND,
            epoch: 3,
            repeat: false,
            qpc: 5,
            format: stereo_pcm_format(),
        },
        &mut |_| {},
    );
    let mut mixed = 0;
    chain.push_record(
        Record::Data {
            endpoint: ENDPOINT_PRIMARY,
            epoch: 1,
            flags: 0,
            pos: half.len() as u64,
            qpc: 6,
            qpc_written: 0,
            bytes: &half,
        },
        &mut |event| {
            if matches!(event, ChainEvent::Block(_)) {
                mixed += 1;
            }
        },
    );
    assert_eq!(mixed, 1, "second format change reset the primary decoder");
}

#[test]
fn format_decode_error_overrun_and_discontinuity_do_not_change_the_other_lane() {
    let mut corrupt = Vec::new();
    bitstream::frame_burst(bitstream::DataType::Eac3, &[0; 7], 24_576, &mut corrupt);

    let render = |disturbance: Option<&str>| {
        let mut stream = vec![0; 300_000];
        let mut writer = RecordWriter::new(&mut stream);
        writer.set_endpoint(ENDPOINT_SECOND);
        assert!(writer.format_change(2, 0, &wave_format_bytes(&pcm_format(48_000, 1, 0x2))));
        assert!(writer.stream_start(2, 0));
        if disturbance.is_some() {
            writer.set_endpoint(ENDPOINT_PRIMARY);
            assert!(writer.format_change(1, 0, &wave_format_bytes(&pcm_format(48_000, 1, 0x1))));
            assert!(writer.stream_start(1, 0));
        }
        for ms in 0..400_u64 {
            if disturbance.is_some() && ms < 100 {
                writer.set_endpoint(ENDPOINT_PRIMARY);
                assert!(writer.data(1, ms * 48 * 4, (ms + 1) * 1_000, &mono_pcm_bytes(0.125, 48)));
            }
            if ms == 100 {
                writer.set_endpoint(ENDPOINT_PRIMARY);
                match disturbance {
                    Some("format change") => assert!(writer.format_change(
                        3,
                        100_000,
                        &wave_format_bytes(&pcm_format(48_000, 1, 0x1))
                    )),
                    Some("decode error") => {
                        assert!(writer.format_change(
                            3,
                            100_000,
                            &wave_format_bytes(&bitstream_format(BitstreamCodec::Eac3, 48_000))
                        ));
                        assert!(writer.data(3, 0, 132_000, &corrupt));
                    }
                    Some("overrun") => assert!(writer.overrun(48 * 4 * 100, 100_000, 99)),
                    Some("discontinuity") => assert!(writer.push(
                        RecordHeader {
                            kind: bio_abi::RECORD_KIND_DATA,
                            flags: RECORD_FLAG_DISCONTINUITY,
                            epoch: 1,
                            byte_pos: 48 * 4 * 100,
                            qpc: 100_000,
                            qpc_written: 0,
                            len: 0,
                            endpoint: ENDPOINT_PRIMARY,
                            reserved: 0,
                        },
                        &[],
                    )),
                    None => {}
                    Some(other) => panic!("unknown disturbance {other}"),
                }
            }
            writer.set_endpoint(ENDPOINT_SECOND);
            assert!(writer.data(
                2,
                ms * 48 * 4,
                (ms + 1) * 1_000,
                &mono_pcm_bytes(0.001 * (ms / 10 + 1) as f32, 48)
            ));
        }
        let len = writer.len();
        stream.truncate(len);
        let mut chain = Chain::with_qpc_frequency(ChainConfig::default(), 1_000_000);
        chain.set_prepared(separated_prepared("event-isolation"));
        let mut output = [Vec::new(), Vec::new()];
        let mut formats = 0;
        let mut overruns = 0;
        let mut errors = 0;
        let mut collect = |event: ChainEvent<'_>| match event {
            ChainEvent::Block(block) => {
                output[0].extend_from_slice(block.channel(0));
                output[1].extend_from_slice(block.channel(1));
            }
            ChainEvent::FormatChanged {
                endpoint: ENDPOINT_PRIMARY,
                ..
            } => formats += 1,
            ChainEvent::Overrun {
                endpoint: ENDPOINT_PRIMARY,
                ..
            } => overruns += 1,
            ChainEvent::Error {
                endpoint: Some(ENDPOINT_PRIMARY),
                ..
            } => errors += 1,
            ChainEvent::Error { error, .. } => panic!("unexpected error: {error}"),
            _ => {}
        };
        chain.push_records(&stream, &mut collect);
        chain.finish(&mut collect);
        drop(collect);
        let drops = chain.dropped_frames();
        (output, formats, overruns, errors, drops)
    };

    let (solo, _, _, _, solo_drops) = render(None);
    assert_eq!(solo_drops, 0);
    for disturbance in ["format change", "decode error", "overrun", "discontinuity"] {
        let (mixed, formats, overruns, errors, drops) = render(Some(disturbance));
        // Separated impulse responses expose every sample of the unchanged
        // right-hand lane while the disturbed left-hand lane is genuinely audible.
        assert!(
            mixed[0][480..4_320].iter().all(|sample| *sample > 0.1),
            "{disturbance}: primary must emit normal audio before the disturbance"
        );
        assert_eq!(mixed[1].len(), solo[1].len(), "{disturbance} length");
        assert_eq!(
            first_nonzero(&mixed[1]),
            first_nonzero(&solo[1]),
            "{disturbance} start"
        );
        for (frame, (&actual, &expected)) in mixed[1].iter().zip(&solo[1]).enumerate() {
            assert!(
                (actual - expected).abs() < 2.0e-6,
                "{disturbance} frame={frame}: {actual} != {expected}"
            );
        }
        assert_eq!(
            formats,
            1 + usize::from(matches!(disturbance, "format change" | "decode error"))
        );
        assert_eq!(overruns, usize::from(disturbance == "overrun"));
        assert_eq!(errors, usize::from(disturbance == "decode error"));
        // Each event cuts the old primary generation after its 64-frame fade;
        // the exact right-hand solo comparison above proves lane isolation.
        assert_eq!(drops, MIX_FRAMES as u64 - 16);
    }
}

#[test]
fn oversized_arrival_slack_is_clamped() {
    let chain = Chain::new(ChainConfig {
        arrival_slack_frames: u64::MAX,
        ..ChainConfig::default()
    });
    assert_eq!(chain.arrival_slack_frames(), MAX_ARRIVAL_SLACK_FRAMES);
}

#[test]
fn endpoint_mask_selects_only_low_endpoint_bits() {
    let run = |mask| {
        let mut chain = Chain::new(ChainConfig {
            endpoints: EndpointFilter::Mask(mask),
            ..ChainConfig::default()
        });
        let mut selected = Vec::new();
        for endpoint in [ENDPOINT_PRIMARY, ENDPOINT_SECOND] {
            chain.push_record(
                Record::FormatChange {
                    endpoint,
                    epoch: 1,
                    repeat: false,
                    qpc: 0,
                    format: stereo_pcm_format(),
                },
                &mut |event| {
                    if let ChainEvent::FormatChanged { endpoint, .. } = event {
                        selected.push(endpoint);
                    }
                },
            );
        }
        selected
    };
    assert_eq!(run(0b0101), vec![ENDPOINT_PRIMARY]);
    assert_eq!(run(0b1_0000_0101), vec![ENDPOINT_PRIMARY]);
}

#[test]
fn endpoint_filter_ignores_unselected_endpoint_without_counting_it_invalid() {
    let mut chain = Chain::new(ChainConfig {
        endpoints: engine::EndpointFilter::Only(ENDPOINT_PRIMARY),
        ..ChainConfig::default()
    });
    let mut events = 0;
    chain.push_record(
        Record::FormatChange {
            endpoint: ENDPOINT_SECOND,
            epoch: 1,
            repeat: false,
            qpc: 0,
            format: stereo_pcm_format(),
        },
        &mut |_| events += 1,
    );
    chain.push_record(
        Record::Data {
            endpoint: ENDPOINT_SECOND,
            epoch: 1,
            flags: 0,
            pos: 0,
            qpc: 1,
            qpc_written: 0,
            bytes: &[0; 8],
        },
        &mut |_| events += 1,
    );
    assert_eq!(events, 0);
    assert_eq!(chain.skipped_records(), 0);

    chain.push_record(
        Record::StreamStop {
            endpoint: bio_abi::MAX_ENDPOINTS as u16,
            epoch: 0,
            repeat: false,
            qpc: 0,
        },
        &mut |_| events += 1,
    );
    assert_eq!(events, 0);
    assert_eq!(chain.skipped_records(), 1);

    let mut stream = vec![0; 64];
    let mut writer = RecordWriter::new(&mut stream);
    writer.set_endpoint(ENDPOINT_SECOND);
    assert!(writer.stream_stop(1, 2));
    let length = writer.len();
    // Turn the endpoint-1 stop into a bad-length record while retaining its header.
    stream[32..36].copy_from_slice(&1_u32.to_le_bytes());
    stream.resize(length + 1, 0);
    chain.push_records(&stream, &mut |_| events += 1);
    assert_eq!(events, 0);
    assert_eq!(chain.skipped_records(), 1);
}

#[test]
fn sound_before_the_first_brir_is_counted_as_dropped() {
    let mut chain = Chain::new(ChainConfig {
        arrival_slack_frames: 0,
        ..ChainConfig::default()
    });
    let mut needs = 0;
    chain.push_records(&pcm_stream(1), &mut |event| {
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
    assert_eq!(chain.dropped_frames(), MIX_FRAMES as u64);
    assert_eq!(chain.lane_underrun_frames(), 0);

    chain.set_prepared(identity_prepared("first-brir-accounting"));
    let mut audible = false;
    chain.push_record(
        Record::Data {
            endpoint: ENDPOINT_PRIMARY,
            epoch: 1,
            flags: 0,
            pos: (MIX_FRAMES * 2 * size_of::<f32>()) as u64,
            qpc: 200_010,
            qpc_written: 0,
            bytes: &pcm_bytes(0.25, MIX_FRAMES),
        },
        &mut |event| {
            if let ChainEvent::Block(block) = event {
                audible |= block.channel(0).iter().any(|sample| *sample != 0.0)
                    || block.channel(1).iter().any(|sample| *sample != 0.0);
            }
        },
    );
    chain.finish(&mut |event| {
        if let ChainEvent::Block(block) = event {
            audible |= block.channel(0).iter().any(|sample| *sample != 0.0)
                || block.channel(1).iter().any(|sample| *sample != 0.0);
        }
    });

    assert!(audible);
    assert_eq!(chain.dropped_frames(), MIX_FRAMES as u64);
    assert_eq!(chain.lane_underrun_frames(), 0);
}

#[test]
fn needs_brir_is_emitted_once_for_repeated_common_shape_mismatch() {
    let temp = TempDir::new("needs-brir");
    let ir = temp.0.join("FL.wav");
    write_stereo_ir_at_rate(&ir, 96_000, 16, 0, 1.0, 0, 1.0);
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

#[test]
fn resampled_restarts_beside_an_active_lane_do_not_creep_ahead() {
    // B is 44.1 kHz, so every stop flushes one FIR length of ring-out. B stops
    // and starts again every 10 ms for 600 ms while A plays throughout.
    let mut stream = vec![0; 400_000];
    let mut writer = RecordWriter::new(&mut stream);
    writer.set_endpoint(ENDPOINT_PRIMARY);
    assert!(writer.format_change(1, 0, &wave_format_bytes(&pcm_format(48_000, 1, 0x1))));
    writer.set_endpoint(ENDPOINT_SECOND);
    assert!(writer.format_change(2, 0, &wave_format_bytes(&pcm_format(44_100, 1, 0x2))));
    for tick in 0..60_u64 {
        writer.set_endpoint(ENDPOINT_PRIMARY);
        assert!(writer.data(
            1,
            tick * 1_920,
            (tick + 1) * 10_000,
            &mono_pcm_bytes(0.125, 480)
        ));
        writer.set_endpoint(ENDPOINT_SECOND);
        assert!(writer.stream_start(2, tick * 10_000));
        assert!(writer.data(2, 0, (tick + 1) * 10_000, &mono_pcm_bytes(0.25, 441)));
        assert!(writer.stream_stop(2, (tick + 1) * 10_000));
    }
    let len = writer.len();
    stream.truncate(len);

    let mixed = render_push_records_stream(&stream);
    assert_eq!(mixed.drops, 0);
    assert_eq!(mixed.underruns, 0);
    // Each restart lands at the byte clock, so B ends with A plus one ring-out
    // interval instead of drifting further ahead with every restart.
    assert!(mixed.output[1].len() > 60 * 480 + 480);
    // A (FL, left ear only) is untouched by B's (FR, right ear) restarts.
    let solo = render_push_records_stream(&retain_endpoint_records(&stream, ENDPOINT_PRIMARY));
    let left = &mixed.output[0];
    assert_close(&left[..solo.output[0].len()], &solo.output[0], "A");
    assert!(
        left[solo.output[0].len()..]
            .iter()
            .all(|sample| sample.abs() <= 2.0e-6)
    );
}

#[test]
fn ddp_restarts_beside_an_active_lane_keep_every_burst() {
    // B carries E-AC-3 in a 192 kHz IEC 61937 carrier (768,000 bytes/s). A
    // burst's payload ends about 160 frames into its 32 ms period and decodes
    // into 1,536 frames at once, so B's last scene runs ahead of its byte
    // clock. Each generation sends four full bursts and the first 10 ms of a
    // fifth, stops right after that fifth burst decoded, and starts again with
    // no gap, three times over, while A plays PCM throughout.
    let fixture = std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../decode/tests/fixtures/ddp/tones-5.1.eac3"),
    )
    .unwrap();
    const FRAME: usize = 2_560;
    const BURST: usize = 24_576;
    const TICK_BYTES: usize = 7_680;
    const GENERATIONS: usize = 3;
    let generation_bytes = 4 * BURST + TICK_BYTES;
    let mut generations = Vec::new();
    for generation in 0..GENERATIONS {
        let mut carrier = Vec::new();
        for index in 0..5 {
            let frame_index = generation * 5 + index;
            let frame = &fixture[frame_index * FRAME..(frame_index + 1) * FRAME];
            let mut burst = Vec::new();
            bitstream::frame_burst(bitstream::DataType::Eac3, frame, BURST, &mut burst);
            carrier.extend_from_slice(&burst);
        }
        carrier.truncate(generation_bytes);
        generations.push(carrier);
    }
    let bytes_to_us = |bytes: usize| bytes as u64 * 1_000_000 / 768_000;

    // (end time, endpoint order, record) sorted by DMA time, B first on ties.
    enum Item {
        Start(u64),
        Data {
            pos: usize,
            generation: usize,
            len: usize,
            end: u64,
        },
        Stop(u64),
        Pcm(u64),
    }
    let mut items = Vec::new();
    let mut base = 0_usize;
    for generation in 0..GENERATIONS {
        let start = bytes_to_us(base);
        items.push((start, 0, Item::Start(start)));
        let mut pos = 0;
        while pos < generation_bytes {
            let len = TICK_BYTES.min(generation_bytes - pos);
            let end = bytes_to_us(base + pos + len);
            items.push((
                end,
                0,
                Item::Data {
                    pos,
                    generation,
                    len,
                    end,
                },
            ));
            pos += len;
        }
        let stop = bytes_to_us(base + generation_bytes);
        items.push((stop, 0, Item::Stop(stop)));
        base += generation_bytes;
    }
    let total_us = bytes_to_us(base);
    let mut tick = 0;
    while tick * 10_000 < total_us {
        let end = (tick + 1) * 10_000;
        items.push((end, 1, Item::Pcm(end)));
        tick += 1;
    }
    items.sort_by_key(|(end, order, _)| (*end, *order));

    let mut stream = vec![0; 600_000];
    let mut writer = RecordWriter::new(&mut stream);
    writer.set_endpoint(ENDPOINT_PRIMARY);
    assert!(writer.format_change(
        1,
        0,
        &wave_format_bytes(&bitstream_format(BitstreamCodec::Eac3, 48_000))
    ));
    writer.set_endpoint(ENDPOINT_SECOND);
    assert!(writer.format_change(2, 0, &wave_format_bytes(&pcm_format(48_000, 1, 0x1))));
    for (_, _, item) in &items {
        match *item {
            Item::Start(qpc) => {
                writer.set_endpoint(ENDPOINT_PRIMARY);
                assert!(writer.stream_start(1, qpc));
            }
            Item::Data {
                pos,
                generation,
                len,
                end,
            } => {
                writer.set_endpoint(ENDPOINT_PRIMARY);
                let bytes = &generations[generation][pos..pos + len];
                assert!(writer.data(1, pos as u64, end, bytes));
            }
            Item::Stop(qpc) => {
                writer.set_endpoint(ENDPOINT_PRIMARY);
                assert!(writer.stream_stop(1, qpc));
            }
            Item::Pcm(end) => {
                writer.set_endpoint(ENDPOINT_SECOND);
                let pos = (end / 10_000 - 1) * 1_920;
                assert!(writer.data(2, pos, end, &mono_pcm_bytes(0.125, 480)));
            }
        }
    }
    let len = writer.len();
    stream.truncate(len);

    let mixed = render_push_records_stream(&stream);
    assert_eq!(mixed.errors, 0);
    assert_eq!(mixed.drops, 0);
    assert_eq!(mixed.underruns, 0);
}

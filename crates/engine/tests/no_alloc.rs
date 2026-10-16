#![forbid(unsafe_code)]

use std::sync::Arc;

use assert_no_alloc::{AllocDisabler, assert_no_alloc};
use brir::PreparedBrir;
use convolver::FilterBank;
use engine::{
    Chain, ChainConfig, ChainEvent, MIX_FRAMES, MIX_RATE, MatrixUpmixer, MixerConfig, Prepared,
    RingUpmixer, SceneMixer, UpmixStage,
};
use render::PanGeometry;
use ssr_abi::{RECORD_FLAG_DISCONTINUITY, Record, RecordWriter, wave_format_bytes};
use ssr_types::app::UpmixBypass;
use ssr_types::{
    BitstreamCodec, BitstreamFormat, Direction, Layout, MAX_OBJECTS, Object, PcmFormat, Position,
    SampleType, SceneBlock, SourceFormat,
};
use streamring::{Consumer, Geometry, InProcessEvent, Producer, RegionBuf};

#[global_allocator]
static ALLOCATOR: AllocDisabler = AllocDisabler;

fn pcm_format(rate: u32) -> SourceFormat {
    SourceFormat::Pcm(PcmFormat {
        channels: 2,
        sample_rate: rate,
        bits_per_sample: 32,
        valid_bits: 32,
        sample_type: SampleType::Float,
        channel_mask: 3,
    })
}

fn pcm_record(epoch: u32, frames: usize) -> Vec<u8> {
    let pcm = vec![0_u8; frames * 2 * 4];
    let mut record = vec![0; ssr_abi::record_size(pcm.len())];
    let mut writer = RecordWriter::new(&mut record);
    assert!(writer.data(epoch, 0, 1, &pcm));
    let length = writer.len();
    record.truncate(length);
    record
}

fn prepared() -> Box<Prepared> {
    let ir = [1.0_f32];
    let filters = [[ir.as_slice(), ir.as_slice()]];
    let brir = Arc::new(PreparedBrir {
        layout: Layout::new(vec![Direction::new("front", 0.0, 0.0)]).unwrap(),
        bank: Arc::new(FilterBank::new(&filters, MIX_FRAMES).unwrap()),
        rate: MIX_RATE,
        partition: MIX_FRAMES,
        lfe: None,
        gain: 1.0,
        limiter_ceiling: 1.0,
        ir_frames: 1,
        trimmed_leading_frames: 0,
    });
    Box::new(Prepared::new(brir, MIX_FRAMES).unwrap())
}

fn speaker_prepared() -> Box<Prepared> {
    let layout = Layout::new(vec![Direction::new("front", 0.0, 0.0)]).unwrap();
    let fir_ir = [0.0_f32, 1.0];
    let fir_filters = [fir_ir.as_slice()];
    let fir = Arc::new(FilterBank::new_mono(&fir_filters, MIX_FRAMES).unwrap());
    let speakers = speakers::PreparedSpeakers {
        layout,
        rate: MIX_RATE,
        partition: MIX_FRAMES,
        device_channels: 4,
        channel_mask: 0,
        speakers: vec![speakers::PreparedSpeaker {
            name: "front".into(),
            device_channel: 2,
            small: true,
            crossover: Some(speakers::crossover_coefficients(MIX_RATE, 80.0)),
            delay_frames: 3,
            gain: 0.5,
            fir: Some(Arc::clone(&fir)),
        }],
        subwoofers: vec![speakers::PreparedSubwoofer {
            device_channel: 3,
            delay_frames: 3,
            gain: 0.5,
            fir: Some(fir),
        }],
        bass_to: Vec::new(),
        lfe_gain: 10.0_f32.sqrt(),
        gain: 1.0,
        limiter_ceiling: 1.0,
    };
    Box::new(Prepared::speakers(Arc::new(speakers), MIX_FRAMES).unwrap())
}

fn correction_prepared() -> Box<Prepared> {
    let yaml = r#"
description: "inputs 0 L, 1 R | outputs 0 L, 1 R | generated"
devices:
  samplerate: 48000
  capture: {channels: 2}
  playback: {channels: 2}
filters:
  gain: {type: Gain, parameters: {gain: -3, inverted: false}}
mixers:
  route_out:
    channels: {in: 2, out: 2}
    mapping:
      - {dest: 0, sources: [{channel: 0}]}
      - {dest: 1, sources: [{channel: 1}]}
pipeline:
  - {type: Filter, channels: [0, 1], names: [gain]}
  - {type: Mixer, name: route_out}
"#;
    let correction = correction::parse(yaml, std::path::Path::new(".")).unwrap();
    let layout = correction.layout().unwrap();
    let inputs = correction.inputs.clone();
    let lfe_input = correction.lfe_input();
    let output_layout = ssr_types::OutputLayout {
        channels: correction.device_channels as u16,
        channel_mask: correction.channel_mask,
    };
    let prepared = Arc::new(correction.prepare(MIX_RATE, MIX_FRAMES).unwrap());
    Box::new(
        Prepared::correction(
            prepared,
            layout,
            inputs,
            lfe_input,
            output_layout,
            0.0,
            0.0,
            MIX_FRAMES,
        )
        .unwrap(),
    )
}

fn geometry() -> PanGeometry {
    PanGeometry::new(&Layout::new(vec![Direction::new("front", 0.0, 0.0)]).unwrap()).unwrap()
}

fn side_geometry() -> PanGeometry {
    PanGeometry::new(&Layout::new(vec![Direction::new("side", 90.0, 0.0)]).unwrap()).unwrap()
}

fn block<'a>(
    rate: u32,
    signals: &'a [Vec<f32>],
    objects: &'a mut [Object<'a>; MAX_OBJECTS],
) -> SceneBlock<'a> {
    for (index, signal) in signals.iter().enumerate() {
        objects[index] = Object {
            signal,
            position: Position::FRONT,
            gain: 1.0,
            spread: 0.0,
        };
    }
    SceneBlock {
        epoch: 1,
        frames: signals[0].len(),
        rate,
        source_qpc: 0,
        objects: &objects[..signals.len()],
        lfe: None,
        new_objects: 0,
    }
}

#[test]
fn steady_state_pcm_push_records_does_not_allocate() {
    let mut format_record = vec![0; 256];
    let mut writer = RecordWriter::new(&mut format_record);
    assert!(writer.format_change(1, 0, &wave_format_bytes(&pcm_format(MIX_RATE))));
    let length = writer.len();
    format_record.truncate(length);
    let data_record = pcm_record(1, MIX_FRAMES);

    let mut chain = Chain::new(ChainConfig {
        arrival_slack_frames: 0,
        ..ChainConfig::default()
    });
    chain.set_prepared(prepared());
    chain.push_records(&format_record, &mut |_| {});
    assert_no_alloc(|| {
        chain.push_records(&data_record, &mut |event| {
            assert!(matches!(event, ChainEvent::Block(_)));
        });
    });
}

#[test]
fn steady_state_speaker_output_does_not_allocate() {
    let mut format_record = vec![0; 256];
    let mut writer = RecordWriter::new(&mut format_record);
    assert!(writer.format_change(1, 0, &wave_format_bytes(&pcm_format(MIX_RATE))));
    let length = writer.len();
    format_record.truncate(length);
    let data_record = pcm_record(1, MIX_FRAMES);

    let mut chain = Chain::new(ChainConfig {
        arrival_slack_frames: 0,
        ..ChainConfig::default()
    });
    chain.set_prepared(speaker_prepared());
    chain.push_records(&format_record, &mut |_| {});
    assert_no_alloc(|| {
        chain.push_records(&data_record, &mut |event| {
            let ChainEvent::Block(block) = event else {
                panic!("expected an output block");
            };
            assert_eq!(block.channels, 4);
        });
    });
}

#[test]
fn steady_state_correction_output_does_not_allocate() {
    let mut format_record = vec![0; 256];
    let mut writer = RecordWriter::new(&mut format_record);
    assert!(writer.format_change(1, 0, &wave_format_bytes(&pcm_format(MIX_RATE))));
    let length = writer.len();
    format_record.truncate(length);
    let data_record = pcm_record(1, MIX_FRAMES);

    let mut chain = Chain::new(ChainConfig {
        arrival_slack_frames: 0,
        ..ChainConfig::default()
    });
    chain.set_prepared(correction_prepared());
    chain.push_records(&format_record, &mut |_| {});
    assert_no_alloc(|| {
        chain.push_records(&data_record, &mut |event| {
            let ChainEvent::Block(block) = event else {
                panic!("expected an output block");
            };
            assert_eq!(block.channels, 2);
        });
    });
}

#[test]
fn chain_idle_does_not_allocate() {
    let mut format_record = vec![0; 256];
    let mut writer = RecordWriter::new(&mut format_record);
    assert!(writer.format_change(1, 0, &wave_format_bytes(&pcm_format(MIX_RATE))));
    let length = writer.len();
    format_record.truncate(length);
    let data_record = pcm_record(1, MIX_FRAMES);
    let mut chain = Chain::new(ChainConfig {
        arrival_slack_frames: 0,
        ..ChainConfig::default()
    });
    chain.set_prepared(prepared());
    chain.push_records(&format_record, &mut |_| {});
    chain.push_records(&data_record, &mut |_| {});
    chain.push_record(
        Record::StreamStop {
            endpoint: 0,
            epoch: 1,
            repeat: false,
            qpc: 1,
        },
        &mut |_| {},
    );

    assert_no_alloc(|| {
        chain.idle(100_001, &mut |event| {
            assert!(matches!(event, ChainEvent::Block(_)));
        });
    });
}

#[test]
fn unknown_subformat_format_change_does_not_allocate() {
    let mut wave_format = wave_format_bytes(&pcm_format(MIX_RATE));
    wave_format[39] ^= 0xff;
    let mut format_record = vec![0; 256];
    let mut writer = RecordWriter::new(&mut format_record);
    assert!(writer.format_change(1, 0, &wave_format));
    let length = writer.len();
    format_record.truncate(length);
    let mut chain = Chain::new(ChainConfig::default());
    let mut errors = 0;

    assert_no_alloc(|| {
        chain.push_records(&format_record, &mut |event| {
            if matches!(event, ChainEvent::Error { .. }) {
                errors += 1;
            }
        });
    });
    assert_eq!(errors, 1);
}

#[test]
fn format_change_and_bitstream_decode_error_do_not_allocate() {
    let format = SourceFormat::Bitstream(BitstreamFormat {
        codec: BitstreamCodec::Eac3,
        container_rate: 192_000,
        container_channels: 2,
        encoded_rate: 0,
        encoded_channels: 6,
    });
    let mut burst = Vec::new();
    bitstream::frame_burst(bitstream::DataType::Eac3, &[0_u8; 7], 32, &mut burst);
    let mut chain = Chain::new(ChainConfig::default());
    // As the runtime does: the E-AC-3 decode runs on the decode thread.
    let mut worker = chain.take_decode_worker().unwrap();

    assert_no_alloc(|| {
        chain.push_record(
            Record::FormatChange {
                endpoint: 0,
                epoch: 1,
                repeat: false,
                qpc: 0,
                format,
            },
            &mut |event| assert!(matches!(event, ChainEvent::FormatChanged { .. })),
        );
        // The burst is copied into a job; its result is not back yet.
        chain.push_record(
            Record::Data {
                endpoint: 0,
                epoch: 1,
                flags: 0,
                pos: 0,
                qpc: 0,
                qpc_written: 0,
                bytes: &burst,
            },
            &mut |event| panic!("unexpected event {event:?}"),
        );
    });
    assert!(chain.decode_in_flight() > 0);
    while worker.run_one() {}
    let mut errors = 0;
    assert_no_alloc(|| {
        chain.idle(0, &mut |event| {
            assert!(matches!(event, ChainEvent::Error { .. }));
            errors += 1;
        });
    });
    assert_eq!(errors, 1);
    assert_eq!(chain.decode_in_flight(), 0);
}

#[test]
fn mixing_two_endpoints_does_not_allocate() {
    let primary_signals = [vec![0.25; MIX_FRAMES]];
    let secondary_signals = [vec![0.5; MIX_FRAMES]];
    let mut primary_objects = [Object::SILENT; MAX_OBJECTS];
    let mut secondary_objects = [Object::SILENT; MAX_OBJECTS];
    let primary = block(MIX_RATE, &primary_signals, &mut primary_objects);
    let secondary = block(MIX_RATE, &secondary_signals, &mut secondary_objects);
    let geometry = geometry();
    let mut mixer = SceneMixer::new(MixerConfig {
        arrival_slack_frames: 0,
        qpc_frequency: 1_000_000,
        endpoint_mask: u32::MAX,
    });
    mixer.lane_start(0, 0);
    mixer.lane_start(1, 0);

    assert_no_alloc(|| {
        mixer.push_scene(0, &primary, Some(&geometry)).unwrap();
        mixer.push_scene(1, &secondary, Some(&geometry)).unwrap();
        mixer.advance(0, MIX_FRAMES as u64, &mut |mixed| {
            assert_eq!(mixed.directions, 1);
            assert_eq!(mixed.channels.len(), MIX_FRAMES)
        });
    });
}

#[test]
fn resample_recut_drop_stop_and_reset_paths_do_not_allocate() {
    let primary_signals = [vec![0.25; MIX_FRAMES]];
    let resample_44_signals = [vec![0.5; 441]];
    let resample_88_signals = [vec![0.625; 882]];
    let resample_96_signals = [vec![0.75; 960]];
    let ddp_signals = [vec![1.0; 1536]];
    let mut primary_objects = [Object::SILENT; MAX_OBJECTS];
    let mut objects_44 = [Object::SILENT; MAX_OBJECTS];
    let mut objects_88 = [Object::SILENT; MAX_OBJECTS];
    let mut objects_96 = [Object::SILENT; MAX_OBJECTS];
    let mut ddp_objects = [Object::SILENT; MAX_OBJECTS];
    let primary = block(MIX_RATE, &primary_signals, &mut primary_objects);
    let resample_44 = block(44_100, &resample_44_signals, &mut objects_44);
    let resample_88 = block(88_200, &resample_88_signals, &mut objects_88);
    let resample_96 = block(96_000, &resample_96_signals, &mut objects_96);
    let ddp = block(MIX_RATE, &ddp_signals, &mut ddp_objects);
    let geometry = geometry();
    let mut mixer = SceneMixer::new(MixerConfig::default());
    mixer.lane_start(0, 0);
    mixer.lane_start(1, 0);

    assert_no_alloc(|| {
        mixer.push_scene(0, &primary, Some(&geometry)).unwrap();
        mixer.push_scene(1, &resample_44, Some(&geometry)).unwrap();
        mixer.advance(0, MIX_FRAMES as u64, &mut |_| {});
        mixer.advance(1, MIX_FRAMES as u64, &mut |_| {});
        mixer.lane_discontinuity(1, 0);
        mixer.push_scene(1, &resample_88, Some(&geometry)).unwrap();
        mixer.advance(1, MIX_FRAMES as u64, &mut |_| {});
        mixer.lane_discontinuity(1, 0);
        mixer.push_scene(1, &resample_96, Some(&geometry)).unwrap();
        mixer.advance(1, MIX_FRAMES as u64, &mut |_| {});
        mixer.lane_discontinuity(1, 0);
        mixer.push_scene(1, &ddp, Some(&geometry)).unwrap();
        mixer.push_scene(1, &ddp, Some(&geometry)).unwrap();
        mixer.lane_stop(1, &mut |_| {});
        mixer.finish(&mut |_| {});
    });
    assert!(mixer.dropped_frames() > 0);
}

#[test]
fn layout_change_and_lazy_pending_conversion_do_not_allocate() {
    let first = geometry();
    let second = side_geometry();
    let signals = [vec![0.25; MIX_FRAMES]];
    let mut objects = [Object::SILENT; MAX_OBJECTS];
    let scene = block(MIX_RATE, &signals, &mut objects);
    let mut mixer = SceneMixer::new(MixerConfig::default());
    mixer.push_scene(0, &scene, Some(&first)).unwrap();

    assert_no_alloc(|| {
        mixer.change_layout(Some(&first), &second);
        mixer.advance(0, MIX_FRAMES as u64, &mut |bed| {
            assert_eq!(bed.directions, 1);
        });
    });
}

#[test]
fn discontinuity_pcm_path_does_not_allocate() {
    let format = pcm_format(MIX_RATE);
    let pcm = vec![0_u8; MIX_FRAMES * 2 * 4];
    let mut chain = Chain::new(ChainConfig::default());
    chain.push_record(
        Record::FormatChange {
            endpoint: 0,
            epoch: 1,
            repeat: false,
            qpc: 0,
            format,
        },
        &mut |_| {},
    );
    assert_no_alloc(|| {
        chain.push_record(
            Record::Data {
                endpoint: 0,
                epoch: 1,
                flags: RECORD_FLAG_DISCONTINUITY,
                pos: 0,
                qpc: 1,
                qpc_written: 1,
                bytes: &pcm,
            },
            &mut |_| {},
        );
    });
}

fn stereo_data(endpoint: u16, epoch: u32, pcm: &[u8], qpc: u64) -> Record<'_> {
    Record::Data {
        endpoint,
        epoch,
        flags: 0,
        pos: 0,
        qpc,
        qpc_written: 0,
        bytes: pcm,
    }
}

#[test]
fn upmix_stages_do_not_allocate() {
    const QPC: u64 = 10_000_000;
    let upmix_stage = |endpoint: u16, upmixer: Box<dyn engine::Upmixer>| {
        Some(Box::new(UpmixStage::new(endpoint, upmixer, QPC).unwrap()))
    };
    // Endpoint 1's upmixer is a ring whose host runs on this thread between
    // records, outside the guard.
    let geometry = Geometry {
        lanes: 1,
        input_channels: 2,
        output_channels: 6,
        max_frames: MIX_FRAMES as u32,
        sample_rate: MIX_RATE,
        latency_bound_frames: MIX_RATE,
    };
    let storage = Arc::new(RegionBuf::new(geometry).unwrap());
    let ready = InProcessEvent::new();
    let mut producer = Producer::new(storage.region(), [None, None], &ready);
    producer.announce(geometry, 1).unwrap();
    let publisher = producer.publisher().unwrap();
    let mut consumer = Consumer::new(storage.region(), &ready, [None, None]);
    consumer.validate_and_ready(2).unwrap();
    let mut slot = vec![0.0; geometry.samples_per_slot()];
    let mut host_matrix = MatrixUpmixer::stereo_to_5_1(MIX_RATE, 0);
    let mut host_result = vec![0.0; 6 * MIX_FRAMES];
    let mut host = |consumer: &Consumer<'_>| {
        while let Some(sequence) = consumer.next_published(0) {
            consumer.slot_mut(0, sequence, &mut slot, |samples| {
                host_matrix.matrix_block(&samples[..2 * MIX_FRAMES], &mut host_result);
                samples.copy_from_slice(&host_result);
            });
            consumer.complete(0, sequence);
        }
    };
    let ring = RingUpmixer::new(
        Box::new(Arc::clone(&storage)),
        Box::new(InProcessEvent::new()),
        publisher,
        MatrixUpmixer::STEREO.to_vec(),
        MatrixUpmixer::OUTPUTS_5_1.to_vec(),
        0,
    )
    .unwrap();

    let mut chain = Chain::with_qpc_frequency(
        ChainConfig {
            arrival_slack_frames: 0,
            ..ChainConfig::default()
        },
        QPC,
    );
    chain.set_prepared(prepared());
    let mut worker = chain.take_decode_worker().unwrap();
    // Endpoint 0's matrix misses blocks 3 to 5 and 9 and returns NaN for 7.
    let matrix = MatrixUpmixer::stereo_to_5_1(MIX_RATE, 37)
        .with_misses(&[3, 4, 5, 9])
        .with_nan(7);
    assert!(
        chain
            .set_upmixer(0, upmix_stage(0, Box::new(matrix)))
            .is_none()
    );
    assert!(
        chain
            .set_upmixer(1, upmix_stage(1, Box::new(ring)))
            .is_none()
    );
    let mut spare = upmix_stage(0, Box::new(MatrixUpmixer::stereo_lfe_to_5_1(MIX_RATE, 480)));

    let mut pcm = vec![0_u8; MIX_FRAMES * 2 * 4];
    for (index, sample) in pcm.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        *sample = (((index % 97) as f32 - 48.0) / 200.0).to_le_bytes();
    }
    let block_ticks = MIX_FRAMES as u64 * QPC / u64::from(MIX_RATE);
    let mut qpc = 1_000_000;
    let mut retired = Vec::with_capacity(4);
    for block in 0..40 {
        assert_no_alloc(|| {
            if block == 0 {
                for endpoint in [0, 1] {
                    chain.push_record(
                        Record::FormatChange {
                            endpoint,
                            epoch: 1,
                            repeat: false,
                            qpc,
                            format: pcm_format(MIX_RATE),
                        },
                        &mut |_| {},
                    );
                }
            }
            qpc += block_ticks;
            match block {
                // The output kind's switch, off and on while both play.
                20 => chain.set_upmix_apply(false),
                24 => chain.set_upmix_apply(true),
                // Another upmixer replaces endpoint 0's while it plays.
                30 => {
                    if let Some(old) = chain.set_upmixer(0, spare.take()) {
                        retired.push(old);
                    }
                }
                _ => {}
            }
            chain.push_record(stereo_data(0, 1, &pcm, qpc), &mut |_| {});
            chain.push_record(stereo_data(1, 1, &pcm, qpc), &mut |_| {});
        });
        host(&consumer);
    }
    assert_eq!(retired.len(), 1);
    let replaced = retired[0].status();
    // Blocks count from each restart: 3, 4, 5 and 9 before the switch went
    // off, then 3 and 4 again before the swap.
    assert_eq!(replaced.missed_blocks, 6);
    assert_eq!(replaced.nonfinite_samples, 6 * MIX_FRAMES as u64);
    let ring_status = chain.upmix_status(1).unwrap();
    assert!(ring_status.upmixing && ring_status.delivered_blocks > 20);

    // A 5.1 E-AC-3 stream on the ring's endpoint: transparent, placed from
    // the decode thread's deliveries.
    let fixture = std::fs::read(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../decode/tests/fixtures/ddp/tones-5.1.eac3"),
    )
    .unwrap();
    let mut bursts = Vec::new();
    for frame in 1..4 {
        let mut burst = Vec::new();
        bitstream::frame_burst(
            bitstream::DataType::Eac3,
            &fixture[frame * 2560..(frame + 1) * 2560],
            24_576,
            &mut burst,
        );
        bursts.push(burst);
    }
    let eac3 = SourceFormat::Bitstream(BitstreamFormat {
        codec: BitstreamCodec::Eac3,
        container_rate: 192_000,
        container_channels: 2,
        encoded_rate: MIX_RATE,
        encoded_channels: 6,
    });
    let burst_ticks = 1_536 * QPC / u64::from(MIX_RATE);
    for (index, burst) in bursts.iter().enumerate() {
        assert_no_alloc(|| {
            if index == 0 {
                chain.push_record(
                    Record::FormatChange {
                        endpoint: 1,
                        epoch: 2,
                        repeat: false,
                        qpc,
                        format: eac3,
                    },
                    &mut |_| {},
                );
            }
            qpc += burst_ticks;
            chain.push_record(
                Record::Data {
                    endpoint: 1,
                    epoch: 2,
                    flags: 0,
                    pos: 0,
                    qpc,
                    qpc_written: 0,
                    bytes: burst,
                },
                &mut |_| {},
            );
            chain.push_record(stereo_data(0, 1, &pcm, qpc), &mut |_| {});
        });
        while worker.run_one() {}
        assert_no_alloc(|| chain.idle(qpc, &mut |_| {}));
    }
    let surround = chain.upmix_status(1).unwrap();
    assert_eq!(surround.bypass, Some(UpmixBypass::NativeSurround));
    assert!(chain.upmix_status(0).unwrap().upmixing);
}

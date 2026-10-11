#![forbid(unsafe_code)]

use bio_types::{PcmFormat, Position, SampleType, SceneBlock};
use decode::{
    DecodeConfig, DecodeError,
    pcm::{PcmDecoder, default_mask, speaker_position},
};
use proptest::prelude::*;

fn format(bits: u16, valid: u16, sample_type: SampleType, channels: u16) -> PcmFormat {
    PcmFormat {
        channels,
        sample_rate: 48_000,
        bits_per_sample: bits,
        valid_bits: valid,
        sample_type,
        channel_mask: 0,
    }
}

#[derive(Debug, PartialEq)]
struct OwnedScene {
    epoch: u32,
    qpc: u64,
    rate: u32,
    signals: Vec<Vec<f32>>,
    positions: Vec<Position>,
    lfe: Option<Vec<f32>>,
}

impl From<&SceneBlock<'_>> for OwnedScene {
    fn from(scene: &SceneBlock<'_>) -> Self {
        assert!(scene.is_consistent());
        assert!(
            scene
                .objects
                .iter()
                .all(|o| o.gain == 1.0 && o.spread == 0.0)
        );
        Self {
            epoch: scene.epoch,
            qpc: scene.source_qpc,
            rate: scene.rate,
            signals: scene.objects.iter().map(|o| o.signal.to_vec()).collect(),
            positions: scene.objects.iter().map(|o| o.position).collect(),
            lfe: scene.lfe.map(<[f32]>::to_vec),
        }
    }
}

fn check_samples(format: PcmFormat, bytes: &[u8], expected: &[f32]) {
    let mut decoder = PcmDecoder::new(DecodeConfig::default());
    decoder.configure(format).unwrap();
    let block_bytes = 480 * format.bytes_per_frame();
    let input: Vec<u8> = bytes.iter().copied().cycle().take(block_bytes).collect();
    let mut seen = 0;
    decoder
        .push(&input, 41, 7, &mut |scene| {
            seen += 1;
            assert_eq!(
                (scene.frames, scene.rate, scene.epoch, scene.source_qpc),
                (480, 48_000, 7, 41)
            );
            assert_eq!(scene.objects.len(), 1);
            assert!(scene.lfe.is_none());
            for (i, &sample) in scene.objects[0].signal.iter().enumerate() {
                assert_eq!(sample, expected[i % expected.len()], "sample {i}");
                assert!((-1.0..1.0).contains(&sample));
            }
        })
        .unwrap();
    assert_eq!(seen, 1);
}

#[test]
fn int16_samples_are_signed_little_endian() {
    let input: Vec<u8> = [i16::MIN, -16384, 0, 16384, i16::MAX]
        .into_iter()
        .flat_map(i16::to_le_bytes)
        .collect();
    check_samples(
        format(16, 16, SampleType::Int, 1),
        &input,
        &[-1.0, -0.5, 0.0, 0.5, 32767.0 / 32768.0],
    );
}

#[test]
fn packed_int24_sign_extends() {
    check_samples(
        format(24, 24, SampleType::Int, 1),
        &[0, 0, 128, 0, 0, 192, 0, 0, 0, 0, 0, 64, 255, 255, 127],
        &[-1.0, -0.5, 0.0, 0.5, 8388607.0 / 8388608.0],
    );
}

#[test]
fn int32_maximum_stays_below_one() {
    let input: Vec<u8> = [i32::MIN, -1073741824, 0, 1073741824, i32::MAX]
        .into_iter()
        .flat_map(i32::to_le_bytes)
        .collect();
    check_samples(
        format(32, 32, SampleType::Int, 1),
        &input,
        &[-1.0, -0.5, 0.0, 0.5, f32::from_bits(1.0_f32.to_bits() - 1)],
    );
}

#[test]
fn int24_in32_ignores_low_padding_bits() {
    let input: Vec<u8> = [
        i32::MIN + 255,
        -1073741824 + 77,
        255,
        1073741824 + 77,
        i32::MAX,
    ]
    .into_iter()
    .flat_map(i32::to_le_bytes)
    .collect();
    check_samples(
        format(32, 24, SampleType::Int, 1),
        &input,
        &[-1.0, -0.5, 0.0, 0.5, 8388607.0 / 8388608.0],
    );
}

#[test]
fn float32_clamps_and_silences_nan() {
    let input: Vec<u8> = [
        -1.0_f32,
        -0.25,
        0.0,
        0.25,
        1.0,
        f32::NAN,
        f32::INFINITY,
        f32::NEG_INFINITY,
    ]
    .into_iter()
    .flat_map(f32::to_le_bytes)
    .collect();
    let max = f32::from_bits(1.0_f32.to_bits() - 1);
    check_samples(
        format(32, 32, SampleType::Float, 1),
        &input,
        &[-1.0, -0.25, 0.0, 0.25, max, 0.0, max, -1.0],
    );
}

#[test]
fn default_masks_follow_documented_windows_order() {
    for (channels, mask) in [
        (1, 4),
        (2, 3),
        (3, 7),
        (4, 0x33),
        (5, 0x37),
        (6, 0x3f),
        (8, 0x63f),
    ] {
        assert_eq!(default_mask(channels), Some(mask));
        let mut decoder = PcmDecoder::new(DecodeConfig::default());
        decoder
            .configure(format(16, 16, SampleType::Int, channels))
            .unwrap();
    }
    for channels in [0, 7, 9, u16::MAX] {
        assert_eq!(default_mask(channels), None);
    }
}

#[test]
fn speaker_bits_cover_standard_positions_only() {
    for (bit, az, el) in [
        (1, 30.0, 0.0),
        (2, -30.0, 0.0),
        (4, 0.0, 0.0),
        (0x10, 150.0, 0.0),
        (0x20, -150.0, 0.0),
        (0x40, 15.0, 0.0),
        (0x80, -15.0, 0.0),
        (0x100, 180.0, 0.0),
        (0x200, 90.0, 0.0),
        (0x400, -90.0, 0.0),
        (0x800, 0.0, 90.0),
        (0x1000, 45.0, 45.0),
        (0x2000, 0.0, 45.0),
        (0x4000, -45.0, 45.0),
        (0x8000, 135.0, 45.0),
        (0x10000, 180.0, 45.0),
        (0x20000, -135.0, 45.0),
    ] {
        assert_eq!(speaker_position(bit), Some(Position::from_angles(az, el)));
    }
    for bit in [0, 3, 8, 0x40000, u32::MAX] {
        assert_eq!(speaker_position(bit), None);
    }
}

#[test]
fn seven_one_mask_routes_in_bit_order_and_separates_lfe() {
    let mut decoder = PcmDecoder::new(DecodeConfig::default());
    let f = PcmFormat {
        channel_mask: 0x63f,
        ..format(16, 16, SampleType::Int, 8)
    };
    decoder.configure(f).unwrap();
    let input: Vec<u8> = (0..480)
        .flat_map(|_| (1_i16..=8).flat_map(|i| (i * 1024).to_le_bytes()))
        .collect();
    let mut seen = 0;
    decoder
        .push(&input, 9, 4, &mut |scene| {
            seen += 1;
            assert_eq!(scene.objects.len(), 7);
            for (i, (channel, az)) in [
                (1, 30.0),
                (2, -30.0),
                (3, 0.0),
                (5, 150.0),
                (6, -150.0),
                (7, 90.0),
                (8, -90.0),
            ]
            .into_iter()
            .enumerate()
            {
                assert_eq!(scene.objects[i].position, Position::from_angles(az, 0.0));
                assert!(
                    scene.objects[i]
                        .signal
                        .iter()
                        .all(|&s| s == channel as f32 / 32.0)
                );
            }
            assert_eq!(scene.lfe.unwrap(), &[0.125; 480]);
        })
        .unwrap();
    assert_eq!(seen, 1);
}

#[test]
fn configure_rejects_invalid_shapes_and_deselects_old_format() {
    let good = format(16, 16, SampleType::Int, 2);
    let mut decoder = PcmDecoder::new(DecodeConfig { max_frames: 480 });
    for bad in [
        PcmFormat {
            channels: 0,
            ..good
        },
        PcmFormat {
            channels: 9,
            ..good
        },
        PcmFormat {
            channels: 7,
            ..good
        },
        PcmFormat {
            channel_mask: 1,
            ..good
        },
        PcmFormat {
            channel_mask: 7,
            ..good
        },
        PcmFormat {
            channel_mask: 0x80000001,
            ..good
        },
        PcmFormat {
            sample_rate: 96_000,
            ..good
        },
        PcmFormat {
            sample_rate: 99,
            ..good
        },
        PcmFormat {
            bits_per_sample: 8,
            ..good
        },
        PcmFormat {
            valid_bits: 0,
            ..good
        },
        PcmFormat {
            valid_bits: 17,
            ..good
        },
        PcmFormat {
            sample_type: SampleType::Float,
            ..good
        },
        PcmFormat {
            bits_per_sample: 32,
            valid_bits: 24,
            sample_type: SampleType::Float,
            ..good
        },
    ] {
        decoder.configure(good).unwrap();
        assert!(
            matches!(
                decoder.configure(bad),
                Err(DecodeError::Unsupported { codec: "PCM", .. })
            ),
            "{bad:?}"
        );
        assert_eq!(
            decoder.push(&[], 0, 0, &mut |_| panic!("no scene")),
            Err(DecodeError::NoFormat)
        );
    }
    decoder
        .configure(PcmFormat {
            channels: 7,
            channel_mask: 0x13f,
            ..good
        })
        .unwrap();
}

#[test]
fn partial_first_frame_keeps_its_first_byte_qpc() {
    let mut decoder = PcmDecoder::new(DecodeConfig::default());
    decoder
        .configure(format(16, 16, SampleType::Int, 2))
        .unwrap();
    let mut scenes = Vec::new();
    let mut out = |s: &SceneBlock<'_>| scenes.push(OwnedScene::from(s));
    decoder.push(&[0], 11, 3, &mut out).unwrap();
    decoder.push(&[], 22, 3, &mut out).unwrap();
    decoder
        .push(&vec![0; 1919 + 1920 + 1], 33, 3, &mut out)
        .unwrap();
    decoder.push(&vec![0; 1919], 44, 3, &mut out).unwrap();
    assert_eq!(
        scenes.iter().map(|s| s.qpc).collect::<Vec<_>>(),
        [11, 33, 33]
    );
}

#[test]
fn reset_reconfigure_and_epoch_change_discard_remainders() {
    let mut decoder = PcmDecoder::new(DecodeConfig::default());
    let f = PcmFormat {
        sample_rate: 44_100,
        ..format(16, 16, SampleType::Int, 1)
    };
    decoder.configure(f).unwrap();
    for action in 0..3 {
        decoder
            .push(&[255; 881], 1, 1, &mut |_| panic!("incomplete"))
            .unwrap();
        let epoch = match action {
            0 => {
                decoder.reset();
                1
            }
            1 => {
                decoder.configure(f).unwrap();
                1
            }
            _ => 2,
        };
        let mut seen = 0;
        decoder
            .push(&[0; 882], 2, epoch, &mut |s| {
                seen += 1;
                assert_eq!((s.frames, s.source_qpc, s.epoch), (441, 2, epoch));
                assert_eq!(s.objects[0].signal, &[0.0; 441]);
            })
            .unwrap();
        assert_eq!(seen, 1);
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]
    #[test]
    fn arbitrary_byte_chunking_preserves_blocks(
        seed in prop::collection::vec(any::<u8>(), 1..257),
        cuts in prop::collection::vec(1_usize..511, 1..31),
        variant in 0..5_usize,
        channels in prop::sample::select(vec![1_u16, 2, 4, 6, 8]),
    ) {
        let (bits, valid, kind) = [(16,16,SampleType::Int), (24,24,SampleType::Int),
            (32,32,SampleType::Int), (32,24,SampleType::Int), (32,32,SampleType::Float)][variant];
        let f = format(bits, valid, kind, channels);
        let length = 480 * 3 * f.bytes_per_frame() + 1;
        let input: Vec<u8> = seed.into_iter().cycle().take(length).collect();
        let mut whole = PcmDecoder::new(DecodeConfig::default());
        let mut chunks = PcmDecoder::new(DecodeConfig::default());
        whole.configure(f).unwrap(); chunks.configure(f).unwrap();
        let mut expected = Vec::new(); let mut actual = Vec::new();
        whole.push(&input, 9, 3, &mut |s| expected.push(OwnedScene::from(s))).unwrap();
        let mut offset = 0;
        for size in cuts.into_iter().cycle() {
            let end = (offset + size).min(input.len());
            chunks.push(&input[offset..end], 9, 3, &mut |s| actual.push(OwnedScene::from(s))).unwrap();
            offset = end;
            if offset == input.len() { break; }
        }
        prop_assert_eq!(actual, expected);
    }
}

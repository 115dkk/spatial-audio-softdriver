#![forbid(unsafe_code)]

use decode::{
    DecodeConfig, DecodeError,
    direct::{DirectLayout, DirectLayoutOverride, DirectLayouts, parse_speaker, preset},
    pcm::{PcmDecoder, default_mask, speaker_position},
};
use proptest::prelude::*;
use ssr_types::{PcmFormat, Position, SampleType, SceneBlock, Speaker};

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
fn ffmpeg_native_direct_defaults_and_presets_have_the_documented_order() {
    use Speaker::*;
    let defaults = DirectLayouts::default();
    for (channels, expected) in [
        (1, &[C][..]),
        (2, &[L, R]),
        (3, &[L, R, C]),
        (4, &[L, R, Lrs, Rrs]),
        (5, &[L, R, C, Lrs, Rrs]),
        (6, &[L, R, C, Lfe, Lrs, Rrs]),
        (8, &[L, R, C, Lfe, Lrs, Rrs, Lss, Rss]),
        (10, &[L, R, C, Lfe, Lrs, Rrs, Lss, Rss, Tfl, Tfr]),
        (12, &[L, R, C, Lfe, Lrs, Rrs, Lss, Rss, Tfl, Tfr, Trl, Trr]),
        (
            14,
            &[L, R, C, Lfe, Lrs, Rrs, Lc, Rc, Lss, Rss, Tfl, Tfr, Trl, Trr],
        ),
        (
            16,
            &[
                L, R, C, Lfe, Lrs, Rrs, Lc, Rc, Lss, Rss, Tfl, Tfr, Trl, Trr, Tsl, Tsr,
            ],
        ),
    ] {
        assert_eq!(defaults.get(channels).unwrap().speakers(), expected);
    }
    for channels in [7, 9, 11, 13, 15, 17, 32] {
        assert_eq!(defaults.get(channels), None);
    }
    // Where Windows assumes a mask for mask zero, the direct default is that
    // mask in bit order, so both readings of the same stream agree.
    for channels in 1..=8 {
        let Some(mut mask) = default_mask(channels) else {
            continue;
        };
        let mut from_mask = Vec::new();
        while mask != 0 {
            from_mask.push(Speaker::from_wave_bit(mask.isolate_lowest_one()).unwrap());
            mask &= mask - 1;
        }
        assert_eq!(defaults.get(channels).unwrap().speakers(), from_mask);
    }
    assert_eq!(
        preset("5.1.4").unwrap().speakers(),
        &[L, R, C, Lfe, Lrs, Rrs, Tfl, Tfr, Trl, Trr]
    );
}

#[test]
fn direct_layout_argument_validates_count_names_and_duplicates() {
    let value: DirectLayoutOverride = "16=9.1.6".parse().unwrap();
    assert_eq!(value.channels, 16);
    assert_eq!(value.layout, preset("9.1.6").unwrap());
    let value: DirectLayoutOverride = "3=l,R,c".parse().unwrap();
    assert_eq!(
        value.layout.speakers(),
        &[Speaker::L, Speaker::R, Speaker::C]
    );
    for bad in [
        "0=mono",
        "33=stereo",
        "3=stereo",
        "2=L,L",
        "2=L,nope",
        "2",
        "2=FL,l",
        "2=LFE,LFE2",
    ] {
        assert!(bad.parse::<DirectLayoutOverride>().is_err(), "{bad}");
    }
    // FFmpeg's channel names, as Impulcifer users write them, for a 9.1.6
    // whose extra front pair is the wides rather than FFmpeg's FLC/FRC.
    use Speaker::*;
    let value: DirectLayoutOverride = "16=FL,FR,FC,LFE,BL,BR,WL,WR,SL,SR,TFL,TFR,TBL,TBR,TSL,TSR"
        .parse()
        .unwrap();
    assert_eq!(
        value.layout.speakers(),
        &[
            L, R, C, Lfe, Lrs, Rrs, Lw, Rw, Lss, Rss, Tfl, Tfr, Trl, Trr, Tsl, Tsr
        ]
    );
    let value: DirectLayoutOverride = "6=flc,frc,bc,tbc,tc,tfc".parse().unwrap();
    assert_eq!(value.layout.speakers(), &[Lc, Rc, Cs, Trc, Tc, Tfc]);
    // Every alias names the position of the FFmpeg channel bit that shares
    // the WAVE bit, where one exists.
    for (alias, bit) in [
        ("FL", 0x1),
        ("FR", 0x2),
        ("FC", 0x4),
        ("BL", 0x10),
        ("BR", 0x20),
        ("FLC", 0x40),
        ("FRC", 0x80),
        ("BC", 0x100),
        ("SL", 0x200),
        ("SR", 0x400),
        ("TC", 0x800),
        ("TFL", 0x1000),
        ("TFC", 0x2000),
        ("TFR", 0x4000),
        ("TBL", 0x8000),
        ("TBC", 0x10000),
        ("TBR", 0x20000),
    ] {
        assert_eq!(parse_speaker(alias), Speaker::from_wave_bit(bit), "{alias}");
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
fn seven_one_four_pcm_preserves_height_channels_and_lfe_across_fragments() {
    for (bits, kind) in [
        (16, SampleType::Int),
        (32, SampleType::Int),
        (32, SampleType::Float),
    ] {
        let mut decoder = PcmDecoder::new(DecodeConfig::default());
        decoder
            .configure(PcmFormat {
                channel_mask: 0x2d63f,
                ..format(bits, bits, kind, 12)
            })
            .unwrap();
        let mut input = Vec::new();
        for _ in 0..480 {
            for channel in 1..=12 {
                match (bits, kind) {
                    (16, _) => input.extend_from_slice(&((channel * 1024) as i16).to_le_bytes()),
                    (32, SampleType::Int) => {
                        input.extend_from_slice(&(channel * (1_i32 << 26)).to_le_bytes())
                    }
                    (32, SampleType::Float) => {
                        input.extend_from_slice(&(channel as f32 / 32.0).to_le_bytes())
                    }
                    _ => unreachable!(),
                }
            }
        }
        let mut seen = 0;
        for chunk in input.chunks(137) {
            decoder
                .push(chunk, 9, 4, &mut |scene| {
                    seen += 1;
                    assert_eq!(
                        (scene.objects.len(), scene.frames, scene.source_qpc),
                        (11, 480, 9)
                    );
                    for (object, (channel, az, el)) in scene.objects.iter().zip([
                        (1, 30.0, 0.0),
                        (2, -30.0, 0.0),
                        (3, 0.0, 0.0),
                        (5, 150.0, 0.0),
                        (6, -150.0, 0.0),
                        (7, 90.0, 0.0),
                        (8, -90.0, 0.0),
                        (9, 45.0, 45.0),
                        (10, -45.0, 45.0),
                        (11, 135.0, 45.0),
                        (12, -135.0, 45.0),
                    ]) {
                        assert_eq!(object.position, Position::from_angles(az, el));
                        assert!(object.signal.iter().all(|&s| s == channel as f32 / 32.0));
                    }
                    assert_eq!(scene.lfe.unwrap(), &[0.125; 480]);
                })
                .unwrap();
        }
        assert_eq!(seen, 1);
    }
}

#[test]
fn direct_pcm_routes_sixteen_channels_and_lfe_in_ffmpeg_native_order() {
    let mut decoder = PcmDecoder::new(DecodeConfig::default());
    let f = format(16, 16, SampleType::Int, 16);
    decoder.configure(f).unwrap();
    let input: Vec<u8> = (0..480)
        .flat_map(|_| (1_i16..=16).flat_map(|value| (value * 1024).to_le_bytes()))
        .collect();
    decoder
        .push(&input, 3, 9, &mut |scene| {
            assert_eq!(scene.objects.len(), 15);
            assert_eq!(scene.lfe.unwrap(), &[0.125; 480]);
            for (object, speaker) in scene.objects.iter().zip(
                preset("9.1.6")
                    .unwrap()
                    .speakers()
                    .iter()
                    .copied()
                    .filter(|speaker| *speaker != Speaker::Lfe),
            ) {
                assert_eq!(object.position, speaker.position().unwrap());
            }
        })
        .unwrap();
}

#[test]
fn custom_direct_layout_enables_eleven_and_thirty_two_channels() {
    let mut speakers = Vec::new();
    let all = [
        Speaker::L,
        Speaker::R,
        Speaker::C,
        Speaker::Lfe,
        Speaker::Lrs,
        Speaker::Rrs,
        Speaker::Lc,
        Speaker::Rc,
        Speaker::Cs,
        Speaker::Lss,
        Speaker::Rss,
        Speaker::Tc,
        Speaker::Tfl,
        Speaker::Tfc,
        Speaker::Tfr,
        Speaker::Trl,
        Speaker::Trc,
        Speaker::Trr,
        Speaker::Ls,
        Speaker::Rs,
        Speaker::Lw,
        Speaker::Rw,
        Speaker::Tsl,
        Speaker::Tsr,
    ];
    speakers.extend(all.iter().copied());
    speakers.extend(
        [
            Speaker::L,
            Speaker::R,
            Speaker::C,
            Speaker::Lrs,
            Speaker::Rrs,
            Speaker::Lc,
            Speaker::Rc,
            Speaker::Cs,
        ]
        .iter()
        .copied(),
    );
    // Duplicate positions are legal in the programmatic type, which lets a
    // caller describe all 32 direct channels; the CLI rejects duplicates.
    let mut direct = DirectLayouts::empty();
    direct
        .set(32, DirectLayout::new(&speakers).unwrap())
        .unwrap();
    direct
        .set(11, DirectLayout::new(&speakers[..11]).unwrap())
        .unwrap();
    for channels in [11, 32] {
        let mut decoder = PcmDecoder::new(DecodeConfig {
            direct,
            ..DecodeConfig::default()
        });
        decoder
            .configure(format(32, 32, SampleType::Int, channels))
            .unwrap();
    }
    let mut defaults = PcmDecoder::new(DecodeConfig::default());
    let error = defaults
        .configure(format(16, 16, SampleType::Int, 11))
        .unwrap_err();
    assert!(matches!(error, DecodeError::NoDirectLayout(11)), "{error}");
    assert!(
        defaults
            .configure(format(16, 16, SampleType::Int, 33))
            .is_err()
    );
}

#[test]
fn every_wave_speaker_bit_and_common_masks_route_in_bit_order() {
    for (channels, mask) in [(18, 0x3ffff), (7, 0x70f), (6, 0x60f), (8, 0xff)] {
        let mut decoder = PcmDecoder::new(DecodeConfig::default());
        decoder
            .configure(PcmFormat {
                channel_mask: mask,
                ..format(16, 16, SampleType::Int, channels)
            })
            .unwrap();
        let input: Vec<u8> = (0..480)
            .flat_map(|_| {
                (1_i16..=channels as i16).flat_map(|channel| (channel * 1024).to_le_bytes())
            })
            .collect();
        let ordered: Vec<Speaker> = (0..18)
            .filter_map(|bit| {
                let value = 1_u32 << bit;
                (mask & value != 0).then(|| Speaker::from_wave_bit(value).unwrap())
            })
            .collect();
        let mut seen = 0;
        decoder
            .push(&input, 11, 7, &mut |scene| {
                seen += 1;
                let expected_objects = ordered
                    .iter()
                    .copied()
                    .filter(|speaker| *speaker != Speaker::Lfe);
                assert_eq!(
                    scene.objects.len(),
                    channels as usize - usize::from(mask & 8 != 0)
                );
                for (object, (channel, speaker)) in scene.objects.iter().zip(
                    ordered
                        .iter()
                        .copied()
                        .enumerate()
                        .filter(|(_, speaker)| *speaker != Speaker::Lfe),
                ) {
                    assert_eq!(object.position, speaker.position().unwrap());
                    assert!(
                        object
                            .signal
                            .iter()
                            .all(|&sample| sample == (channel + 1) as f32 / 32.0)
                    );
                }
                assert_eq!(
                    scene.lfe.map(|signal| signal[0]),
                    ordered
                        .iter()
                        .position(|speaker| *speaker == Speaker::Lfe)
                        .map(|channel| (channel + 1) as f32 / 32.0)
                );
                assert_eq!(expected_objects.count(), scene.objects.len());
            })
            .unwrap();
        assert_eq!(seen, 1);
    }
}

#[test]
fn configure_rejects_invalid_shapes_and_deselects_old_format() {
    let good = format(16, 16, SampleType::Int, 2);
    let mut decoder = PcmDecoder::new(DecodeConfig {
        max_frames: 480,
        ..DecodeConfig::default()
    });
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
            channels: 33,
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
                // Mask zero with a count that has no direct layout (9, 7).
                Err(DecodeError::Unsupported { codec: "PCM", .. }
                    | DecodeError::NoDirectLayout(7 | 9))
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

/// The chain emits a lane's complete frames after every chunk, so the lane's
/// placed audio never trails the bytes it received by part of a block.
#[test]
fn emit_buffered_emits_complete_frames_and_keeps_a_partial_frame() {
    let mut decoder = PcmDecoder::new(DecodeConfig::default());
    let f = format(16, 16, SampleType::Int, 2);
    decoder.configure(f).unwrap();
    let frame = |value: i16| [value.to_le_bytes(), value.to_le_bytes()].concat();
    let mut scenes = Vec::new();
    let mut out = |s: &SceneBlock<'_>| scenes.push(OwnedScene::from(s));

    decoder.emit_buffered(&mut out);
    let mut first = (0..100).flat_map(|i| frame(i * 10)).collect::<Vec<_>>();
    first.push(0x34);
    decoder.push(&first, 7, 5, &mut out).unwrap();
    decoder.emit_buffered(&mut out);
    // The half frame from the QPC-7 record starts the next scene.
    let mut second = vec![0x12, 0x34, 0x12];
    second.extend((101..480).flat_map(|i| frame(i * 10)));
    decoder.push(&second, 9, 5, &mut out).unwrap();
    decoder.emit_buffered(&mut out);
    decoder.emit_buffered(&mut out);

    assert_eq!(
        scenes
            .iter()
            .map(|s| (s.signals[0].len(), s.qpc, s.epoch))
            .collect::<Vec<_>>(),
        [(100, 7, 5), (380, 7, 5)]
    );
    let samples = scenes
        .iter()
        .flat_map(|s| s.signals[0].iter().copied())
        .collect::<Vec<_>>();
    let expected = (0..480)
        .map(|i| {
            let value = if i == 100 { 0x1234 } else { i * 10 };
            f32::from(value as i16) / 32_768.0
        })
        .collect::<Vec<_>>();
    assert_eq!(samples, expected);
}

#[test]
fn flush_emits_complete_pcm_residuals_and_discards_partial_frames() {
    for frames in [96, 479, 481, 576] {
        let mut decoder = PcmDecoder::new(DecodeConfig::default());
        let f = format(16, 16, SampleType::Int, 2);
        decoder.configure(f).unwrap();
        let mut input = vec![0_u8; frames * f.bytes_per_frame() + 1];
        for frame in 0..frames {
            let value = (frame as i16).wrapping_mul(17);
            let offset = frame * f.bytes_per_frame();
            input[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
            input[offset + 2..offset + 4].copy_from_slice(&value.to_le_bytes());
        }
        input[frames * f.bytes_per_frame()] = 0xff;

        let mut scenes = Vec::new();
        decoder
            .push(&input, 123, 9, &mut |scene| {
                scenes.push(OwnedScene::from(scene))
            })
            .unwrap();
        decoder
            .flush(&mut |scene| scenes.push(OwnedScene::from(scene)))
            .unwrap();

        let emitted = scenes
            .iter()
            .map(|scene| scene.signals[0].len())
            .sum::<usize>();
        assert_eq!(emitted, frames);
        assert!(
            scenes
                .iter()
                .all(|scene| { scene.epoch == 9 && scene.qpc == 123 && scene.rate == 48_000 })
        );
        assert!(
            scenes
                .last()
                .is_some_and(|scene| scene.signals[0].len() <= 480)
        );

        let before = scenes.len();
        decoder
            .flush(&mut |scene| scenes.push(OwnedScene::from(scene)))
            .unwrap();
        assert_eq!(scenes.len(), before);
    }
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

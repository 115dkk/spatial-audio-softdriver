#![forbid(unsafe_code)]

use bitstream::{Burst, DataType};
use decode::{DecodeConfig, ddp::DdpDecoder};
use ssr_types::Position;

const FIXTURE: &[u8] = include_bytes!("fixtures/ddp/tones-5.1.eac3");

fn burst(payload: &[u8]) -> Burst<'_> {
    Burst {
        data_type: DataType::Eac3,
        pc: 21,
        payload,
        start: 0,
    }
}

fn amplitude(signal: &[f32], frequency: f64) -> f64 {
    let (mut re, mut im) = (0.0, 0.0);
    for (i, &sample) in signal.iter().enumerate() {
        let phase = std::f64::consts::TAU * frequency * i as f64 / 48_000.0;
        re += f64::from(sample) * phase.cos();
        im += f64::from(sample) * phase.sin();
    }
    2.0 * re.hypot(im) / signal.len() as f64
}

#[test]
fn ddp_six_channel_tones_have_expected_positions_and_timing() {
    let mut decoder = DdpDecoder::new(DecodeConfig::default());
    let mut bytes = FIXTURE;
    let mut count = 0;
    while !bytes.is_empty() {
        let header = eac3::parse_header(bytes).unwrap();
        assert_eq!(header.samples, 1536);
        let (frame, rest) = bytes.split_at(header.frame_size);
        bytes = rest;
        let qpc = 1000 + count * 320;
        let mut emitted = 0;
        decoder
            .push(&burst(frame), qpc, 7, &mut |scene| {
                emitted += 1;
                assert!(scene.is_consistent());
                assert_eq!(
                    (scene.frames, scene.rate, scene.epoch, scene.source_qpc),
                    (1536, 48_000, 7, qpc)
                );
                assert_eq!(scene.objects.len(), 5);
                // Native AC-3 order is L, C, R, Ls, Rs, unlike WAV mask order.
                for (object, (az, frequency)) in scene.objects.iter().zip([
                    (30.0, 375.0),
                    (0.0, 750.0),
                    (-30.0, 562.5),
                    (110.0, 937.5),
                    (-110.0, 1125.0),
                ]) {
                    assert_eq!(object.position, Position::from_angles(az, 0.0));
                    assert_eq!((object.gain, object.spread), (1.0, 0.0));
                    assert!(object.signal.iter().all(|v| v.is_finite()));
                    if (1..30).contains(&count) {
                        let own = amplitude(object.signal, frequency);
                        assert!(own > 0.03 && own < 0.2, "tone {frequency}: amplitude {own}");
                        for other in [375.0, 562.5, 750.0, 937.5, 1125.0] {
                            if other != frequency {
                                assert!(amplitude(object.signal, other) < own * 0.1);
                            }
                        }
                    }
                }
                let lfe = scene.lfe.unwrap();
                if (1..30).contains(&count) {
                    assert!(amplitude(lfe, 62.5) > 0.03);
                }
            })
            .unwrap();
        assert_eq!(emitted, 1);
        count += 1;
    }
    assert_eq!(count, 32);
}

#[test]
fn ddp_reset_and_epoch_change_match_a_cold_decoder() {
    let size = eac3::parse_header(FIXTURE).unwrap().frame_size;
    let frame = burst(&FIXTURE[..size]);
    let mut decoder = DdpDecoder::new(DecodeConfig::default());
    let mut cold = Vec::new();
    decoder
        .push(&frame, 1, 1, &mut |s| {
            cold = s.objects.iter().map(|o| o.signal.to_vec()).collect()
        })
        .unwrap();
    decoder.push(&frame, 1, 1, &mut |_| {}).unwrap();
    decoder.reset();
    decoder
        .push(&frame, 2, 1, &mut |s| {
            for (a, b) in s.objects.iter().zip(&cold) {
                assert_eq!(a.signal, b);
            }
        })
        .unwrap();
    decoder
        .push(&frame, 3, 2, &mut |s| {
            for (a, b) in s.objects.iter().zip(&cold) {
                assert_eq!(a.signal, b);
            }
            assert_eq!((s.epoch, s.source_qpc), (2, 3));
        })
        .unwrap();
}

#[test]
fn ddp_rejects_bad_access_units_without_emitting_and_recovers() {
    let size = eac3::parse_header(FIXTURE).unwrap().frame_size;
    let first = &FIXTURE[..size];
    let mut decoder = DdpDecoder::new(DecodeConfig::default());
    let mut dependent = first.to_vec();
    dependent[2] |= 0x40;
    let mut alternate = first.to_vec();
    alternate[2] |= 0x08;
    let mut short_samples = first.to_vec();
    short_samples[4] &= !0x30;
    let mut trailing = first.to_vec();
    trailing.push(0);
    for invalid in [
        &[][..],
        &[0; 7],
        &first[..size - 1],
        &dependent,
        &alternate,
        &short_samples,
        &trailing,
        &FIXTURE[..size * 2],
    ] {
        assert!(
            decoder
                .push(&burst(invalid), 0, 0, &mut |_| panic!(
                    "invalid input emitted"
                ))
                .is_err()
        );
        let mut seen = false;
        decoder
            .push(&burst(first), 1, 0, &mut |_| seen = true)
            .unwrap();
        assert!(seen);
    }
    let mut wrong = burst(first);
    wrong.data_type = DataType::Ac3;
    assert!(
        decoder
            .push(&wrong, 0, 0, &mut |_| panic!("wrong codec"))
            .is_err()
    );
    let mut small = DdpDecoder::new(DecodeConfig {
        max_frames: 1535,
        ..DecodeConfig::default()
    });
    assert!(
        small
            .push(&burst(first), 0, 0, &mut |_| panic!("too small"))
            .is_err()
    );
}

/// Private raw E-AC-3 stream, not an IEC 61937 or media-container file.
/// Vector redistribution rights could not be established for upstream's
/// captured JOC fixture. This is deliberately ignored, not silently skipped.
#[test]
#[ignore = "requires a redistributable or locally supplied SSR_DDP_JOC_SAMPLE raw E-AC-3 stream"]
fn ddp_joc_external_sample_has_positioned_objects() {
    let path = std::env::var_os("SSR_DDP_JOC_SAMPLE").expect("set SSR_DDP_JOC_SAMPLE");
    let input = std::fs::read(path).unwrap();
    let mut decoder = DdpDecoder::new(DecodeConfig::default());
    let mut start = 0;
    let mut cursor = 0;
    let mut samples = 0;
    let mut joc = false;
    let mut scenes = 0;
    while cursor < input.len() {
        let header = eac3::parse_header(&input[cursor..]).unwrap();
        if header.stream_type != eac3::StreamType::Dependent && samples == 1536 {
            let mut emitted_frames = 0;
            decoder
                .push(&burst(&input[start..cursor]), scenes, 9, &mut |s| {
                    assert!(s.is_consistent());
                    assert!(s.objects.iter().all(|o| o.position.direction().is_some()
                        || (o.position == Position::default() && o.spread == 1.0)));
                    assert!((1..=1536).contains(&s.frames));
                    emitted_frames += s.frames;
                })
                .unwrap();
            assert_eq!(emitted_frames, 1536);
            scenes += 1;
            start = cursor;
            samples = 0;
        }
        let end = cursor + header.frame_size;
        let info = eac3::inspect_access_unit(&input[cursor..end]).unwrap();
        joc |= info
            .payloads()
            .any(|p| matches!(p.parsed, eac3::ParsedEmdfPayloadData::Joc(_)));
        if header.stream_type != eac3::StreamType::Dependent {
            samples += usize::from(header.samples);
        }
        cursor = end;
    }
    assert_eq!(samples, 1536);
    decoder
        .push(&burst(&input[start..]), scenes, 9, &mut |s| {
            assert!(s.is_consistent());
            assert!(!s.objects.is_empty());
        })
        .unwrap();
    assert!(joc, "sample contains no JOC payload");
}

#![forbid(unsafe_code)]

use std::sync::Arc;

use convolver::{Binauralizer, ConvolverError, FilterBank, MonoConvolver};
use proptest::prelude::*;
use ssr_types::Feeds;

fn direct_convolution(signals: &[Vec<f32>], irs: &[[Vec<f32>; 2]]) -> [Vec<f32>; 2] {
    let frames = signals[0].len();
    let mut output = [vec![0.0; frames], vec![0.0; frames]];
    for (input, signal) in signals.iter().enumerate() {
        for (ear, ear_output) in output.iter_mut().enumerate() {
            for frame in 0..frames {
                let first_tap = frame.saturating_sub(signal.len() - 1);
                for tap in first_tap..=frame.min(irs[input][ear].len() - 1) {
                    ear_output[frame] += signal[frame - tap] * irs[input][ear][tap];
                }
            }
        }
    }
    output
}

fn run_partitioned(signals: &[Vec<f32>], irs: &[[Vec<f32>; 2]], partition: usize) -> [Vec<f32>; 2] {
    let frames = signals[0].len();
    let borrowed: Vec<[&[f32]; 2]> = irs
        .iter()
        .map(|ears| [ears[0].as_slice(), ears[1].as_slice()])
        .collect();
    let bank = Arc::new(FilterBank::new(&borrowed, partition).unwrap());
    let mut convolver = Binauralizer::new(bank, frames);
    let mut feeds = Feeds::new(signals.len(), frames);
    for (input, signal) in signals.iter().enumerate() {
        feeds.channel_mut(input).copy_from_slice(signal);
    }
    let mut output = [vec![0.0; frames], vec![0.0; frames]];
    let (left, right) = output.split_at_mut(1);
    convolver.process(&feeds, &mut left[0], &mut right[0]);
    output
}

fn assert_matches_direct(signals: &[Vec<f32>], irs: &[[Vec<f32>; 2]], partition: usize) {
    let expected = direct_convolution(signals, irs);
    let actual = run_partitioned(signals, irs, partition);
    let peak = expected
        .iter()
        .flatten()
        .fold(0.0_f32, |maximum, value| maximum.max(value.abs()));
    let tolerance = if peak == 0.0 { 1.0e-7 } else { 1.0e-4 * peak };
    for ear in 0..2 {
        let error = expected[ear]
            .iter()
            .zip(&actual[ear])
            .fold(0.0_f32, |maximum, (expected, actual)| {
                maximum.max((expected - actual).abs())
            });
        assert!(
            error < tolerance,
            "partition {partition}, ear {ear}: max error {error} >= {tolerance}, peak {peak}"
        );
    }
}

#[test]
fn partitioned_matches_direct_for_required_sizes_and_input_counts() {
    let cases = [
        (64, 1, 1, 1),
        (64, 3, 9, 70),
        (480, 1, 22, 481),
        (480, 3, 4, 997),
        (512, 1, 7, 1025),
        (512, 3, 2, 777),
    ];
    for (partition, hops, inputs, ir_len) in cases {
        let frames = partition * hops;
        let signals: Vec<Vec<f32>> = (0..inputs)
            .map(|input| {
                (0..frames)
                    .map(|frame| {
                        (((frame * 19 + input * 11 + 3) % 97) as f32 / 48.0 - 1.0) / inputs as f32
                    })
                    .collect()
            })
            .collect();
        let irs: Vec<[Vec<f32>; 2]> = (0..inputs)
            .map(|input| {
                std::array::from_fn(|ear| {
                    (0..ir_len + (input + ear) % 3)
                        .map(|tap| {
                            let pseudo =
                                ((tap * 37 + input * 13 + ear * 7) % 101) as f32 / 50.0 - 1.0;
                            pseudo * (-(tap as f32) / (ir_len.max(1) as f32 * 0.22)).exp()
                                / ir_len.max(1) as f32
                        })
                        .collect()
                })
            })
            .collect();
        assert_matches_direct(&signals, &irs, partition);
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(16))]

    #[test]
    fn random_partitioned_convolution_matches_direct(
        partition in prop_oneof![Just(64usize), Just(480usize), Just(512usize)],
        hops in prop_oneof![Just(1usize), Just(3usize)],
        inputs in 1usize..=5,
        ir_extra in 0usize..80,
        seed in any::<u32>(),
    ) {
        let frames = partition * hops;
        let ir_len = partition + ir_extra;
        let signals: Vec<Vec<f32>> = (0..inputs)
            .map(|input| {
                (0..frames)
                    .map(|frame| {
                        let value = seed
                            .wrapping_add((input as u32).wrapping_mul(747_796_405))
                            .wrapping_add((frame as u32).wrapping_mul(2_891_336_453));
                        ((value >> 8) as f32 / (u32::MAX >> 8) as f32 * 2.0 - 1.0)
                            / inputs as f32
                    })
                    .collect()
            })
            .collect();
        let irs: Vec<[Vec<f32>; 2]> = (0..inputs)
            .map(|input| {
                std::array::from_fn(|ear| {
                    (0..ir_len)
                        .map(|tap| {
                            let value = seed
                                .wrapping_add((input as u32).wrapping_mul(1_664_525))
                                .wrapping_add((ear as u32).wrapping_mul(1_013_904_223))
                                .wrapping_add((tap as u32).wrapping_mul(22_695_477));
                            let random = (value >> 8) as f32 / (u32::MAX >> 8) as f32 * 2.0 - 1.0;
                            random * (-(tap as f32) / (ir_len as f32 * 0.2)).exp() / ir_len as f32
                        })
                        .collect()
                })
            })
            .collect();
        assert_matches_direct(&signals, &irs, partition);
    }
}

#[test]
fn impulse_ir_passes_input_through() {
    let partition = 64;
    let signal: Vec<f32> = (0..partition * 3)
        .map(|frame| (frame as f32 * 0.03125).sin())
        .collect();
    let irs = vec![[vec![1.0], vec![1.0]]];
    let output = run_partitioned(std::slice::from_ref(&signal), &irs, partition);
    for ear in output {
        for (actual, expected) in ear.iter().zip(&signal) {
            assert!((actual - expected).abs() < 2.0e-6);
        }
    }
}

#[test]
fn mono_matches_binaural_left_across_blocks_partitions_and_quiet_transitions() {
    for partition in [64, 480, 512] {
        for hops in [1, 3] {
            let frames = partition * hops;
            let irs: Vec<Vec<f32>> = (0..3)
                .map(|input| {
                    (0..partition * 129 + 17)
                        .map(|tap| {
                            (((tap * 37 + input * 11) % 101) as f32 / 50.0 - 1.0)
                                * (-(tap as f32) / 700.0).exp()
                        })
                        .collect()
                })
                .collect();
            let mono_irs = irs.iter().map(Vec::as_slice).collect::<Vec<_>>();
            let stereo_irs = irs
                .iter()
                .map(|ir| [ir.as_slice(), ir.as_slice()])
                .collect::<Vec<_>>();
            let mut mono = MonoConvolver::new(
                Arc::new(FilterBank::new_mono(&mono_irs, partition).unwrap()),
                frames,
            );
            let mut stereo = Binauralizer::new(
                Arc::new(FilterBank::new(&stereo_irs, partition).unwrap()),
                frames,
            );
            let mut feeds = Feeds::new(irs.len(), frames);
            let mut mono_output = vec![0.0; frames];
            let mut left = vec![0.0; frames];
            let mut right = vec![0.0; frames];

            for block in 0..48 {
                feeds.clear();
                let active = block < 5 || block == 23;
                if active {
                    for input in 0..irs.len() {
                        for (frame, sample) in feeds.channel_mut(input).iter_mut().enumerate() {
                            *sample = ((block * 43 + input * 29 + frame * 31) as f32).sin();
                        }
                    }
                    mono.note_active_input();
                    stereo.note_active_input();
                }
                mono.process(&feeds, &mut mono_output);
                stereo.process(&feeds, &mut left, &mut right);
                assert_eq!(
                    mono_output
                        .iter()
                        .map(|sample| sample.to_bits())
                        .collect::<Vec<_>>(),
                    left.iter()
                        .map(|sample| sample.to_bits())
                        .collect::<Vec<_>>(),
                    "partition {partition}, hops {hops}, block {block}"
                );
                if block == 23 {
                    mono.reset();
                    stereo.reset();
                }
                if !active {
                    mono.note_silent_input(frames);
                    stereo.note_silent_input(frames);
                }
                assert_eq!(mono.is_quiet(), stereo.is_quiet());
            }
        }
    }
}

#[test]
#[should_panic(expected = "filter bank output count mismatch")]
fn binauralizer_rejects_a_mono_bank() {
    let ir = [1.0_f32];
    let bank = Arc::new(FilterBank::new_mono(&[&ir], 64).unwrap());
    let _ = Binauralizer::new(bank, 64);
}

#[test]
#[should_panic(expected = "filter bank output count mismatch")]
fn mono_convolver_rejects_a_stereo_bank() {
    let ir = [1.0_f32];
    let bank = Arc::new(FilterBank::new(&[[&ir, &ir]], 64).unwrap());
    let _ = MonoConvolver::new(bank, 64);
}

#[test]
fn reset_clears_stream_history() {
    let partition = 64;
    let left = vec![0.75; partition + 11];
    let right = vec![-0.5; partition * 2 + 3];
    let borrowed = [[left.as_slice(), right.as_slice()]];
    let bank = Arc::new(FilterBank::new(&borrowed, partition).unwrap());
    let mut convolver = Binauralizer::new(bank, partition);
    let mut feeds = Feeds::new(1, partition);
    feeds.channel_mut(0)[partition - 1] = 1.0;
    let mut output_left = vec![0.0; partition];
    let mut output_right = vec![0.0; partition];
    convolver.process(&feeds, &mut output_left, &mut output_right);

    convolver.reset();
    feeds.clear();
    convolver.process(&feeds, &mut output_left, &mut output_right);
    assert!(output_left.iter().all(|sample| sample.abs() < 1.0e-7));
    assert!(output_right.iter().all(|sample| sample.abs() < 1.0e-7));
}

#[test]
fn filter_bank_validates_input() {
    assert!(matches!(
        FilterBank::new(&[], 64),
        Err(ConvolverError::NoInputs)
    ));
    let empty = [[&[][..], &[][..]]];
    assert!(matches!(
        FilterBank::new(&empty, 64),
        Err(ConvolverError::EmptyIr(0))
    ));
    let impulse = [[&[1.0][..], &[1.0][..]]];
    assert!(matches!(
        FilterBank::new(&impulse, 15),
        Err(ConvolverError::BadPartition(15))
    ));
    assert!(matches!(
        FilterBank::new(&impulse, 17),
        Err(ConvolverError::BadPartition(17))
    ));
}

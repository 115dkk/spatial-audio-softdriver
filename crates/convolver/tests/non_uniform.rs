#![forbid(unsafe_code)]

use std::sync::Arc;

use convolver::{Binauralizer, FilterBank};
use ssr_types::Feeds;

fn round_up(value: usize, multiple: usize) -> usize {
    value.div_ceil(multiple) * multiple
}

fn make_sparse_signals(inputs: usize, frames: usize, partition: usize) -> Vec<Vec<f32>> {
    (0..inputs)
        .map(|input| {
            let mut signal = vec![0.0; frames];
            let events = [
                (0, 0.75 / (input + 1) as f32),
                (partition / 2 + input * 3, -0.25 / (input + 1) as f32),
                (partition * 2 + 17 + input * 5, 0.125 / (input + 1) as f32),
                (partition * 9 + input * 7, -0.0625 / (input + 1) as f32),
                (partition * 65 + input * 11, 0.03125 / (input + 1) as f32),
                (partition * 129 + input * 13, -0.015625 / (input + 1) as f32),
            ];
            for (frame, value) in events {
                if frame < frames {
                    signal[frame] = value;
                }
            }
            signal
        })
        .collect()
}

fn make_irs(inputs: usize, length: usize) -> Vec<[Vec<f32>; 2]> {
    (0..inputs)
        .map(|input| {
            std::array::from_fn(|ear| {
                (0..length)
                    .map(|tap| {
                        let pseudo =
                            ((tap * 37 + input * 13 + ear * 7 + 3) % 101) as f32 / 50.0 - 1.0;
                        pseudo * (-(tap as f32) / (length as f32 * 0.3)).exp()
                            / (length as f32).sqrt()
                    })
                    .collect()
            })
        })
        .collect()
}

fn direct_sparse_convolution(signals: &[Vec<f32>], irs: &[[Vec<f32>; 2]]) -> [Vec<f32>; 2] {
    let frames = signals[0].len();
    let mut output = [vec![0.0; frames], vec![0.0; frames]];
    for (input, signal) in signals.iter().enumerate() {
        for (source_frame, source) in signal
            .iter()
            .copied()
            .enumerate()
            .filter(|(_, sample)| *sample != 0.0)
        {
            for (ear, ear_output) in output.iter_mut().enumerate() {
                let count = irs[input][ear].len().min(frames - source_frame);
                for (destination, coefficient) in ear_output[source_frame..source_frame + count]
                    .iter_mut()
                    .zip(&irs[input][ear][..count])
                {
                    *destination += source * *coefficient;
                }
            }
        }
    }
    output
}

fn run_streaming(
    signals: &[Vec<f32>],
    irs: &[[Vec<f32>; 2]],
    partition: usize,
    block_frames: usize,
) -> [Vec<f32>; 2] {
    let frames = signals[0].len();
    assert!(frames.is_multiple_of(block_frames));
    let borrowed: Vec<[&[f32]; 2]> = irs
        .iter()
        .map(|ears| [ears[0].as_slice(), ears[1].as_slice()])
        .collect();
    let bank = Arc::new(FilterBank::new(&borrowed, partition).unwrap());
    let mut convolver = Binauralizer::new(bank, block_frames);
    let mut feeds = Feeds::new(signals.len(), block_frames);
    let mut output = [vec![0.0; frames], vec![0.0; frames]];

    for block_start in (0..frames).step_by(block_frames) {
        for (input, signal) in signals.iter().enumerate() {
            feeds
                .channel_mut(input)
                .copy_from_slice(&signal[block_start..block_start + block_frames]);
        }
        let (left, right) = output.split_at_mut(1);
        convolver.process(
            &feeds,
            &mut left[0][block_start..block_start + block_frames],
            &mut right[0][block_start..block_start + block_frames],
        );
    }
    output
}

fn assert_close_relative(expected: &[Vec<f32>; 2], actual: &[Vec<f32>; 2], description: &str) {
    let peak = expected
        .iter()
        .flatten()
        .fold(0.0_f32, |maximum, sample| maximum.max(sample.abs()));
    let tolerance = if peak == 0.0 { 1.0e-7 } else { peak * 1.0e-4 };
    for ear in 0..2 {
        let (error_frame, error) = expected[ear]
            .iter()
            .zip(&actual[ear])
            .enumerate()
            .map(|(frame, (expected, actual))| (frame, (expected - actual).abs()))
            .max_by(|left, right| left.1.total_cmp(&right.1))
            .unwrap();
        assert!(
            error < tolerance,
            "{description}, ear {ear}, frame {error_frame}: max error {error} >= {tolerance}, peak {peak}"
        );
    }
}

#[test]
fn non_uniform_segments_match_direct_convolution() {
    for ir_length in [1_000, 20_000, 120_000] {
        for partition in [480, 512] {
            for block_hops in [1, 3] {
                for inputs in [1, 4] {
                    let block_frames = partition * block_hops;
                    let frames = round_up(ir_length + partition * 4, block_frames);
                    let signals = make_sparse_signals(inputs, frames, partition);
                    let irs = make_irs(inputs, ir_length);
                    let expected = direct_sparse_convolution(&signals, &irs);
                    let actual = run_streaming(&signals, &irs, partition, block_frames);
                    assert_close_relative(
                        &expected,
                        &actual,
                        &format!(
                            "IR {ir_length}, partition {partition}, block {block_frames}, inputs {inputs}"
                        ),
                    );
                }
            }
        }
    }
}

#[test]
fn impulses_at_segment_boundaries_are_sample_accurate() {
    let partition = 480;
    let first_boundary = 16 * partition;
    let second_boundary = 128 * partition;
    let frames = second_boundary + partition * 2;
    let mut left_ir = vec![0.0; second_boundary + 1];
    let mut right_ir = vec![0.0; second_boundary + 1];
    left_ir[first_boundary] = 0.75;
    left_ir[second_boundary] = -0.375;
    right_ir[first_boundary] = -0.5;
    right_ir[second_boundary] = 0.25;
    let irs = vec![[left_ir, right_ir]];
    let mut signal = vec![0.0; frames];
    signal[0] = 1.0;
    let signals = vec![signal];

    let expected = direct_sparse_convolution(&signals, &irs);
    let actual = run_streaming(&signals, &irs, partition, partition);
    assert_close_relative(&expected, &actual, "segment-boundary impulses");
    assert!((actual[0][first_boundary] - 0.75).abs() < 1.0e-5);
    assert!((actual[0][second_boundary] + 0.375).abs() < 1.0e-5);
    assert!((actual[1][first_boundary] + 0.5).abs() < 1.0e-5);
    assert!((actual[1][second_boundary] - 0.25).abs() < 1.0e-5);
}

#[test]
fn reset_clears_partially_scheduled_large_window() {
    let partition = 480;
    let first_boundary = 16 * partition;
    let second_boundary = 128 * partition;
    let mut left_ir = vec![0.0; second_boundary + 1];
    let mut right_ir = vec![0.0; second_boundary + 1];
    left_ir[first_boundary] = 0.5;
    right_ir[first_boundary] = -0.5;
    left_ir[second_boundary] = 1.0;
    right_ir[second_boundary] = -1.0;
    let borrowed = [[left_ir.as_slice(), right_ir.as_slice()]];
    let bank = Arc::new(FilterBank::new(&borrowed, partition).unwrap());
    let mut convolver = Binauralizer::new(bank, partition);
    let mut feeds = Feeds::new(1, partition);
    let mut left = vec![0.0; partition];
    let mut right = vec![0.0; partition];

    feeds.channel_mut(0)[0] = 1.0;
    convolver.process(&feeds, &mut left, &mut right);
    feeds.clear();
    // Before block 100, the first 64P window's work and the 8P window that
    // starts at block 88 are both in progress and nonzero.
    for block in 1..100 {
        if block == 88 {
            feeds.channel_mut(0)[0] = 1.0;
        }
        convolver.process(&feeds, &mut left, &mut right);
        feeds.clear();
    }
    convolver.reset();

    for _ in 0..132 {
        convolver.process(&feeds, &mut left, &mut right);
        assert!(left.iter().all(|sample| sample.to_bits() == 0));
        assert!(right.iter().all(|sample| sample.to_bits() == 0));
    }
}

#[test]
fn quiet_bound_never_precedes_the_last_nonzero_output() {
    let partition = 480;
    for ir_length in [1, 1_000, 20_000, 120_000] {
        let mut left_ir = vec![0.0; ir_length];
        let mut right_ir = vec![0.0; ir_length];
        left_ir[ir_length - 1] = 1.0;
        right_ir[ir_length - 1] = -1.0;
        let borrowed = [[left_ir.as_slice(), right_ir.as_slice()]];
        let bank = Arc::new(FilterBank::new(&borrowed, partition).unwrap());
        let mut convolver = Binauralizer::new(bank, partition);
        let mut feeds = Feeds::new(1, partition);
        let mut left = vec![0.0; partition];
        let mut right = vec![0.0; partition];

        feeds.channel_mut(0)[0] = 1.0;
        convolver.note_active_input();
        convolver.process(&feeds, &mut left, &mut right);
        feeds.clear();

        let mut hop = 0_usize;
        let mut quiet_hop = None;
        let mut last_nonzero_hop = left
            .iter()
            .chain(&right)
            .any(|sample| *sample != 0.0)
            .then_some(hop);
        let observation_hops = ir_length.div_ceil(partition) + 2 * 64 + 4;
        for _ in 0..observation_hops {
            hop += 1;
            convolver.process(&feeds, &mut left, &mut right);
            if left.iter().chain(&right).any(|sample| *sample != 0.0) {
                last_nonzero_hop = Some(hop);
            }
            convolver.note_silent_input(partition);
            if quiet_hop.is_none() && convolver.is_quiet() {
                quiet_hop = Some(hop);
            }
        }

        let quiet_hop = quiet_hop.expect("the quiet bound is reached");
        let last_nonzero_hop = last_nonzero_hop.expect("the delayed impulse is audible");
        assert!(
            quiet_hop > last_nonzero_hop,
            "IR {ir_length}: quiet hop {quiet_hop} does not follow final output hop {last_nonzero_hop}"
        );
    }
}

#[test]
fn repeated_runs_are_bit_identical() {
    let partition = 480;
    let ir_length = 120_000;
    let block_frames = partition * 3;
    let frames = round_up(ir_length + partition * 4, block_frames);
    let signals = make_sparse_signals(1, frames, partition);
    let irs = make_irs(1, ir_length);
    let first = run_streaming(&signals, &irs, partition, block_frames);
    let second = run_streaming(&signals, &irs, partition, block_frames);

    for ear in 0..2 {
        assert!(
            first[ear]
                .iter()
                .zip(&second[ear])
                .all(|(first, second)| first.to_bits() == second.to_bits())
        );
    }
}

#[test]
fn partitions_reports_total_across_segments() {
    let partition = 480;
    let one_segment = vec![0.0; 1_000];
    let two_segments = vec![0.0; 20_000];
    let three_segments = vec![0.0; 120_000];

    let one = FilterBank::new(
        &[[one_segment.as_slice(), one_segment.as_slice()]],
        partition,
    )
    .unwrap();
    let two = FilterBank::new(
        &[[two_segments.as_slice(), two_segments.as_slice()]],
        partition,
    )
    .unwrap();
    let three = FilterBank::new(
        &[[three_segments.as_slice(), three_segments.as_slice()]],
        partition,
    )
    .unwrap();

    assert_eq!(one.partitions(), 3);
    assert_eq!(two.partitions(), 20);
    assert_eq!(three.partitions(), 32);
}

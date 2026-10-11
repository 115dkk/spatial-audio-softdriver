#![forbid(unsafe_code)]

use std::sync::Arc;
use std::time::Instant;

use assert_no_alloc::{AllocDisabler, assert_no_alloc};
use bio_types::Feeds;
use convolver::{Binauralizer, FilterBank};

#[global_allocator]
static ALLOCATOR: AllocDisabler = AllocDisabler;

#[test]
fn steady_state_process_does_not_allocate() {
    const INPUTS: usize = 22;
    const PARTITION: usize = 480;
    const IR_FRAMES: usize = 24_000;

    let left: Vec<Vec<f32>> = (0..INPUTS)
        .map(|input| {
            (0..IR_FRAMES)
                .map(|frame| {
                    let phase = (frame * 17 + input * 7) as f32;
                    phase.sin() * (-(frame as f32) / 4_000.0).exp() / 1_000.0
                })
                .collect()
        })
        .collect();
    let right: Vec<Vec<f32>> = (0..INPUTS)
        .map(|input| {
            (0..IR_FRAMES)
                .map(|frame| {
                    let phase = (frame * 13 + input * 5) as f32;
                    phase.cos() * (-(frame as f32) / 4_300.0).exp() / 1_000.0
                })
                .collect()
        })
        .collect();
    let borrowed: Vec<[&[f32]; 2]> = (0..INPUTS)
        .map(|input| [left[input].as_slice(), right[input].as_slice()])
        .collect();
    let bank = Arc::new(FilterBank::new(&borrowed, PARTITION).unwrap());
    let mut binauralizer = Binauralizer::new(bank, PARTITION);
    let mut feeds = Feeds::new(INPUTS, PARTITION);
    for input in 0..INPUTS {
        for (frame, sample) in feeds.channel_mut(input).iter_mut().enumerate() {
            *sample = ((input * 29 + frame * 31) as f32).sin() / INPUTS as f32;
        }
    }
    let mut left_output = vec![0.0; PARTITION];
    let mut right_output = vec![0.0; PARTITION];

    // Warm up thread-local allocator bookkeeping and FFT code before the check.
    assert_no_alloc(|| {});
    binauralizer.process(&feeds, &mut left_output, &mut right_output);
    assert_no_alloc(|| {
        binauralizer.process(&feeds, &mut left_output, &mut right_output);
        binauralizer.reset();
    });

    if !cfg!(debug_assertions) {
        let iterations = 20;
        let start = Instant::now();
        for _ in 0..iterations {
            binauralizer.process(&feeds, &mut left_output, &mut right_output);
        }
        println!(
            "22 inputs x 24000-frame IRs x 480-frame block: {:?} per block",
            start.elapsed() / iterations
        );
    }
}

#[test]
fn three_segment_process_and_reset_do_not_allocate() {
    const INPUTS: usize = 4;
    const PARTITION: usize = 480;
    const IR_FRAMES: usize = 120_000;

    let left: Vec<f32> = (0..IR_FRAMES)
        .map(|frame| ((frame * 17) as f32).sin() / 10_000.0)
        .collect();
    let right: Vec<f32> = (0..IR_FRAMES)
        .map(|frame| ((frame * 13) as f32).cos() / 10_000.0)
        .collect();
    let borrowed: Vec<[&[f32]; 2]> = (0..INPUTS)
        .map(|_| [left.as_slice(), right.as_slice()])
        .collect();
    let bank = Arc::new(FilterBank::new(&borrowed, PARTITION).unwrap());
    let mut binauralizer = Binauralizer::new(bank, PARTITION);
    let mut feeds = Feeds::new(INPUTS, PARTITION);
    for input in 0..INPUTS {
        for (frame, sample) in feeds.channel_mut(input).iter_mut().enumerate() {
            *sample = ((input * 29 + frame * 31) as f32).sin() / INPUTS as f32;
        }
    }
    let mut left_output = vec![0.0; PARTITION];
    let mut right_output = vec![0.0; PARTITION];

    assert_no_alloc(|| {});
    // Reach both larger segments, then reset while their work schedules are active.
    for _ in 0..130 {
        binauralizer.process(&feeds, &mut left_output, &mut right_output);
    }
    assert_no_alloc(|| {
        for _ in 0..70 {
            binauralizer.process(&feeds, &mut left_output, &mut right_output);
        }
        binauralizer.reset();
        binauralizer.process(&feeds, &mut left_output, &mut right_output);
    });
}

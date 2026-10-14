#![forbid(unsafe_code)]

use std::hint::black_box;
use std::sync::Arc;
use std::time::Instant;

use convolver::{Binauralizer, FilterBank};
use ssr_types::Feeds;

fn measure_case(inputs: usize, ir_frames: usize, iterations: usize) -> (f64, f64) {
    const PARTITION: usize = 480;

    let left: Vec<Vec<f32>> = (0..inputs)
        .map(|input| {
            (0..ir_frames)
                .map(|frame| {
                    let phase = (frame * 17 + input * 7) as f32;
                    phase.sin() * (-(frame as f32) / 50_000.0).exp() / 10_000.0
                })
                .collect()
        })
        .collect();
    let right: Vec<Vec<f32>> = (0..inputs)
        .map(|input| {
            (0..ir_frames)
                .map(|frame| {
                    let phase = (frame * 13 + input * 5) as f32;
                    phase.cos() * (-(frame as f32) / 47_000.0).exp() / 10_000.0
                })
                .collect()
        })
        .collect();
    let borrowed: Vec<[&[f32]; 2]> = (0..inputs)
        .map(|input| [left[input].as_slice(), right[input].as_slice()])
        .collect();
    let bank = Arc::new(FilterBank::new(&borrowed, PARTITION).unwrap());
    let mut binauralizer = Binauralizer::new(bank, PARTITION);
    let mut feeds = Feeds::new(inputs, PARTITION);
    for input in 0..inputs {
        for (frame, sample) in feeds.channel_mut(input).iter_mut().enumerate() {
            *sample = ((input * 29 + frame * 31) as f32).sin() / inputs as f32;
        }
    }
    let mut left_output = vec![0.0; PARTITION];
    let mut right_output = vec![0.0; PARTITION];

    // Fill every segment's history and execute each work position before timing.
    for _ in 0..256 {
        binauralizer.process(&feeds, &mut left_output, &mut right_output);
    }

    let mut total_micros = 0.0;
    let mut maximum_micros = 0.0_f64;
    for _ in 0..iterations {
        let start = Instant::now();
        binauralizer.process(
            black_box(&feeds),
            black_box(&mut left_output),
            black_box(&mut right_output),
        );
        let elapsed = start.elapsed().as_secs_f64() * 1_000_000.0;
        total_micros += elapsed;
        maximum_micros = maximum_micros.max(elapsed);
    }
    black_box((&left_output, &right_output));
    (total_micros / iterations as f64, maximum_micros)
}

#[test]
fn reports_representative_block_times() {
    if cfg!(debug_assertions) {
        eprintln!("timing measurement is skipped outside release mode");
        return;
    }

    let iterations = 2_048;
    let (long_mean, long_maximum) = measure_case(21, 310_144, iterations);
    println!(
        "timing: 21 inputs (42 IRs), 310144 taps, P=480, {iterations} blocks: mean {long_mean:.3} us, max {long_maximum:.3} us"
    );
    let (short_mean, short_maximum) = measure_case(22, 24_000, iterations);
    println!(
        "timing: 22 inputs (44 IRs), 24000 taps, P=480, {iterations} blocks: mean {short_mean:.3} us, max {short_maximum:.3} us"
    );
}

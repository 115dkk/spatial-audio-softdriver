#![forbid(unsafe_code)]

use engine::{BedBlock, MIX_FRAMES, MIX_RATE, MixerConfig, SceneMixer};
use render::PanGeometry;
use ssr_types::{Direction, Layout, MAX_OBJECTS, Object, Position, SceneBlock};

fn config() -> MixerConfig {
    MixerConfig {
        arrival_slack_frames: 0,
        qpc_frequency: 1_000_000,
        endpoint_mask: u32::MAX,
    }
}

fn layout(angles: &[f32]) -> Layout {
    Layout::new(
        angles
            .iter()
            .enumerate()
            .map(|(index, angle)| Direction::new(format!("D{index}"), *angle, 0.0))
            .collect(),
    )
    .unwrap()
}

fn stereo_geometry() -> PanGeometry {
    PanGeometry::new(&layout(&[30.0, -30.0])).unwrap()
}

fn scene<'a>(
    epoch: u32,
    rate: u32,
    qpc: u64,
    signals: &'a [Vec<f32>],
    positions: &[Position],
    gains: &[f32],
    objects: &'a mut [Object<'a>; MAX_OBJECTS],
) -> SceneBlock<'a> {
    for (index, signal) in signals.iter().enumerate() {
        objects[index] = Object {
            signal,
            position: positions[index],
            gain: gains[index],
            spread: 0.0,
        };
    }
    SceneBlock {
        epoch,
        frames: signals.first().map_or(0, Vec::len),
        rate,
        source_qpc: qpc,
        objects: &objects[..signals.len()],
        lfe: None,
        new_objects: 0,
    }
}

fn one<'a>(
    epoch: u32,
    rate: u32,
    qpc: u64,
    signal: &'a [f32],
    position: Position,
    gain: f32,
    objects: &'a mut [Object<'a>; MAX_OBJECTS],
) -> SceneBlock<'a> {
    objects[0] = Object {
        signal,
        position,
        gain,
        spread: 0.0,
    };
    SceneBlock {
        epoch,
        frames: signal.len(),
        rate,
        source_qpc: qpc,
        objects: &objects[..1],
        lfe: None,
        new_objects: 0,
    }
}

fn collect_direction(block: &BedBlock<'_>, direction: usize, output: &mut Vec<f32>) {
    let start = direction * block.frames;
    output.extend_from_slice(&block.channels[start..start + block.frames]);
}

fn trim_trailing_zeros(samples: &[f32]) -> &[f32] {
    let end = samples
        .iter()
        .rposition(|sample| *sample != 0.0)
        .map_or(0, |index| index + 1);
    &samples[..end]
}

fn tone(rate: u32, frames: usize, frequency: f64, offset: usize) -> Vec<f32> {
    (0..frames)
        .map(|frame| {
            (std::f64::consts::TAU * frequency * (offset + frame) as f64 / rate as f64).sin() as f32
        })
        .collect()
}

fn resample_tone(rate: u32, frequency: f64, seconds: usize) -> Vec<f32> {
    let geometry = PanGeometry::new(&layout(&[0.0])).unwrap();
    let source_frames = rate as usize / 100;
    let mut mixer = SceneMixer::new(config());
    mixer.lane_start(0, 0);
    let mut output = Vec::with_capacity(MIX_RATE as usize * seconds + MIX_FRAMES);
    let mut offset = 0;
    while offset < rate as usize * seconds {
        let frames = source_frames.min(rate as usize * seconds - offset);
        let signal = tone(rate, frames, frequency, offset);
        let mut objects = [Object::SILENT; MAX_OBJECTS];
        let block = one(1, rate, 0, &signal, Position::FRONT, 1.0, &mut objects);
        mixer.push_scene(0, &block, Some(&geometry)).unwrap();
        mixer.advance(0, MIX_FRAMES as u64, &mut |mixed| {
            collect_direction(mixed, 0, &mut output)
        });
        offset += frames;
    }
    mixer.finish(&mut |mixed| collect_direction(mixed, 0, &mut output));
    output
}

fn tone_amplitude(samples: &[f32], rate: u32, frequency: f64) -> f64 {
    let skip = samples.len().min(2 * MIX_FRAMES);
    let samples = &samples[skip..];
    let (mut sin_sum, mut cos_sum) = (0.0, 0.0);
    for (frame, &sample) in samples.iter().enumerate() {
        let phase = std::f64::consts::TAU * frequency * frame as f64 / rate as f64;
        sin_sum += f64::from(sample) * phase.sin();
        cos_sum += f64::from(sample) * phase.cos();
    }
    2.0 * sin_sum.hypot(cos_sum) / samples.len() as f64
}

fn band_energy(samples: &[f32], rate: u32, low: u32, high: u32) -> f64 {
    let skip = samples.len().min(2 * MIX_FRAMES);
    let samples = &samples[skip..];
    let bins = samples.len();
    let first_bin = (u64::from(low) * bins as u64).div_ceil(u64::from(rate)) as usize;
    let last_bin = (u64::from(high) * bins as u64 / u64::from(rate)) as usize;
    let mut energy = 0.0;
    for bin in first_bin..=last_bin {
        let angle = std::f64::consts::TAU * bin as f64 / bins as f64;
        let (step_sin, step_cos) = angle.sin_cos();
        let (mut oscillator_sin, mut oscillator_cos) = (0.0_f64, 1.0_f64);
        let (mut sin_sum, mut cos_sum) = (0.0, 0.0);
        for &sample in samples {
            sin_sum += f64::from(sample) * oscillator_sin;
            cos_sum += f64::from(sample) * oscillator_cos;
            let next_cos = oscillator_cos * step_cos - oscillator_sin * step_sin;
            oscillator_sin = oscillator_sin * step_cos + oscillator_cos * step_sin;
            oscillator_cos = next_cos;
        }
        energy += sin_sum * sin_sum + cos_sum * cos_sum;
    }
    energy.sqrt() * 2.0 / samples.len() as f64
}

#[test]
fn one_48_khz_interval_passes_sample_for_sample() {
    let geometry = PanGeometry::new(&layout(&[0.0])).unwrap();
    let samples = (0..MIX_FRAMES)
        .map(|frame| frame as f32)
        .collect::<Vec<_>>();
    let mut objects = [Object::SILENT; MAX_OBJECTS];
    let block = one(
        7,
        MIX_RATE,
        1234,
        &samples,
        Position::FRONT,
        1.0,
        &mut objects,
    );
    let mut mixer = SceneMixer::new(config());
    mixer.push_scene(0, &block, Some(&geometry)).unwrap();
    let mut emitted = 0;
    mixer.advance(0, MIX_FRAMES as u64, &mut |mixed| {
        emitted += 1;
        assert_eq!(
            (mixed.epoch, mixed.rate, mixed.frames),
            (7, MIX_RATE, MIX_FRAMES)
        );
        assert_eq!(mixed.source_qpc, 1234);
        assert_eq!(mixed.directions, 1);
        assert_eq!(mixed.channels, samples);
    });
    assert_eq!(emitted, 1);
}

#[test]
fn ddp_sized_blocks_are_recut_without_changing_the_signal() {
    let geometry = PanGeometry::new(&layout(&[0.0])).unwrap();
    let mut mixer = SceneMixer::new(config());
    let mut actual = Vec::new();
    for block_index in 0..2 {
        let start = block_index * 1536;
        let signal = (start..start + 1536)
            .map(|frame| frame as f32 + 1.0)
            .collect::<Vec<_>>();
        let mut objects = [Object::SILENT; MAX_OBJECTS];
        let block = one(
            1,
            MIX_RATE,
            block_index as u64 * 32_000,
            &signal,
            Position::FRONT,
            1.0,
            &mut objects,
        );
        mixer.push_scene(0, &block, Some(&geometry)).unwrap();
        mixer.advance(0, 1536, &mut |mixed| {
            collect_direction(mixed, 0, &mut actual)
        });
    }
    mixer.finish(&mut |mixed| collect_direction(mixed, 0, &mut actual));
    assert_eq!(actual.len(), 7 * MIX_FRAMES);
    for (frame, &sample) in actual[..3072].iter().enumerate() {
        assert_eq!(sample, frame as f32 + 1.0);
    }
    assert!(actual[3072..].iter().all(|sample| *sample == 0.0));
}

#[test]
fn recut_source_qpc_uses_the_configured_clock_frequency() {
    let geometry = PanGeometry::new(&layout(&[0.0])).unwrap();
    let signal = vec![0.25; 2 * MIX_FRAMES];
    let mut objects = [Object::SILENT; MAX_OBJECTS];
    let block = one(
        7,
        MIX_RATE,
        1234,
        &signal,
        Position::FRONT,
        1.0,
        &mut objects,
    );
    let mut mixer = SceneMixer::new(config());
    mixer.push_scene(0, &block, Some(&geometry)).unwrap();
    let mut qpcs = Vec::new();
    mixer.advance(0, 2 * MIX_FRAMES as u64, &mut |mixed| {
        qpcs.push(mixed.source_qpc)
    });
    assert_eq!(qpcs, [1234, 11_234]);
}

#[test]
fn every_lane_places_all_objects_without_folding() {
    let geometry = PanGeometry::new(&layout(&[0.0])).unwrap();
    let primary_signals = (0..60)
        .map(|index| vec![index as f32 + 1.0; MIX_FRAMES])
        .collect::<Vec<_>>();
    let secondary_signals = (0..12)
        .map(|index| vec![100.0 + index as f32; MIX_FRAMES])
        .collect::<Vec<_>>();
    let positions = vec![Position::FRONT; MAX_OBJECTS];
    let gains = vec![1.0; MAX_OBJECTS];
    let mut primary_objects = [Object::SILENT; MAX_OBJECTS];
    let mut secondary_objects = [Object::SILENT; MAX_OBJECTS];
    let primary = scene(
        1,
        MIX_RATE,
        0,
        &primary_signals,
        &positions,
        &gains,
        &mut primary_objects,
    );
    let secondary = scene(
        2,
        MIX_RATE,
        0,
        &secondary_signals,
        &positions,
        &gains,
        &mut secondary_objects,
    );
    let expected = primary_signals
        .iter()
        .chain(&secondary_signals)
        .map(|signal| signal[0])
        .sum::<f32>();
    let mut mixer = SceneMixer::new(config());
    mixer.lane_start(0, 0);
    mixer.lane_start(1, 0);
    mixer.push_scene(1, &secondary, Some(&geometry)).unwrap();
    mixer.push_scene(0, &primary, Some(&geometry)).unwrap();
    mixer.advance(0, MIX_FRAMES as u64, &mut |mixed| {
        assert_eq!(mixed.directions, 1);
        assert!((mixed.channels[0] - expected).abs() <= expected * 1.0e-5);
    });
}

#[test]
fn metadata_change_inside_one_interval_takes_effect() {
    let geometry = stereo_geometry();
    let first_signal = vec![1.0; 96];
    let second_signal = vec![1.0; MIX_FRAMES - 96];
    let mut first_objects = [Object::SILENT; MAX_OBJECTS];
    let mut second_objects = [Object::SILENT; MAX_OBJECTS];
    let first = one(
        1,
        MIX_RATE,
        0,
        &first_signal,
        Position::from_angles(30.0, 0.0),
        1.0,
        &mut first_objects,
    );
    let second = one(
        1,
        MIX_RATE,
        2_000,
        &second_signal,
        Position::from_angles(-30.0, 0.0),
        0.5,
        &mut second_objects,
    );
    let mut mixer = SceneMixer::new(config());
    mixer.push_scene(0, &first, Some(&geometry)).unwrap();
    mixer.push_scene(0, &second, Some(&geometry)).unwrap();
    mixer.advance(0, MIX_FRAMES as u64, &mut |mixed| {
        let left = &mixed.channels[..MIX_FRAMES];
        let right = &mixed.channels[MIX_FRAMES..];
        assert!(
            left[..96]
                .iter()
                .all(|sample| (*sample - 1.0).abs() < 1.0e-6)
        );
        assert!(right[..96].iter().all(|sample| sample.abs() < 1.0e-6));
        assert!(left[96..160].windows(2).all(|pair| pair[0] >= pair[1]));
        assert!(right[96..160].windows(2).all(|pair| pair[0] <= pair[1]));
        assert!(left[160..].iter().all(|sample| sample.abs() < 1.0e-6));
        assert!(
            right[160..]
                .iter()
                .all(|sample| (*sample - 0.5).abs() < 1.0e-6)
        );
    });
}

fn moving_lane(include_other: bool) -> Vec<u32> {
    let geometry = stereo_geometry();
    let first_signal = vec![1.0; MIX_FRAMES];
    let second_signal = vec![1.0; MIX_FRAMES];
    let other_signal = vec![0.125; MIX_FRAMES];
    let mut first_objects = [Object::SILENT; MAX_OBJECTS];
    let mut second_objects = [Object::SILENT; MAX_OBJECTS];
    let mut other_objects = [Object::SILENT; MAX_OBJECTS];
    let first = one(
        1,
        MIX_RATE,
        0,
        &first_signal,
        Position::from_angles(30.0, 0.0),
        1.0,
        &mut first_objects,
    );
    let second = one(
        1,
        MIX_RATE,
        10_000,
        &second_signal,
        Position::from_angles(-30.0, 0.0),
        1.0,
        &mut second_objects,
    );
    let other = one(
        2,
        MIX_RATE,
        0,
        &other_signal,
        Position::FRONT,
        1.0,
        &mut other_objects,
    );
    let mut mixer = SceneMixer::new(config());
    mixer.lane_start(1, 0);
    if include_other {
        mixer.lane_start(0, 0);
    }
    mixer.push_scene(1, &first, Some(&geometry)).unwrap();
    if include_other {
        mixer.push_scene(0, &other, Some(&geometry)).unwrap();
    }
    mixer.advance(1, MIX_FRAMES as u64, &mut |_| {});
    mixer.push_scene(1, &second, Some(&geometry)).unwrap();
    if include_other {
        mixer.lane_discontinuity(0, 0);
    }
    let mut bits = Vec::new();
    mixer.advance(1, MIX_FRAMES as u64, &mut |mixed| {
        for direction in 0..mixed.directions {
            let start = direction * mixed.frames;
            bits.extend(
                mixed.channels[start..start + mixed.frames]
                    .iter()
                    .map(|sample| sample.to_bits()),
            );
        }
    });
    bits
}

#[test]
fn a_lanes_ramp_is_independent_of_other_lanes() {
    assert_eq!(moving_lane(false), moving_lane(true));
}

#[test]
fn overlapping_generations_never_fold() {
    fn run(restart: bool) -> (Vec<u32>, u64) {
        let geometry = PanGeometry::new(&layout(&[0.0])).unwrap();
        let signal = vec![1.0; MIX_FRAMES];
        let mut old_objects = [Object::SILENT; MAX_OBJECTS];
        let old = one(
            1,
            MIX_RATE,
            0,
            &signal,
            Position::FRONT,
            1.0,
            &mut old_objects,
        );
        let mut mixer = SceneMixer::new(config());
        mixer.push_scene(0, &old, Some(&geometry)).unwrap();
        if restart {
            let silent = vec![0.0; MIX_FRAMES];
            let mut new_objects = [Object::SILENT; MAX_OBJECTS];
            let new = one(
                2,
                MIX_RATE,
                0,
                &silent,
                Position::FRONT,
                1.0,
                &mut new_objects,
            );
            mixer.lane_discontinuity(0, -64);
            mixer.push_scene(0, &new, Some(&geometry)).unwrap();
        } else {
            mixer.lane_discontinuity(0, -64);
        }
        let mut output = Vec::new();
        mixer.finish(&mut |mixed| {
            output.extend(
                mixed.channels[..mixed.frames]
                    .iter()
                    .map(|sample| sample.to_bits()),
            )
        });
        (output, mixer.dropped_frames())
    }

    let restarted = run(true);
    let stopped = run(false);
    assert_eq!(&restarted.0[..MIX_FRAMES], stopped.0.as_slice());
    assert!(restarted.0[MIX_FRAMES..].iter().all(|sample| *sample == 0));
    assert_eq!(restarted.1, (MIX_FRAMES - 64) as u64);
    assert_eq!(stopped.1, restarted.1);
}

#[test]
fn layout_change_converts_pending_frames() {
    let old = PanGeometry::new(&layout(&[0.0])).unwrap();
    let new = stereo_geometry();
    let signal = vec![1.0; MIX_FRAMES];
    let mut objects = [Object::SILENT; MAX_OBJECTS];
    let block = one(1, MIX_RATE, 0, &signal, Position::FRONT, 1.0, &mut objects);
    let mut mixer = SceneMixer::new(config());
    mixer.push_scene(0, &block, Some(&old)).unwrap();
    mixer.change_layout(Some(&old), &new);
    mixer.advance(0, MIX_FRAMES as u64, &mut |mixed| {
        assert_eq!(mixed.directions, 2);
        for frame in 0..MIX_FRAMES {
            let power = mixed.channels[frame].powi(2) + mixed.channels[MIX_FRAMES + frame].powi(2);
            assert!((power - 1.0).abs() < 1.0e-5);
        }
    });
}

#[test]
fn layout_changes_beyond_the_conversion_ring() {
    let layouts = [
        layout(&[0.0]),
        layout(&[30.0, -30.0]),
        layout(&[0.0, 120.0, -120.0]),
        layout(&[45.0, -45.0, 135.0, -135.0]),
        layout(&[0.0, 72.0, 144.0, -144.0, -72.0]),
        layout(&[30.0, -30.0]),
    ];
    let geometries = layouts
        .iter()
        .map(|layout| PanGeometry::new(layout).unwrap())
        .collect::<Vec<_>>();
    let signal = vec![1.0; MIX_FRAMES];
    let mut objects = [Object::SILENT; MAX_OBJECTS];
    let block = one(1, MIX_RATE, 0, &signal, Position::FRONT, 1.0, &mut objects);
    let mut mixer = SceneMixer::new(config());
    mixer.push_scene(0, &block, Some(&geometries[0])).unwrap();
    for pair in geometries.windows(2) {
        mixer.change_layout(Some(&pair[0]), &pair[1]);
    }
    let mut emitted = 0;
    mixer.advance(0, MIX_FRAMES as u64, &mut |mixed| {
        emitted += 1;
        assert_eq!(mixed.directions, geometries.last().unwrap().directions());
        assert!(mixed.channels.iter().any(|sample| sample.abs() > 0.0));
    });
    assert_eq!(emitted, 1);
}

#[test]
fn no_geometry_keeps_the_timeline_and_emits_needs_brir() {
    // A missing geometry is the mixer's NeedsBrir condition: the sound is
    // explicitly dropped while the common timeline still advances.
    let signal = vec![1.0; MIX_FRAMES];
    let mut objects = [Object::SILENT; MAX_OBJECTS];
    let block = one(1, MIX_RATE, 0, &signal, Position::FRONT, 1.0, &mut objects);
    let mut mixer = SceneMixer::new(config());
    mixer.push_scene(0, &block, None).unwrap();
    let mut blocks = 0;
    mixer.advance(0, MIX_FRAMES as u64, &mut |mixed| {
        blocks += 1;
        assert_eq!(mixed.directions, 0);
        assert!(mixed.channels.is_empty());
        assert!(mixed.lfe.is_none());
        assert!(mixed.silent);
    });
    assert_eq!(blocks, 1);
    assert_eq!(mixer.now(), MIX_FRAMES as u64);
    assert_eq!(mixer.dropped_frames(), MIX_FRAMES as u64);
    assert_eq!(mixer.lane_underrun_frames(), 0);
}

#[test]
fn mismatched_geometry_counts_exact_dropped_frames_without_panicking() {
    let mono = PanGeometry::new(&layout(&[0.0])).unwrap();
    let stereo = stereo_geometry();
    let signal = vec![1.0; MIX_FRAMES];
    let mut first_objects = [Object::SILENT; MAX_OBJECTS];
    let first = one(
        1,
        MIX_RATE,
        0,
        &signal,
        Position::FRONT,
        1.0,
        &mut first_objects,
    );
    let mut second_objects = [Object::SILENT; MAX_OBJECTS];
    let second = one(
        2,
        MIX_RATE,
        0,
        &signal,
        Position::FRONT,
        1.0,
        &mut second_objects,
    );
    let mut mixer = SceneMixer::new(config());
    mixer.push_scene(0, &first, Some(&mono)).unwrap();
    mixer.advance(0, MIX_FRAMES as u64, &mut |_| {});
    mixer.push_scene(0, &second, Some(&stereo)).unwrap();
    mixer.finish(&mut |_| {});
    assert_eq!(mixer.dropped_frames(), MIX_FRAMES as u64);
    assert_eq!(mixer.lane_underrun_frames(), 0);
}

#[test]
fn resampler_meets_passband_and_stopband_specification() {
    for rate in [44_100, 88_200, 96_000, 176_400, 192_000] {
        let mut passband = 0.0_f64;
        for frequency in (500..=20_000).step_by(500).map(f64::from) {
            let output = resample_tone(rate, frequency, 1);
            let db = 20.0 * tone_amplitude(&output, MIX_RATE, frequency).log10();
            passband = passband.max(db.abs());
            assert!(
                (-0.1..=0.1).contains(&db),
                "{rate} Hz {frequency} Hz: {db:.3} dB"
            );
        }
        eprintln!("{rate} Hz passband worst: {passband:.4} dB");
    }
    for rate in [88_200, 96_000, 176_400, 192_000] {
        let mut stopband = f64::NEG_INFINITY;
        for frequency in (25_000..rate / 2).step_by(500).map(f64::from) {
            let output = resample_tone(rate, frequency, 1);
            let alias = (frequency - (frequency / MIX_RATE as f64).round() * MIX_RATE as f64).abs();
            let db = 20.0
                * tone_amplitude(&output, MIX_RATE, alias)
                    .max(f64::MIN_POSITIVE)
                    .log10();
            stopband = stopband.max(db);
            assert!(
                db <= -60.0,
                "{rate} Hz {frequency} Hz aliases at {db:.3} dB"
            );
        }
        eprintln!("{rate} Hz stopband worst: {stopband:.2} dB");
    }
    let output = resample_tone(44_100, 21_000.0, 1);
    let db = 20.0 * tone_amplitude(&output, MIX_RATE, 21_000.0).log10();
    assert!((-0.1..=0.1).contains(&db), "44.1 kHz 21 kHz: {db:.3} dB");
    let image_db = 20.0
        * band_energy(&output, MIX_RATE, 22_050, 24_000)
            .max(f64::MIN_POSITIVE)
            .log10();
    eprintln!("44.1 kHz image band: {image_db:.2} dB");
    assert!(image_db <= -60.0, "44.1 kHz image band: {image_db:.3} dB");
}

fn deterministic_output_hash(rate: u32) -> u64 {
    let geometry = PanGeometry::new(&layout(&[0.0])).unwrap();
    let frames = rate as usize / 100;
    let mut mixer = SceneMixer::new(config());
    let mut seed = 0xa409_3822_299f_31d0_u64 ^ u64::from(rate);
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for block_index in 0..3 {
        let signal = (0..frames)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                (seed >> 40) as f32 / 0x00ff_ffff_u32 as f32 - 0.5
            })
            .collect::<Vec<_>>();
        let mut objects = [Object::SILENT; MAX_OBJECTS];
        let block = one(
            1,
            rate,
            block_index * 10_000,
            &signal,
            Position::FRONT,
            1.0,
            &mut objects,
        );
        mixer.push_scene(0, &block, Some(&geometry)).unwrap();
        mixer.advance(0, MIX_FRAMES as u64, &mut |mixed| {
            for sample in mixed.channels {
                hash ^= u64::from(sample.to_bits());
                hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
            }
        });
    }
    mixer.flush_lane(0, Some(&geometry)).unwrap();
    mixer.finish(&mut |mixed| {
        for sample in mixed.channels {
            hash ^= u64::from(sample.to_bits());
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    });
    hash
}

#[test]
fn legacy_rates_keep_fixed_sample_hashes() {
    let actual = [
        deterministic_output_hash(44_100),
        deterministic_output_hash(MIX_RATE),
        deterministic_output_hash(96_000),
    ];
    assert_eq!(
        actual,
        [
            0xcdc9_8c1b_0de2_e3d5,
            0xa888_d295_95bf_b38f,
            0x16d4_69bd_659a_0f3e,
        ]
    );
}

#[test]
fn equal_clocks_with_ddp_bursts_do_not_drop_but_fast_secondary_does() {
    fn run(parts_per_million: u64) -> (u64, Vec<f32>) {
        let geometry = PanGeometry::new(&layout(&[0.0])).unwrap();
        let signal = vec![1.0; 1536];
        let mut mixer = SceneMixer::new(config());
        mixer.lane_start(1, 0);
        let mut source_total = 0_u64;
        let mut transport_total = 0_u64;
        for _ in 0..1600 {
            let mut objects = [Object::SILENT; MAX_OBJECTS];
            let block = one(1, MIX_RATE, 0, &signal, Position::FRONT, 1.0, &mut objects);
            mixer.push_scene(1, &block, Some(&geometry)).unwrap();
            source_total += 1536;
            let next_transport = (source_total as u128 * 1_000_000
                / u128::from(1_000_000 + parts_per_million))
                as u64;
            mixer.advance(1, next_transport - transport_total, &mut |_| {});
            transport_total = next_transport;
            if mixer.dropped_frames() > 0 {
                break;
            }
        }
        let mut tail = Vec::new();
        mixer.finish(&mut |mixed| collect_direction(mixed, 0, &mut tail));
        (mixer.dropped_frames(), tail)
    }

    assert_eq!(run(0).0, 0);
    let (dropped, tail) = run(500);
    assert!(dropped > 0);
    let tail = trim_trailing_zeros(&tail);
    let fade = &tail[tail.len() - 63..];
    assert!(fade.windows(2).all(|pair| pair[0] >= pair[1]));
    assert!(fade[0] < 1.0);
}

#[test]
fn overfull_lane_crossfades_across_dropped_frames() {
    let geometry = PanGeometry::new(&layout(&[0.0])).unwrap();
    let signal = vec![1.0; 1536];
    let mut first_objects = [Object::SILENT; MAX_OBJECTS];
    let mut second_objects = [Object::SILENT; MAX_OBJECTS];
    let first = one(
        1,
        MIX_RATE,
        0,
        &signal,
        Position::FRONT,
        1.0,
        &mut first_objects,
    );
    let second = one(
        1,
        MIX_RATE,
        0,
        &signal,
        Position::FRONT,
        1.0,
        &mut second_objects,
    );
    let mut mixer = SceneMixer::new(config());
    mixer.push_scene(1, &first, Some(&geometry)).unwrap();
    mixer.push_scene(1, &second, Some(&geometry)).unwrap();
    assert!(mixer.dropped_frames() > 0);
    let mut output = Vec::new();
    mixer.finish(&mut |mixed| collect_direction(mixed, 0, &mut output));
    let fade = &output[2432..2496];
    assert!((fade[0] - 63.0 / 64.0).abs() < 1.0e-6);
    assert_eq!(fade[63], 0.0);
    assert!(fade.windows(2).all(|pair| pair[0] >= pair[1]));
}

#[test]
fn lfe_crossfade_uses_the_discarded_interval_tail() {
    let geometry = PanGeometry::new(&layout(&[0.0])).unwrap();
    let signal = vec![1.0; 1536];
    let scene = SceneBlock {
        epoch: 1,
        frames: signal.len(),
        rate: MIX_RATE,
        source_qpc: 0,
        objects: &[],
        lfe: Some(&signal),
        new_objects: 0,
    };
    let mut mixer = SceneMixer::new(config());
    mixer.push_scene(1, &scene, Some(&geometry)).unwrap();
    mixer.push_scene(1, &scene, Some(&geometry)).unwrap();
    assert!(mixer.dropped_frames() > 0);
    let mut output = Vec::new();
    mixer.finish(&mut |mixed| output.extend_from_slice(mixed.lfe.unwrap()));
    let fade = &output[2432..2496];
    assert!((fade[0] - 63.0 / 64.0).abs() < 1.0e-6);
    assert_eq!(fade[63], 0.0);
}

#[test]
fn fir_flush_is_consumed_and_finish_is_idempotent() {
    fn run(flushes: usize) -> Vec<f32> {
        let geometry = PanGeometry::new(&layout(&[0.0])).unwrap();
        let signal = vec![1.0; 441];
        let other_signal = vec![0.5; 2 * MIX_FRAMES];
        let mut objects = [Object::SILENT; MAX_OBJECTS];
        let mut other_objects = [Object::SILENT; MAX_OBJECTS];
        let block = one(1, 44_100, 0, &signal, Position::FRONT, 1.0, &mut objects);
        let other = one(
            2,
            MIX_RATE,
            0,
            &other_signal,
            Position::FRONT,
            1.0,
            &mut other_objects,
        );
        let mut mixer = SceneMixer::new(config());
        mixer.lane_start(0, 0);
        mixer.lane_start(1, 0);
        mixer.push_scene(0, &block, Some(&geometry)).unwrap();
        mixer.push_scene(1, &other, Some(&geometry)).unwrap();
        for _ in 0..flushes {
            mixer.flush_lane(0, Some(&geometry)).unwrap();
        }
        let mut output = Vec::new();
        mixer.lane_stop(0, &mut |mixed| collect_direction(mixed, 0, &mut output));
        mixer.finish(&mut |mixed| collect_direction(mixed, 0, &mut output));
        let finished = output.clone();
        mixer.finish(&mut |mixed| collect_direction(mixed, 0, &mut output));
        assert_eq!(output, finished);
        output
    }
    assert_eq!(run(1), run(2));
}

#[test]
fn orderly_stop_flushes_the_same_fir_tail_as_explicit_zero_input() {
    fn run(rate: u32, flush: bool) -> Vec<f32> {
        let geometry = PanGeometry::new(&layout(&[0.0])).unwrap();
        let frames = rate as usize / 100;
        let mut signal = vec![0.0; frames];
        signal[frames - 1] = 1.0;
        let mut objects = [Object::SILENT; MAX_OBJECTS];
        let block = one(1, rate, 0, &signal, Position::FRONT, 1.0, &mut objects);
        let mut mixer = SceneMixer::new(config());
        mixer.push_scene(0, &block, Some(&geometry)).unwrap();
        if flush {
            mixer.flush_lane(0, Some(&geometry)).unwrap();
        } else {
            let zeros = vec![0.0; if rate >= 176_400 { 512 } else { 256 }];
            let mut zero_objects = [Object::SILENT; MAX_OBJECTS];
            let zero = one(1, rate, 0, &zeros, Position::FRONT, 1.0, &mut zero_objects);
            mixer.push_scene(0, &zero, Some(&geometry)).unwrap();
        }
        let mut output = Vec::new();
        mixer.lane_stop(0, &mut |mixed| collect_direction(mixed, 0, &mut output));
        output
    }
    for rate in [44_100, 88_200, 96_000, 176_400, 192_000] {
        let actual = run(rate, true);
        let expected = run(rate, false);
        assert_eq!(actual, expected, "{rate} Hz FIR tail");
        assert!(trim_trailing_zeros(&actual).len() > MIX_FRAMES);
    }
}

#[test]
fn rate_change_restart_preserves_the_previous_fir_tail() {
    fn run(next_rate: Option<u32>) -> Vec<f32> {
        let geometry = PanGeometry::new(&layout(&[0.0])).unwrap();
        let first_rate = 176_400;
        let mut signal = vec![0.0; first_rate as usize / 100];
        let final_frame = signal.len() - 1;
        signal[final_frame] = 1.0;
        let mut first_objects = [Object::SILENT; MAX_OBJECTS];
        let first = one(
            1,
            first_rate,
            0,
            &signal,
            Position::FRONT,
            1.0,
            &mut first_objects,
        );
        let mut mixer = SceneMixer::new(config());
        mixer.push_scene(0, &first, Some(&geometry)).unwrap();
        mixer.flush_lane(0, Some(&geometry)).unwrap();
        mixer.advance(0, MIX_FRAMES as u64, &mut |_| {});
        mixer.lane_discontinuity(0, 0);

        if let Some(rate) = next_rate {
            let silence = vec![0.0; rate as usize / 100];
            let mut next_objects = [Object::SILENT; MAX_OBJECTS];
            let next = one(
                2,
                rate,
                0,
                &silence,
                Position::FRONT,
                1.0,
                &mut next_objects,
            );
            mixer.push_scene(0, &next, Some(&geometry)).unwrap();
        }

        let mut output = Vec::new();
        mixer.finish(&mut |mixed| collect_direction(mixed, 0, &mut output));
        output
    }

    let stopped = trim_trailing_zeros(&run(None)).to_vec();
    assert!(stopped.iter().any(|sample| *sample != 0.0));
    for rate in [44_100, 88_200, 96_000, 192_000] {
        let restarted = run(Some(rate));
        assert_eq!(
            &restarted[..stopped.len()],
            stopped.as_slice(),
            "{rate} Hz restart"
        );
    }
}

#[test]
fn stopping_one_lane_does_not_drain_the_other_lane() {
    let geometry = PanGeometry::new(&layout(&[0.0])).unwrap();
    let primary_signal = vec![0.25; 2 * MIX_FRAMES];
    let second_signal = vec![0.5; 2 * MIX_FRAMES];
    let mut primary_objects = [Object::SILENT; MAX_OBJECTS];
    let mut second_objects = [Object::SILENT; MAX_OBJECTS];
    let primary = one(
        1,
        MIX_RATE,
        0,
        &primary_signal,
        Position::FRONT,
        1.0,
        &mut primary_objects,
    );
    let second = one(
        2,
        MIX_RATE,
        0,
        &second_signal,
        Position::FRONT,
        1.0,
        &mut second_objects,
    );
    let mut mixer = SceneMixer::new(config());
    mixer.lane_start(0, 0);
    mixer.lane_start(1, 0);
    mixer.push_scene(0, &primary, Some(&geometry)).unwrap();
    mixer.push_scene(1, &second, Some(&geometry)).unwrap();
    // Clocked lanes, not merely lanes with queued samples, prevent one lane's
    // stop from draining the common frontier.
    mixer.advance(1, 1, &mut |_| panic!("a partial interval is not ready"));
    mixer.advance(0, 1, &mut |_| panic!("a partial interval is not ready"));

    let mut stopped_blocks = 0;
    mixer.lane_stop(1, &mut |_| stopped_blocks += 1);
    assert_eq!(stopped_blocks, 0);
    let mut blocks = 0;
    mixer.advance(0, 2 * MIX_FRAMES as u64 - 1, &mut |_| blocks += 1);
    assert_eq!(blocks, 2);
}

#[test]
fn data_less_lane_holds_a_stop_tail_only_until_the_idle_clock() {
    let geometry = PanGeometry::new(&layout(&[0.0])).unwrap();
    let signal = vec![0.5; MIX_FRAMES + 96];
    let mut objects = [Object::SILENT; MAX_OBJECTS];
    let scene = one(1, MIX_RATE, 0, &signal, Position::FRONT, 1.0, &mut objects);
    let mut mixer = SceneMixer::new(config());
    mixer.lane_start(0, 0);
    mixer.lane_start(1, 0);
    mixer.push_scene(0, &scene, Some(&geometry)).unwrap();
    mixer.advance(0, (MIX_FRAMES + 96) as u64, &mut |_| {});

    // Lane 1 is active but has not advanced: its first data may be a moment
    // away, so the stop does not emit the partial interval it would fill.
    let mut stopped = 0;
    mixer.lane_stop(0, &mut |_| stopped += 1);
    assert_eq!(stopped, 0);

    // Lane 1 never advances (say, no decoder for its format). The idle clock
    // ignores it and plays lane 0's tail out in real time.
    let mut emitted = 0;
    let mut tail = false;
    let until = mixer.now() + 4 * MIX_FRAMES as u64;
    mixer.idle_to(until, &mut |bed| {
        emitted += 1;
        tail |= !bed.silent;
    });
    assert!(emitted >= 1);
    assert!(tail, "the idle clock must emit lane 0's remaining frames");
}

#[test]
fn a_data_less_lane_places_its_first_data_after_the_frontier() {
    let geometry = PanGeometry::new(&layout(&[0.0])).unwrap();
    let first_signal = vec![0.5; MIX_FRAMES + 96];
    let mut first_objects = [Object::SILENT; MAX_OBJECTS];
    let first = one(
        1,
        MIX_RATE,
        0,
        &first_signal,
        Position::FRONT,
        1.0,
        &mut first_objects,
    );
    let later_signal = vec![1.0; MIX_FRAMES];
    let mut later_objects = [Object::SILENT; MAX_OBJECTS];
    let later = one(
        2,
        MIX_RATE,
        0,
        &later_signal,
        Position::FRONT,
        1.0,
        &mut later_objects,
    );
    let mut mixer = SceneMixer::new(config());
    mixer.lane_start(0, 0);
    mixer.lane_start(1, 0);
    mixer.push_scene(0, &first, Some(&geometry)).unwrap();
    mixer.advance(0, (MIX_FRAMES + 96) as u64, &mut |_| {});
    mixer.lane_stop(0, &mut |_| {});

    mixer.push_scene(1, &later, Some(&geometry)).unwrap();
    let mut output = Vec::new();
    mixer.advance(1, MIX_FRAMES as u64, &mut |bed| {
        collect_direction(bed, 0, &mut output)
    });
    mixer.finish(&mut |bed| collect_direction(bed, 0, &mut output));
    let audible = output.iter().filter(|sample| **sample != 0.0).count();
    assert_eq!(audible, MIX_FRAMES);
    assert_eq!(mixer.dropped_frames(), 0);
    assert_eq!(mixer.lane_underrun_frames(), 0);
}

#[test]
fn a_lone_lane_leaves_its_filled_interval_without_waiting_for_the_slack() {
    let geometry = PanGeometry::new(&layout(&[0.0])).unwrap();
    let signal = vec![0.5; MIX_FRAMES];
    let mut objects = [Object::SILENT; MAX_OBJECTS];
    let block = one(1, MIX_RATE, 0, &signal, Position::FRONT, 1.0, &mut objects);
    let mut mixer = SceneMixer::new(MixerConfig {
        arrival_slack_frames: 96,
        ..config()
    });
    mixer.lane_start(0, 0);
    mixer.push_scene(0, &block, Some(&geometry)).unwrap();
    let mut emitted = 0;
    mixer.advance(0, MIX_FRAMES as u64, &mut |mixed| {
        emitted += 1;
        assert_eq!(mixed.channels, signal);
    });
    assert_eq!(emitted, 1);
    assert_eq!(mixer.lane_underrun_frames(), 0);
}

#[test]
fn an_interval_waits_up_to_the_slack_for_an_active_lane_that_has_not_filled_it() {
    let geometry = PanGeometry::new(&layout(&[0.0])).unwrap();
    let full_signal = vec![0.5; MIX_FRAMES];
    let half_signal = vec![0.25; MIX_FRAMES / 2];
    let mut full_objects = [Object::SILENT; MAX_OBJECTS];
    let mut half_objects = [Object::SILENT; MAX_OBJECTS];
    let full = one(
        1,
        MIX_RATE,
        0,
        &full_signal,
        Position::FRONT,
        1.0,
        &mut full_objects,
    );
    let half = one(
        1,
        MIX_RATE,
        0,
        &half_signal,
        Position::FRONT,
        1.0,
        &mut half_objects,
    );
    let mut mixer = SceneMixer::new(MixerConfig {
        arrival_slack_frames: 96,
        ..config()
    });
    mixer.lane_start(0, 0);
    mixer.lane_start(1, 0);
    mixer.push_scene(1, &half, Some(&geometry)).unwrap();
    mixer.advance(1, (MIX_FRAMES / 2) as u64, &mut |_| {
        panic!("emitted before full lane")
    });
    mixer.push_scene(0, &full, Some(&geometry)).unwrap();
    mixer.advance(0, MIX_FRAMES as u64, &mut |_| {
        panic!("emitted before slack")
    });

    let slack_signal = vec![0.5; 96];
    let mut slack_objects = [Object::SILENT; MAX_OBJECTS];
    let slack = one(
        1,
        MIX_RATE,
        0,
        &slack_signal,
        Position::FRONT,
        1.0,
        &mut slack_objects,
    );
    mixer.push_scene(0, &slack, Some(&geometry)).unwrap();
    let mut output = Vec::new();
    mixer.advance(0, 96, &mut |mixed| output.extend_from_slice(mixed.channels));
    assert_eq!(output.len(), MIX_FRAMES);
    assert!(
        output[..MIX_FRAMES / 2]
            .iter()
            .all(|sample| *sample == 0.75)
    );
    assert!(output[MIX_FRAMES / 2..].iter().all(|sample| *sample == 0.5));
    assert_eq!(mixer.lane_underrun_frames(), (MIX_FRAMES / 2) as u64);
}

#[test]
fn stop_pads_and_emits_the_final_partial_interval() {
    let geometry = PanGeometry::new(&layout(&[0.0])).unwrap();
    let signal = vec![0.5; 576];
    let mut objects = [Object::SILENT; MAX_OBJECTS];
    let block = one(1, MIX_RATE, 0, &signal, Position::FRONT, 1.0, &mut objects);
    let mut mixer = SceneMixer::new(config());
    mixer.push_scene(0, &block, Some(&geometry)).unwrap();
    let mut actual = Vec::new();
    mixer.lane_stop(0, &mut |mixed| collect_direction(mixed, 0, &mut actual));
    assert_eq!(actual.len(), 2 * MIX_FRAMES);
    assert!(actual[..576].iter().all(|sample| *sample == 0.5));
    assert!(actual[576..].iter().all(|sample| *sample == 0.0));
}

#[test]
fn silence_has_no_source_time_until_audio_gives_one() {
    // Before any lane has played, the idle clock's silent intervals have no
    // source time and are stamped 0 (unknown). Counted up from 0 instead, the
    // output took them for sound recorded at boot and reported the machine's
    // uptime as latency (p99 429 s in the CI loopback with DD+ and a warm
    // shared endpoint).
    let mut mixer = SceneMixer::new(config());
    let mut before = Vec::new();
    mixer.idle_to(0, &mut |_| {});
    mixer.idle_to(4 * MIX_FRAMES as u64, &mut |bed| {
        before.push((bed.silent, bed.source_qpc));
    });
    assert_eq!(before.len(), 4);
    assert!(
        before.iter().all(|&(silent, qpc)| silent && qpc == 0),
        "{before:?}"
    );

    // After audio, silence carries the last source time on, block by block.
    let geometry = PanGeometry::new(&layout(&[0.0])).unwrap();
    let signal = vec![0.5; MIX_FRAMES];
    let mut objects = [Object::SILENT; MAX_OBJECTS];
    let scene = one(
        1,
        MIX_RATE,
        5_000_000,
        &signal,
        Position::FRONT,
        1.0,
        &mut objects,
    );
    let mut mixer = SceneMixer::new(config());
    mixer.lane_start(0, 0);
    mixer.push_scene(0, &scene, Some(&geometry)).unwrap();
    let mut stamps = Vec::new();
    mixer.advance(0, MIX_FRAMES as u64, &mut |bed| stamps.push(bed.source_qpc));
    mixer.lane_stop(0, &mut |bed| stamps.push(bed.source_qpc));
    let until = mixer.now() + 3 * MIX_FRAMES as u64;
    mixer.idle_to(until, &mut |bed| stamps.push(bed.source_qpc));
    assert_eq!(stamps.first(), Some(&5_000_000));
    assert!(stamps.len() >= 3, "{stamps:?}");
    // 10 ms of the configured 1 MHz clock per block.
    assert!(
        stamps.windows(2).all(|pair| pair[1] == pair[0] + 10_000),
        "{stamps:?}"
    );
}

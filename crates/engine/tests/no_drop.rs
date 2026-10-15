#![forbid(unsafe_code)]

use assert_no_alloc::{AllocDisabler, assert_no_alloc};
use engine::{BedBlock, MIX_FRAMES, MIX_RATE, MixerConfig, SceneMixer};
use render::PanGeometry;
use ssr_types::{Direction, Layout, MAX_OBJECTS, Object, Position, SceneBlock};

#[global_allocator]
static ALLOCATOR: AllocDisabler = AllocDisabler;

fn config() -> MixerConfig {
    MixerConfig {
        arrival_slack_frames: 96,
        qpc_frequency: 1_000_000,
        endpoint_mask: u32::MAX,
    }
}

fn geometry() -> PanGeometry {
    PanGeometry::new(
        &Layout::new(vec![
            Direction::new("L", 30.0, 0.0),
            Direction::new("R", -30.0, 0.0),
        ])
        .unwrap(),
    )
    .unwrap()
}

fn block<'a>(
    epoch: u32,
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
        rate: MIX_RATE,
        source_qpc: 0,
        objects: &objects[..1],
        lfe: None,
        new_objects: 0,
    }
}

fn direction<'a>(block: &'a BedBlock<'a>, index: usize) -> &'a [f32] {
    let start = index * block.frames;
    &block.channels[start..start + block.frames]
}

fn identified(block: usize) -> Vec<f32> {
    (0..MIX_FRAMES)
        .map(|frame| (block * MIX_FRAMES + frame + 1) as f32)
        .collect()
}

fn run_left(with_right: bool, blocks: usize) -> Vec<u32> {
    let geometry = geometry();
    let mut mixer = SceneMixer::new(config());
    mixer.lane_start(0, 0);
    if with_right {
        mixer.lane_start(1, 0);
    }
    let mut output = Vec::new();
    for index in 0..blocks {
        let left_signal = identified(index);
        let mut left_objects = [Object::SILENT; MAX_OBJECTS];
        let left = block(
            1,
            &left_signal,
            Position::from_angles(30.0, 0.0),
            1.0,
            &mut left_objects,
        );
        mixer.push_scene(0, &left, Some(&geometry)).unwrap();
        if with_right {
            let right_signal = vec![-10_000.0 - index as f32; MIX_FRAMES];
            let mut right_objects = [Object::SILENT; MAX_OBJECTS];
            let right = block(
                2,
                &right_signal,
                Position::from_angles(-30.0, 0.0),
                1.0,
                &mut right_objects,
            );
            mixer.push_scene(1, &right, Some(&geometry)).unwrap();
        }
        mixer.advance(0, MIX_FRAMES as u64, &mut |mixed| {
            output.extend(direction(mixed, 0).iter().map(|sample| sample.to_bits()));
        });
        if with_right {
            mixer.advance(1, MIX_FRAMES as u64, &mut |mixed| {
                output.extend(direction(mixed, 0).iter().map(|sample| sample.to_bits()));
            });
        }
    }
    mixer.finish(&mut |mixed| {
        output.extend(direction(mixed, 0).iter().map(|sample| sample.to_bits()));
    });
    output
}

#[test]
fn two_regular_lanes_match_their_solo_sample_streams() {
    assert_eq!(run_left(true, 100), run_left(false, 100));
}

#[test]
fn ten_seconds_all_seven_one_and_stereo_signals_match_exact_solo_outputs() {
    const SOURCE_RATE: u32 = 44_100;
    const SOURCE_FRAMES: usize = 441;
    const BLOCKS: usize = 1_000;

    fn capture(block: &BedBlock<'_>, bed: &mut Vec<f32>, lfe: &mut Vec<f32>) {
        bed.extend_from_slice(block.channels);
        if let Some(samples) = block.lfe {
            lfe.extend_from_slice(samples);
        } else {
            lfe.resize(lfe.len() + block.frames, 0.0);
        }
    }

    let layout = Layout::new(vec![
        Direction::new("L", 30.0, 0.0),
        Direction::new("R", -30.0, 0.0),
        Direction::new("C", 0.0, 0.0),
        Direction::new("SL", 90.0, 0.0),
        Direction::new("SR", -90.0, 0.0),
        Direction::new("BL", 150.0, 0.0),
        Direction::new("BR", -150.0, 0.0),
    ])
    .unwrap();
    let geometry = PanGeometry::new(&layout).unwrap();
    let positions = [
        Position::from_angles(30.0, 0.0),
        Position::from_angles(-30.0, 0.0),
        Position::from_angles(0.0, 0.0),
        Position::from_angles(90.0, 0.0),
        Position::from_angles(-90.0, 0.0),
        Position::from_angles(150.0, 0.0),
        Position::from_angles(-150.0, 0.0),
    ];
    let mut combined = SceneMixer::new(config());
    let mut seven_one_solo = SceneMixer::new(config());
    let mut stereo_solo = SceneMixer::new(config());
    combined.lane_start(0, 0);
    combined.lane_start(1, 0);
    seven_one_solo.lane_start(0, 0);
    stereo_solo.lane_start(1, 0);

    let mut combined_bed = Vec::new();
    let mut combined_lfe = Vec::new();
    let mut seven_one_bed = Vec::new();
    let mut seven_one_lfe = Vec::new();
    let mut stereo_bed = Vec::new();
    let mut stereo_lfe = Vec::new();

    for block_index in 0..BLOCKS {
        let seven_signals: [Vec<f32>; 7] = std::array::from_fn(|channel| {
            (0..SOURCE_FRAMES)
                .map(|frame| {
                    let absolute = block_index * SOURCE_FRAMES + frame;
                    ((absolute % 257) as f32 - 128.0) * (channel + 1) as f32 * 1.0e-4
                })
                .collect()
        });
        let lfe_signal: Vec<f32> = (0..SOURCE_FRAMES)
            .map(|frame| {
                let absolute = block_index * SOURCE_FRAMES + frame;
                ((absolute % 193) as f32 - 96.0) * 3.0e-4
            })
            .collect();
        let mut seven_objects = [Object::SILENT; MAX_OBJECTS];
        for (index, (signal, position)) in seven_signals.iter().zip(positions).enumerate() {
            seven_objects[index] = Object {
                signal,
                position,
                gain: 1.0,
                spread: 0.0,
            };
        }
        let seven_one = SceneBlock {
            epoch: 1,
            frames: SOURCE_FRAMES,
            rate: SOURCE_RATE,
            source_qpc: block_index as u64 * 10_000,
            objects: &seven_objects[..7],
            lfe: Some(&lfe_signal),
            new_objects: 0,
        };

        let stereo_signals: [Vec<f32>; 2] = std::array::from_fn(|channel| {
            (0..SOURCE_FRAMES)
                .map(|frame| {
                    let absolute = block_index * SOURCE_FRAMES + frame;
                    ((absolute % 251) as f32 - 125.0) * (channel + 2) as f32 * 1.0e-4
                })
                .collect()
        });
        let mut stereo_objects = [Object::SILENT; MAX_OBJECTS];
        for index in 0..2 {
            stereo_objects[index] = Object {
                signal: &stereo_signals[index],
                position: positions[index],
                gain: 1.0,
                spread: 0.0,
            };
        }
        let stereo = SceneBlock {
            epoch: 2,
            frames: SOURCE_FRAMES,
            rate: SOURCE_RATE,
            source_qpc: block_index as u64 * 10_000,
            objects: &stereo_objects[..2],
            lfe: None,
            new_objects: 0,
        };

        combined.push_scene(0, &seven_one, Some(&geometry)).unwrap();
        combined.push_scene(1, &stereo, Some(&geometry)).unwrap();
        seven_one_solo
            .push_scene(0, &seven_one, Some(&geometry))
            .unwrap();
        stereo_solo.push_scene(1, &stereo, Some(&geometry)).unwrap();

        combined.advance(0, MIX_FRAMES as u64, &mut |block| {
            capture(block, &mut combined_bed, &mut combined_lfe)
        });
        combined.advance(1, MIX_FRAMES as u64, &mut |block| {
            capture(block, &mut combined_bed, &mut combined_lfe)
        });
        seven_one_solo.advance(0, MIX_FRAMES as u64, &mut |block| {
            capture(block, &mut seven_one_bed, &mut seven_one_lfe)
        });
        stereo_solo.advance(1, MIX_FRAMES as u64, &mut |block| {
            capture(block, &mut stereo_bed, &mut stereo_lfe)
        });
    }

    combined.finish(&mut |block| capture(block, &mut combined_bed, &mut combined_lfe));
    seven_one_solo.finish(&mut |block| capture(block, &mut seven_one_bed, &mut seven_one_lfe));
    stereo_solo.finish(&mut |block| capture(block, &mut stereo_bed, &mut stereo_lfe));

    assert_eq!(combined_bed.len(), seven_one_bed.len());
    assert_eq!(combined_bed.len(), stereo_bed.len());
    for (index, ((actual, seven_one), stereo)) in combined_bed
        .iter()
        .zip(&seven_one_bed)
        .zip(&stereo_bed)
        .enumerate()
    {
        assert_eq!(
            actual.to_bits(),
            (*seven_one + *stereo).to_bits(),
            "speaker-bed sample {index}"
        );
    }
    assert_eq!(combined_lfe.len(), seven_one_lfe.len());
    assert_eq!(combined_lfe.len(), stereo_lfe.len());
    for (index, ((actual, seven_one), stereo)) in combined_lfe
        .iter()
        .zip(&seven_one_lfe)
        .zip(&stereo_lfe)
        .enumerate()
    {
        assert_eq!(stereo.to_bits(), 0.0_f32.to_bits(), "stereo LFE {index}");
        assert_eq!(actual.to_bits(), seven_one.to_bits(), "LFE sample {index}");
    }
    assert_eq!(combined.dropped_frames(), 0);
    assert_eq!(combined.lane_underrun_frames(), 0);
    assert_eq!(seven_one_solo.dropped_frames(), 0);
    assert_eq!(seven_one_solo.lane_underrun_frames(), 0);
    assert_eq!(stereo_solo.dropped_frames(), 0);
    assert_eq!(stereo_solo.lane_underrun_frames(), 0);
}

fn run_with_right_restarts(restarts: bool) -> Vec<u32> {
    let geometry = geometry();
    let mut mixer = SceneMixer::new(config());
    mixer.lane_start(0, 0);
    if restarts {
        mixer.lane_start(1, 0);
    }
    let mut output = Vec::new();
    for index in 0..80 {
        let left_signal = identified(index);
        let mut left_objects = [Object::SILENT; MAX_OBJECTS];
        let left = block(
            1,
            &left_signal,
            Position::from_angles(30.0, 0.0),
            1.0,
            &mut left_objects,
        );
        mixer.push_scene(0, &left, Some(&geometry)).unwrap();
        if restarts && index % 4 == 0 {
            mixer.lane_start(1, 0);
            let right_signal = vec![0.5; MIX_FRAMES];
            let mut right_objects = [Object::SILENT; MAX_OBJECTS];
            let right = block(
                index as u32 + 1,
                &right_signal,
                Position::from_angles(-30.0, 0.0),
                1.0,
                &mut right_objects,
            );
            mixer.push_scene(1, &right, Some(&geometry)).unwrap();
        }
        mixer.advance(0, MIX_FRAMES as u64, &mut |mixed| {
            output.extend(direction(mixed, 0).iter().map(|sample| sample.to_bits()));
        });
        if restarts && index % 4 == 0 {
            mixer.lane_stop(1, &mut |mixed| {
                output.extend(direction(mixed, 0).iter().map(|sample| sample.to_bits()));
            });
        }
    }
    mixer.finish(&mut |mixed| {
        output.extend(direction(mixed, 0).iter().map(|sample| sample.to_bits()));
    });
    output
}

#[test]
fn twenty_seven_one_starts_and_stops_do_not_change_stereo() {
    assert_eq!(
        run_with_right_restarts(true),
        run_with_right_restarts(false)
    );
}

#[test]
fn discontinuity_and_stalled_lane_do_not_change_the_other() {
    let geometry = geometry();
    let mut mixed = SceneMixer::new(config());
    let mut solo = SceneMixer::new(config());
    mixed.lane_start(0, 0);
    mixed.lane_start(1, 0);
    solo.lane_start(0, 0);
    let mut mixed_output = Vec::new();
    let mut solo_output = Vec::new();
    for index in 0..40 {
        let signal = identified(index);
        let mut mixed_objects = [Object::SILENT; MAX_OBJECTS];
        let mixed_block = block(
            1,
            &signal,
            Position::from_angles(30.0, 0.0),
            1.0,
            &mut mixed_objects,
        );
        let mut solo_objects = [Object::SILENT; MAX_OBJECTS];
        let solo_block = block(
            1,
            &signal,
            Position::from_angles(30.0, 0.0),
            1.0,
            &mut solo_objects,
        );
        mixed.push_scene(0, &mixed_block, Some(&geometry)).unwrap();
        solo.push_scene(0, &solo_block, Some(&geometry)).unwrap();
        if index < 2 {
            let stalled_signal = vec![0.5; MIX_FRAMES];
            let mut stalled_objects = [Object::SILENT; MAX_OBJECTS];
            let stalled = block(
                1,
                &stalled_signal,
                Position::from_angles(-30.0, 0.0),
                1.0,
                &mut stalled_objects,
            );
            mixed.push_scene(1, &stalled, Some(&geometry)).unwrap();
        }
        if index == 10 || index == 20 || index == 30 {
            mixed.lane_discontinuity(1, 0);
        }
        mixed.advance(0, MIX_FRAMES as u64, &mut |bed| {
            mixed_output.extend(direction(bed, 0).iter().map(|sample| sample.to_bits()));
        });
        solo.advance(0, MIX_FRAMES as u64, &mut |bed| {
            solo_output.extend(direction(bed, 0).iter().map(|sample| sample.to_bits()));
        });
    }
    // The stalled lane holds the last interval for its slack; finish emits it.
    mixed.finish(&mut |bed| {
        mixed_output.extend(direction(bed, 0).iter().map(|sample| sample.to_bits()));
    });
    solo.finish(&mut |bed| {
        solo_output.extend(direction(bed, 0).iter().map(|sample| sample.to_bits()));
    });
    assert_eq!(mixed_output, solo_output);
}

#[test]
fn sustained_fast_lane_is_counted_and_faded_without_changing_the_other() {
    let geometry = geometry();
    let mut mixer = SceneMixer::new(config());
    mixer.lane_start(0, 0);
    mixer.lane_start(1, 0);
    let mut primary_output = Vec::new();
    for index in 0..80 {
        let primary_signal = identified(index);
        let mut primary_objects = [Object::SILENT; MAX_OBJECTS];
        let primary = block(
            1,
            &primary_signal,
            Position::from_angles(30.0, 0.0),
            1.0,
            &mut primary_objects,
        );
        mixer.push_scene(0, &primary, Some(&geometry)).unwrap();
        let fast_signal = vec![0.5; MIX_FRAMES + 48];
        let mut fast_objects = [Object::SILENT; MAX_OBJECTS];
        let fast = block(
            1,
            &fast_signal,
            Position::from_angles(-30.0, 0.0),
            1.0,
            &mut fast_objects,
        );
        mixer.push_scene(1, &fast, Some(&geometry)).unwrap();
        mixer.advance(0, MIX_FRAMES as u64, &mut |bed| {
            primary_output.extend(direction(bed, 0).iter().map(|sample| sample.to_bits()));
        });
        mixer.advance(1, MIX_FRAMES as u64, &mut |bed| {
            primary_output.extend(direction(bed, 0).iter().map(|sample| sample.to_bits()));
        });
    }
    mixer.finish(&mut |bed| {
        primary_output.extend(direction(bed, 0).iter().map(|sample| sample.to_bits()));
    });
    assert!(mixer.dropped_frames() > 0);
    assert_eq!(primary_output, run_left(false, 80));
}

#[test]
fn dropped_frames_are_not_counted_again_as_lane_underruns() {
    let geometry = geometry();
    let signal = vec![1.0; 1536];
    let mut objects = [Object::SILENT; MAX_OBJECTS];
    let scene = block(
        1,
        &signal,
        Position::from_angles(30.0, 0.0),
        1.0,
        &mut objects,
    );
    let mut mixer = SceneMixer::new(config());
    mixer.push_scene(0, &scene, Some(&geometry)).unwrap();
    mixer.push_scene(0, &scene, Some(&geometry)).unwrap();
    assert!(mixer.dropped_frames() > 0);
    mixer.finish(&mut |_| {});
    assert_eq!(mixer.lane_underrun_frames(), 0);
}

#[test]
fn more_than_eight_separated_ahead_drops_are_counted_once() {
    let geometry = geometry();
    let signal = vec![1.0; 3_000];
    let mut mixer = SceneMixer::new(config());
    mixer.lane_start(0, 0);

    for epoch in 0..12 {
        let mut objects = [Object::SILENT; MAX_OBJECTS];
        let scene = block(
            epoch + 1,
            &signal,
            Position::from_angles(30.0, 0.0),
            1.0,
            &mut objects,
        );
        mixer.push_scene(0, &scene, Some(&geometry)).unwrap();
        mixer.advance(0, 3_000, &mut |_| {});
    }
    mixer.finish(&mut |_| {});

    assert_eq!(mixer.dropped_frames(), 12 * 408);
    assert_eq!(mixer.lane_underrun_frames(), 0);
}

#[test]
fn pending_drop_beyond_the_current_ring_slot_is_not_an_underrun() {
    let geometry = geometry();
    let signal = vec![1.0; 3_000];
    let mut objects = [Object::SILENT; MAX_OBJECTS];
    let scene = block(
        1,
        &signal,
        Position::from_angles(30.0, 0.0),
        1.0,
        &mut objects,
    );
    let mut mixer = SceneMixer::new(config());
    mixer.push_scene(0, &scene, Some(&geometry)).unwrap();
    mixer.advance(0, 3_000, &mut |_| {});
    mixer.finish(&mut |_| {});

    assert_eq!(mixer.dropped_frames(), 408);
    assert_eq!(mixer.lane_underrun_frames(), 0);
}

#[test]
fn discontinuity_cut_counts_each_discarded_frame_once() {
    let geometry = geometry();
    let signal = vec![1.0; MIX_FRAMES];
    let mut objects = [Object::SILENT; MAX_OBJECTS];
    let scene = block(
        1,
        &signal,
        Position::from_angles(30.0, 0.0),
        1.0,
        &mut objects,
    );
    let mut mixer = SceneMixer::new(config());
    mixer.push_scene(0, &scene, Some(&geometry)).unwrap();
    mixer.lane_discontinuity(0, -64);
    mixer.finish(&mut |_| {});

    assert_eq!(mixer.dropped_frames(), (MIX_FRAMES - 64) as u64);
    assert_eq!(mixer.lane_underrun_frames(), 0);
}

#[test]
fn last_lane_drain_moves_new_lane_origin_to_output_boundary() {
    let geometry = geometry();
    let first_signal = vec![1.0; 576];
    let second_signal = vec![2.0; MIX_FRAMES];

    let run = |drain_first: bool| {
        let mut mixer = SceneMixer::new(config());
        if drain_first {
            let mut objects = [Object::SILENT; MAX_OBJECTS];
            let first = block(
                1,
                &first_signal,
                Position::from_angles(30.0, 0.0),
                1.0,
                &mut objects,
            );
            mixer.push_scene(0, &first, Some(&geometry)).unwrap();
            mixer.advance(0, 576, &mut |_| {});
            mixer.lane_stop(0, &mut |_| {});
        }
        let origin = mixer.now();
        mixer.lane_start(1, 0);
        let mut objects = [Object::SILENT; MAX_OBJECTS];
        let second = block(
            2,
            &second_signal,
            Position::from_angles(30.0, 0.0),
            1.0,
            &mut objects,
        );
        mixer.push_scene(1, &second, Some(&geometry)).unwrap();
        let mut output = Vec::new();
        mixer.advance(1, MIX_FRAMES as u64, &mut |mixed| {
            output.extend_from_slice(direction(mixed, 0));
        });
        mixer.finish(&mut |mixed| output.extend_from_slice(direction(mixed, 0)));
        (output, origin)
    };

    let drained = run(true);
    let solo = run(false);
    assert_eq!(drained.1, 2 * MIX_FRAMES as u64);
    assert_eq!(solo.1, 0);
    assert_eq!(drained.0, solo.0);
}

#[test]
fn emitted_frontier_remains_monotonic_after_last_lane_stops() {
    let geometry = geometry();
    let first_signal = vec![1.0; 576];
    let mut first_objects = [Object::SILENT; MAX_OBJECTS];
    let first = block(
        1,
        &first_signal,
        Position::from_angles(30.0, 0.0),
        1.0,
        &mut first_objects,
    );
    let mut mixer = SceneMixer::new(config());
    mixer.push_scene(0, &first, Some(&geometry)).unwrap();
    mixer.advance(0, 576, &mut |_| {});
    mixer.lane_stop(0, &mut |_| {});
    let frontier = mixer.now();

    mixer.lane_start(0, 0);
    let next_signal = vec![2.0; MIX_FRAMES];
    let mut next_objects = [Object::SILENT; MAX_OBJECTS];
    let next = block(
        2,
        &next_signal,
        Position::from_angles(30.0, 0.0),
        1.0,
        &mut next_objects,
    );
    mixer.push_scene(0, &next, Some(&geometry)).unwrap();
    let mut output = Vec::new();
    mixer.advance(0, MIX_FRAMES as u64, &mut |mixed| {
        output.extend_from_slice(direction(mixed, 0));
    });
    mixer.finish(&mut |mixed| output.extend_from_slice(direction(mixed, 0)));
    assert!(mixer.now() >= frontier);
    assert!(
        output
            .iter()
            .filter(|sample| **sample != 0.0)
            .all(|sample| *sample == 2.0)
    );
}

#[test]
fn representative_normal_paths_do_not_allocate() {
    let geometry = geometry();
    let signal = vec![1.0; MIX_FRAMES];
    let mut objects = [Object::SILENT; MAX_OBJECTS];
    let scene = block(
        1,
        &signal,
        Position::from_angles(30.0, 0.0),
        1.0,
        &mut objects,
    );
    let mut mixer = SceneMixer::new(config());
    assert_no_alloc(|| {
        mixer.lane_start(0, 0);
        mixer.push_scene(0, &scene, Some(&geometry)).unwrap();
        mixer.advance(0, MIX_FRAMES as u64, &mut |_| {});
        mixer.lane_discontinuity(0, 0);
        mixer.push_scene(0, &scene, Some(&geometry)).unwrap();
        mixer.lane_stop(0, &mut |_| {});
        mixer.finish(&mut |_| {});
    });
}

/// 200 ms at 48 kHz: the largest delay a lane takes (the mixer's
/// `MAX_DELAY_FRAMES`, private to it).
const MAX_DELAY_FRAMES: u64 = 9_600;
const DELIVERY: usize = 1_536;

/// A signal whose samples name their frame in the lane's stream (1-based).
fn counted(first: usize, frames: usize) -> Vec<f32> {
    (first..first + frames)
        .map(|frame| (frame + 1) as f32)
        .collect()
}

fn push_counted(mixer: &mut SceneMixer, geometry: &PanGeometry, first: usize, frames: usize) {
    let signal = counted(first, frames);
    let mut objects = [Object::SILENT; MAX_OBJECTS];
    let scene = block(
        1,
        &signal,
        Position::from_angles(30.0, 0.0),
        1.0,
        &mut objects,
    );
    mixer.push_scene(0, &scene, Some(geometry)).unwrap();
}

#[test]
fn scenes_that_arrive_together_at_a_generation_start_do_not_cut_later_scenes() {
    const SCENES: usize = 43;
    // `batch` deliveries' bytes arrive (a backlog READ) before any of their
    // scenes; the scenes then come back to back, and the lane goes on one
    // delivery of bytes, then its scene, per cycle.
    let run = |batch: usize| {
        let geometry = geometry();
        let mut mixer = SceneMixer::new(config());
        mixer.lane_start(0, 0);
        let mut output = Vec::new();
        for _ in 0..batch {
            // Before its first scene the mixer has no layout: silent beds.
            mixer.advance(0, DELIVERY as u64, &mut |bed| {
                assert!(bed.silent);
                output.extend(std::iter::repeat_n(0.0_f32.to_bits(), bed.frames));
            });
        }
        for scene in 0..batch {
            push_counted(&mut mixer, &geometry, scene * DELIVERY, DELIVERY);
            assert_eq!(mixer.dropped_frames(), 0, "batch {batch}, scene {scene}");
        }
        for scene in batch..SCENES {
            mixer.advance(0, DELIVERY as u64, &mut |bed| {
                output.extend(direction(bed, 0).iter().map(|sample| sample.to_bits()));
            });
            push_counted(&mut mixer, &geometry, scene * DELIVERY, DELIVERY);
            assert_eq!(mixer.dropped_frames(), 0, "batch {batch}, scene {scene}");
        }
        mixer.finish(&mut |bed| {
            output.extend(direction(bed, 0).iter().map(|sample| sample.to_bits()));
        });
        assert_eq!(mixer.dropped_frames(), 0);
        assert_eq!(mixer.lane_underrun_frames(), 0);
        output
    };
    // One scene per cycle from the start: its first scene waits for one
    // delivery of bytes and starts at the frontier, 1,440 frames in. Three
    // together: the first waits for three and starts 4,320 frames in, and the
    // lane's delay carries the batch. The measured shift is the difference,
    // 2,880 frames (six intervals); after it every sample is the same.
    let one_per_cycle = run(1);
    let batched = run(3);
    let shift = batched.len() - one_per_cycle.len();
    assert_eq!(shift, 2_880);
    assert!(
        batched[..shift]
            .iter()
            .all(|bits| f32::from_bits(*bits) == 0.0)
    );
    assert_eq!(&batched[shift..], one_per_cycle.as_slice());
}

#[test]
fn a_lane_later_than_the_delay_cap_drops_the_late_head_and_holds_the_cap() {
    let geometry = geometry();
    let mut mixer = SceneMixer::new(config());
    mixer.lane_start(0, 0);
    let mut output = Vec::new();
    let mut produced = 0;
    // A regular lane: each 10 ms scene before its 10 ms of bytes, delay 0.
    for _ in 0..4 {
        push_counted(&mut mixer, &geometry, produced, MIX_FRAMES);
        produced += MIX_FRAMES;
        mixer.advance(0, MIX_FRAMES as u64, &mut |bed| {
            output.extend_from_slice(direction(bed, 0));
        });
    }
    // The decoder stalls while bytes keep arriving: intervals are emitted late
    // without this lane until it is the cap and two intervals behind.
    for _ in 0..23 {
        mixer.advance(0, MIX_FRAMES as u64, &mut |bed| {
            output.extend_from_slice(direction(bed, 0));
        });
    }
    let frontier = output.len();
    assert_eq!(
        (frontier - produced) as u64,
        MAX_DELAY_FRAMES + 2 * MIX_FRAMES as u64
    );
    assert_eq!(mixer.dropped_frames(), 0);

    // Its next scene: the delay grows to the cap, the excess is dropped from
    // the head, and the rest lands on the frontier.
    push_counted(&mut mixer, &geometry, produced, DELIVERY);
    let late = 2 * MIX_FRAMES;
    assert_eq!(mixer.dropped_frames(), late as u64);
    produced += DELIVERY;
    // The next cycles place with no further drop.
    for _ in 0..6 {
        mixer.advance(0, MIX_FRAMES as u64, &mut |bed| {
            output.extend_from_slice(direction(bed, 0));
        });
        push_counted(&mut mixer, &geometry, produced, MIX_FRAMES);
        produced += MIX_FRAMES;
        assert_eq!(mixer.dropped_frames(), late as u64);
    }
    mixer.finish(&mut |bed| output.extend_from_slice(direction(bed, 0)));
    assert_eq!(mixer.dropped_frames(), late as u64);

    // Frame `p` of the stream is heard at timeline position `p + delay`: 0
    // before the stall, the cap after it. The first frame after the stall is
    // the first one past the dropped head, on the frontier.
    let gain = output[0];
    assert!(gain > 0.0);
    let heard = |position: usize| (output[position] / gain).round() as usize;
    for (position, sample) in output.iter().enumerate().take(4 * MIX_FRAMES) {
        assert_eq!(sample.to_bits(), ((position + 1) as f32 * gain).to_bits());
    }
    assert!(
        output[4 * MIX_FRAMES..frontier]
            .iter()
            .all(|sample| *sample == 0.0)
    );
    assert_eq!(heard(frontier) - 1, 4 * MIX_FRAMES + late);
    let resumed = &output[frontier..frontier + DELIVERY - late + 6 * MIX_FRAMES];
    for (offset, sample) in resumed.iter().enumerate() {
        let position = frontier + offset;
        let frame = position as u64 - MAX_DELAY_FRAMES;
        assert_eq!(
            sample.to_bits(),
            ((frame + 1) as f32 * gain).to_bits(),
            "position {position}"
        );
    }
}

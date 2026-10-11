#![forbid(unsafe_code)]

use bio_types::{Direction, DirectionId, Feeds, Layout, Object, Position, SceneBlock};
use proptest::prelude::*;
use render::{Panner, PannerConfig, RenderError};

fn layout(entries: &[(&str, f32, f32)]) -> Layout {
    Layout::new(
        entries
            .iter()
            .map(|&(name, azimuth, elevation)| Direction::new(name, azimuth, elevation))
            .collect(),
    )
    .unwrap()
}

fn seven_one_four_layout() -> Layout {
    layout(&[
        ("FL", 30.0, 0.0),
        ("FR", -30.0, 0.0),
        ("FC", 0.0, 0.0),
        ("SL", 90.0, 0.0),
        ("SR", -90.0, 0.0),
        ("BL", 150.0, 0.0),
        ("BR", -150.0, 0.0),
        ("TFL", 45.0, 45.0),
        ("TFR", -45.0, 45.0),
        ("TBL", 135.0, 45.0),
        ("TBR", -135.0, 45.0),
    ])
}

fn dense_twenty_one_layout() -> Layout {
    layout(&[
        ("FL", 30.0, 0.0),
        ("FR", -30.0, 0.0),
        ("FC", 0.0, 0.0),
        ("SL", 90.0, 0.0),
        ("SR", -90.0, 0.0),
        ("BL", 150.0, 0.0),
        ("BR", -150.0, 0.0),
        ("TFL", 45.0, 45.0),
        ("TFR", -45.0, 45.0),
        ("TBL", 135.0, 45.0),
        ("TBR", -135.0, 45.0),
        ("WL", 60.0, 0.0),
        ("WR", -60.0, 0.0),
        ("TSL", 90.0, 45.0),
        ("TSR", -90.0, 45.0),
        ("FCH", 0.0, 45.0),
        ("BC", 180.0, 0.0),
        ("BCH", 180.0, 45.0),
        ("TIFL", 22.5, 45.0),
        ("TIFR", -22.5, 45.0),
        ("TOP", 0.0, 90.0),
    ])
}

fn default_panner(layout: &Layout) -> Panner {
    Panner::new(layout, PannerConfig::default()).unwrap()
}

fn assert_unit_power(gains: &[f32]) {
    assert!(gains.iter().all(|gain| *gain >= 0.0));
    let power: f32 = gains.iter().map(|gain| gain * gain).sum();
    assert!((power - 1.0).abs() <= 1.0e-5, "power was {power}");
}

fn assert_on_direction_exactness(layout: &Layout) {
    let panner = default_panner(layout);
    let mut gains = vec![0.0; layout.len()];
    for (expected, direction) in layout.directions().iter().enumerate() {
        panner.gains(direction.position(), &mut gains);
        for (actual, gain) in gains.iter().enumerate() {
            let expected_gain = f32::from(actual == expected);
            assert!(
                (*gain - expected_gain).abs() <= 1.0e-6,
                "{} selected direction {actual} with gain {gain}",
                direction.name
            );
        }
    }
}

proptest! {
    #[test]
    fn random_3d_gains_are_non_negative_and_unit_power(
        x in -1.0_f32..=1.0,
        y in -1.0_f32..=1.0,
        z in -1.0_f32..=1.0,
    ) {
        let layout = seven_one_four_layout();
        let panner = default_panner(&layout);
        let mut gains = vec![0.0; layout.len()];
        panner.gains(Position::new(x, y, z), &mut gains);
        prop_assert!(gains.iter().all(|gain| *gain >= 0.0));
        let power: f32 = gains.iter().map(|gain| gain * gain).sum();
        prop_assert!((power - 1.0).abs() <= 1.0e-5, "power was {power}");
    }
}

#[test]
fn random_3d_gains_stay_unit_power_for_many_samples() {
    let layout = seven_one_four_layout();
    let panner = default_panner(&layout);
    let mut gains = vec![0.0; layout.len()];
    let mut state = 0x1234_5678_u32;
    for _ in 0..100_000 {
        let mut next_coordinate = || {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (state as f32 / u32::MAX as f32).mul_add(2.0, -1.0)
        };
        panner.gains(
            Position::new(next_coordinate(), next_coordinate(), next_coordinate()),
            &mut gains,
        );
        assert_unit_power(&gains);
    }
}

#[test]
fn seven_one_four_is_exact_on_every_measured_direction() {
    assert_on_direction_exactness(&seven_one_four_layout());
}

#[test]
fn dense_twenty_one_is_exact_on_every_measured_direction() {
    assert_on_direction_exactness(&dense_twenty_one_layout());
}

#[test]
fn centre_and_pure_vertical_positions_follow_layout_rules() {
    let horizontal = layout(&[
        ("FL", 30.0, 0.0),
        ("FR", -30.0, 0.0),
        ("SL", 110.0, 0.0),
        ("SR", -110.0, 0.0),
    ]);
    let horizontal_panner = default_panner(&horizontal);
    let mut horizontal_gains = vec![0.0; horizontal.len()];
    for position in [Position::default(), Position::new(0.0, 0.0, 1.0)] {
        horizontal_panner.gains(position, &mut horizontal_gains);
        assert_unit_power(&horizontal_gains);
        assert!(
            horizontal_gains
                .iter()
                .all(|gain| (*gain - 0.5).abs() <= 1.0e-6)
        );
    }

    let three_dimensional = seven_one_four_layout();
    let panner = default_panner(&three_dimensional);
    let mut gains = vec![0.0; three_dimensional.len()];
    panner.gains(Position::default(), &mut gains);
    assert_unit_power(&gains);
    let expected = (three_dimensional.len() as f32).sqrt().recip();
    assert!(gains.iter().all(|gain| (*gain - expected).abs() <= 1.0e-6));
}

#[test]
fn ceiling_centre_uses_only_the_four_top_directions() {
    let layout = seven_one_four_layout();
    let panner = default_panner(&layout);
    let mut gains = vec![0.0; layout.len()];
    panner.gains(Position::new(0.0, 0.0, 1.0), &mut gains);

    assert_unit_power(&gains);
    for (index, direction) in layout.directions().iter().enumerate() {
        if direction.name.starts_with('T') {
            assert!((gains[index] - 0.5).abs() <= 1.0e-6);
        } else {
            assert_eq!(gains[index], 0.0);
        }
    }
}

#[test]
fn virtual_nadir_keeps_unit_power_on_ear_level_directions() {
    let layout = seven_one_four_layout();
    let panner = default_panner(&layout);
    let mut gains = vec![0.0; layout.len()];
    panner.gains(Position::new(0.0, 0.0, -1.0), &mut gains);

    assert_unit_power(&gains);
    let expected = (7.0_f32).sqrt().recip();
    for (index, direction) in layout.directions().iter().enumerate() {
        if direction.elevation_deg == 0.0 {
            assert!((gains[index] - expected).abs() <= 1.0e-6);
        } else {
            assert_eq!(gains[index], 0.0);
        }
    }
}

#[test]
fn horizontal_five_one_uses_at_most_two_directions() {
    let layout = layout(&[
        ("FL", 30.0, 0.0),
        ("FR", -30.0, 0.0),
        ("FC", 0.0, 0.0),
        ("SL", 110.0, 0.0),
        ("SR", -110.0, 0.0),
    ]);
    let panner = default_panner(&layout);
    let mut gains = vec![0.0; layout.len()];

    for azimuth in (-180..180).step_by(3) {
        panner.gains(Position::from_angles(azimuth as f32, 0.0), &mut gains);
        assert_unit_power(&gains);
        assert!(gains.iter().filter(|gain| **gain > 1.0e-6).count() <= 2);
    }
}

#[test]
fn near_horizontal_layout_projects_measured_elevations_to_azimuth() {
    let layout = layout(&[("L", 45.0, 5.0), ("R", -45.0, -5.0)]);
    let panner = default_panner(&layout);
    let mut gains = vec![0.0; layout.len()];
    panner.gains(Position::from_angles(0.0, 40.0), &mut gains);
    assert_unit_power(&gains);
    let expected = 2.0_f32.sqrt().recip();
    assert!((gains[0] - expected).abs() <= 1.0e-6);
    assert!((gains[1] - expected).abs() <= 1.0e-6);
}

#[test]
fn two_direction_arc_pans_between_endpoints() {
    let layout = layout(&[("FL", 30.0, 0.0), ("FR", -30.0, 0.0)]);
    let panner = default_panner(&layout);
    let mut gains = vec![0.0; layout.len()];
    panner.gains(Position::from_angles(0.0, 0.0), &mut gains);
    assert_unit_power(&gains);
    let expected = 2.0_f32.sqrt().recip();
    assert!((gains[0] - expected).abs() <= 1.0e-6);
    assert!((gains[1] - expected).abs() <= 1.0e-6);
}

#[test]
fn moving_object_ramps_across_the_block_boundary_and_reset_skips_it() {
    let layout = layout(&[("FL", 30.0, 0.0), ("FR", -30.0, 0.0)]);
    let config = PannerConfig {
        lfe_route: None,
        ramp_frames: 4,
    };
    let mut panner = Panner::new(&layout, config).unwrap();
    let signal = [1.0; 8];
    let old_objects = [Object {
        signal: &signal,
        position: Position::from_angles(30.0, 0.0),
        gain: 1.0,
        spread: 0.0,
    }];
    let new_objects = [Object {
        signal: &signal,
        position: Position::from_angles(-30.0, 0.0),
        gain: 1.0,
        spread: 0.0,
    }];
    let old_scene = SceneBlock {
        epoch: 0,
        frames: signal.len(),
        rate: 48_000,
        source_qpc: 0,
        objects: &old_objects,
        lfe: None,
    };
    let new_scene = SceneBlock {
        objects: &new_objects,
        ..old_scene
    };
    let mut feeds = Feeds::new(layout.len(), signal.len());

    panner.render(&old_scene, &mut feeds);
    let old_last = [feeds.channel(0)[7], feeds.channel(1)[7]];
    panner.render(&new_scene, &mut feeds);
    for (direction, previous) in old_last.into_iter().enumerate() {
        assert!((feeds.channel(direction)[0] - previous).abs() <= 0.25 + 1.0e-6);
    }
    assert!((feeds.channel(0)[0] - 0.75).abs() <= 1.0e-6);
    assert!((feeds.channel(1)[0] - 0.25).abs() <= 1.0e-6);
    assert_eq!(feeds.channel(0)[4], 0.0);
    assert_eq!(feeds.channel(1)[4], 1.0);

    panner.render(&old_scene, &mut feeds);
    panner.reset();
    panner.render(&new_scene, &mut feeds);
    assert!(feeds.channel(0).iter().all(|sample| *sample == 0.0));
    assert!(feeds.channel(1).iter().all(|sample| *sample == 1.0));
}

#[test]
fn returning_object_index_starts_without_a_ramp() {
    let layout = layout(&[("FL", 30.0, 0.0), ("FR", -30.0, 0.0)]);
    let mut panner = Panner::new(
        &layout,
        PannerConfig {
            lfe_route: None,
            ramp_frames: 4,
        },
    )
    .unwrap();
    let signal = [1.0; 4];
    let left = [Object {
        signal: &signal,
        position: Position::from_angles(30.0, 0.0),
        gain: 1.0,
        spread: 0.0,
    }];
    let right = [Object {
        signal: &signal,
        position: Position::from_angles(-30.0, 0.0),
        gain: 1.0,
        spread: 0.0,
    }];
    let left_scene = SceneBlock {
        epoch: 0,
        frames: signal.len(),
        rate: 48_000,
        source_qpc: 0,
        objects: &left,
        lfe: None,
    };
    let empty_scene = SceneBlock {
        objects: &[],
        ..left_scene
    };
    let right_scene = SceneBlock {
        objects: &right,
        ..left_scene
    };
    let mut feeds = Feeds::new(layout.len(), signal.len());

    panner.render(&left_scene, &mut feeds);
    panner.render(&empty_scene, &mut feeds);
    panner.render(&right_scene, &mut feeds);

    assert!(feeds.channel(0).iter().all(|sample| *sample == 0.0));
    assert!(feeds.channel(1).iter().all(|sample| *sample == 1.0));
}

#[test]
fn lfe_route_and_object_gain_are_applied() {
    let layout = layout(&[("FL", 30.0, 0.0), ("FR", -30.0, 0.0)]);
    let config = PannerConfig {
        lfe_route: Some((DirectionId(1), 0.5)),
        ramp_frames: 64,
    };
    let mut panner = Panner::new(&layout, config).unwrap();
    let signal = [2.0; 4];
    let lfe = [3.0; 4];
    let objects = [Object {
        signal: &signal,
        position: Position::from_angles(30.0, 0.0),
        gain: 0.25,
        spread: 1.0,
    }];
    let scene = SceneBlock {
        epoch: 0,
        frames: signal.len(),
        rate: 48_000,
        source_qpc: 0,
        objects: &objects,
        lfe: Some(&lfe),
    };
    let mut feeds = Feeds::new(layout.len(), signal.len());

    panner.render(&scene, &mut feeds);

    assert_eq!(feeds.channel(0), &[0.5; 4]);
    assert_eq!(feeds.channel(1), &[1.5; 4]);
}

#[test]
fn degenerate_layout_and_bad_lfe_route_are_rejected() {
    let coincident = layout(&[("A", 0.0, 0.0), ("B", 0.0, 0.0)]);
    assert!(matches!(
        Panner::new(&coincident, PannerConfig::default()),
        Err(RenderError::DegenerateLayout(_))
    ));
    let equivalent_angles = layout(&[("A", 0.0, 0.0), ("B", 360.0, 0.0)]);
    assert!(matches!(
        Panner::new(&equivalent_angles, PannerConfig::default()),
        Err(RenderError::DegenerateLayout(_))
    ));

    let coplanar = layout(&[
        ("FRONT", 0.0, 0.0),
        ("BACK", 180.0, 0.0),
        ("UP", 0.0, 90.0),
        ("DOWN", 0.0, -90.0),
    ]);
    assert!(matches!(
        Panner::new(&coplanar, PannerConfig::default()),
        Err(RenderError::DegenerateLayout(_))
    ));

    let horizontal_opposites = layout(&[("FRONT", 0.0, 0.0), ("BACK", 180.0, 0.0)]);
    assert!(matches!(
        Panner::new(&horizontal_opposites, PannerConfig::default()),
        Err(RenderError::DegenerateLayout(_))
    ));

    let vertical_single = layout(&[("TOP", 0.0, 90.0)]);
    let vertical_panner = default_panner(&vertical_single);
    let mut single_gain = [0.0];
    vertical_panner.gains(Position::new(0.0, 0.0, -1.0), &mut single_gain);
    assert_eq!(single_gain, [1.0]);

    let valid = layout(&[("ONLY", 0.0, 0.0)]);
    assert_eq!(
        Panner::new(
            &valid,
            PannerConfig {
                lfe_route: Some((DirectionId(1), 1.0)),
                ramp_frames: 0,
            },
        )
        .unwrap_err(),
        RenderError::BadLfeRoute(1)
    );
}

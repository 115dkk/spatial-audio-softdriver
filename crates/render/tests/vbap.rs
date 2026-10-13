#![forbid(unsafe_code)]

use bio_types::{Direction, Layout, Position};
use proptest::prelude::*;
use render::{PanGeometry, RenderError};

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

fn twenty_two_direction_layout() -> Layout {
    layout(&[
        ("FL", 30.0, 0.0),
        ("FR", -30.0, 0.0),
        ("FC", 0.0, 0.0),
        ("SL", 90.0, 0.0),
        ("SR", -90.0, 0.0),
        ("BL", 150.0, 0.0),
        ("BR", -150.0, 0.0),
        ("BC", 180.0, 0.0),
        ("WL", 60.0, 0.0),
        ("WR", -60.0, 0.0),
        ("TFL", 45.0, 45.0),
        ("TFR", -45.0, 45.0),
        ("TBL", 135.0, 45.0),
        ("TBR", -135.0, 45.0),
        ("TSL", 90.0, 45.0),
        ("TSR", -90.0, 45.0),
        ("FCH", 0.0, 45.0),
        ("BCH", 180.0, 45.0),
        ("TIFL", 22.5, 45.0),
        ("TIFR", -22.5, 45.0),
        ("TIBL", 157.5, 45.0),
        ("TIBR", -157.5, 45.0),
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

fn assert_unit_power(gains: &[f32]) {
    assert!(gains.iter().all(|gain| *gain >= 0.0));
    let power: f32 = gains.iter().map(|gain| gain * gain).sum();
    assert!((power - 1.0).abs() <= 1.0e-5, "power was {power}");
}

fn assert_on_direction_exactness(layout: &Layout) {
    let panner = PanGeometry::new(layout).unwrap();
    let mut gains = vec![0.0; layout.len()];
    for (expected, direction) in layout.directions().iter().enumerate() {
        panner.gains(direction.position(), 0.0, &mut gains);
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
        let panner = PanGeometry::new(&layout).unwrap();
        let mut gains = vec![0.0; layout.len()];
        panner.gains(Position::new(x, y, z), 0.0, &mut gains);
        prop_assert!(gains.iter().all(|gain| *gain >= 0.0));
        let power: f32 = gains.iter().map(|gain| gain * gain).sum();
        prop_assert!((power - 1.0).abs() <= 1.0e-5, "power was {power}");
    }
}

#[test]
fn random_3d_gains_stay_unit_power_for_many_samples() {
    let layout = seven_one_four_layout();
    let panner = PanGeometry::new(&layout).unwrap();
    let mut gains = vec![0.0; layout.len()];
    let mut state = 0x1234_5678_u32;
    for _ in 0..100_000 {
        let mut next_coordinate = || {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (state as f32 / u32::MAX as f32).mul_add(2.0, -1.0)
        };
        panner.gains(
            Position::new(next_coordinate(), next_coordinate(), next_coordinate()),
            0.0,
            &mut gains,
        );
        assert_unit_power(&gains);
    }
}

#[test]
fn spread_keeps_unit_power() {
    let layouts = [
        seven_one_four_layout(),
        twenty_two_direction_layout(),
        layout(&[
            ("FL", 30.0, 0.0),
            ("FR", -30.0, 0.0),
            ("SL", 110.0, 0.0),
            ("SR", -110.0, 0.0),
        ]),
    ];
    let mut state = 0x4d44_4150_u32;
    for layout in layouts {
        let geometry = PanGeometry::new(&layout).unwrap();
        let mut gains = vec![0.0; layout.len()];
        for _ in 0..2_000 {
            let mut random = || {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                state as f32 / u32::MAX as f32
            };
            let position = Position::new(
                random().mul_add(2.0, -1.0),
                random().mul_add(2.0, -1.0),
                random().mul_add(2.0, -1.0),
            );
            geometry.gains(position, random().mul_add(1.4, -0.2), &mut gains);
            assert_unit_power(&gains);
        }
    }
}

#[test]
fn wider_spread_reaches_more_directions() {
    let layout = twenty_two_direction_layout();
    let geometry = PanGeometry::new(&layout).unwrap();
    let mut gains = vec![0.0; layout.len()];
    let mut previous = 0;
    for step in 0..=20 {
        geometry.gains(
            Position::from_angles(17.0, 18.0),
            step as f32 / 20.0,
            &mut gains,
        );
        let active = gains.iter().filter(|gain| **gain > 1.0e-3).count();
        assert!(active >= previous, "step {step}: {active} < {previous}");
        previous = active;
    }
}

#[test]
fn full_spread_is_nearly_diffuse() {
    let layout = twenty_two_direction_layout();
    let geometry = PanGeometry::new(&layout).unwrap();
    let mut gains = vec![0.0; layout.len()];
    geometry.gains(Position::from_angles(17.0, 18.0), 1.0, &mut gains);
    let minimum = gains.iter().copied().fold(f32::INFINITY, f32::min);
    let maximum = gains.iter().copied().fold(0.0, f32::max);
    assert!(maximum / minimum <= 10.0, "ratio was {}", maximum / minimum);
}

#[test]
fn spread_is_mirror_symmetric() {
    let layout = layout(&[
        ("FL", 30.0, 0.0),
        ("FR", -30.0, 0.0),
        ("SL", 90.0, 0.0),
        ("SR", -90.0, 0.0),
        ("BL", 150.0, 0.0),
        ("BR", -150.0, 0.0),
    ]);
    let geometry = PanGeometry::new(&layout).unwrap();
    let mut left = vec![0.0; layout.len()];
    let mut right = vec![0.0; layout.len()];
    geometry.gains(Position::new(-0.4, 0.7, 0.0), 0.63, &mut left);
    geometry.gains(Position::new(0.4, 0.7, 0.0), 0.63, &mut right);
    for pair in (0..layout.len()).step_by(2) {
        assert!(
            (left[pair] - right[pair + 1]).abs() <= 2.0e-5,
            "pair {pair}: {:?} != {:?}",
            left,
            right
        );
        assert!((left[pair + 1] - right[pair]).abs() <= 2.0e-5);
    }
}

#[test]
fn spread_is_continuous() {
    let layout = twenty_two_direction_layout();
    let geometry = PanGeometry::new(&layout).unwrap();
    let mut previous = vec![0.0; layout.len()];
    let mut current = vec![0.0; layout.len()];
    geometry.gains(Position::from_angles(17.0, 18.0), 0.0, &mut previous);
    let mut maximum_change = 0.0_f32;
    for step in 1..=100 {
        geometry.gains(
            Position::from_angles(17.0, 18.0),
            step as f32 / 100.0,
            &mut current,
        );
        maximum_change = maximum_change.max(
            previous
                .iter()
                .zip(&current)
                .map(|(left, right)| (left - right).abs())
                .fold(0.0, f32::max),
        );
        previous.copy_from_slice(&current);
    }
    assert!(
        maximum_change <= 0.063,
        "maximum change was {maximum_change}"
    );
}

#[test]
fn spread_is_continuous_across_the_zenith() {
    let layout = twenty_two_direction_layout();
    let geometry = PanGeometry::new(&layout).unwrap();
    let mut previous = vec![0.0; layout.len()];
    let mut current = vec![0.0; layout.len()];
    let path = [
        Position::from_angles(0.0, 89.9),
        Position::from_angles(0.0, 90.0),
        Position::from_angles(180.0, 89.9),
    ];
    geometry.gains(path[0], 0.65, &mut previous);
    let mut maximum_change = 0.0_f32;
    for position in path.into_iter().skip(1) {
        geometry.gains(position, 0.65, &mut current);
        maximum_change = maximum_change.max(
            previous
                .iter()
                .zip(&current)
                .map(|(left, right)| (left - right).abs())
                .fold(0.0, f32::max),
        );
        previous.copy_from_slice(&current);
    }
    // The minimal-rotation basis measures 0.0015457952 on this three-point path;
    // 0.0016 leaves only a small allowance for platform arithmetic variation.
    assert!(
        maximum_change <= 0.0016,
        "maximum change was {maximum_change}"
    );
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
    let horizontal_panner = PanGeometry::new(&horizontal).unwrap();
    let mut horizontal_gains = vec![0.0; horizontal.len()];
    for position in [Position::default(), Position::new(0.0, 0.0, 1.0)] {
        horizontal_panner.gains(position, 0.0, &mut horizontal_gains);
        assert_unit_power(&horizontal_gains);
        assert!(
            horizontal_gains
                .iter()
                .all(|gain| (*gain - 0.5).abs() <= 1.0e-6)
        );
    }

    let three_dimensional = seven_one_four_layout();
    let panner = PanGeometry::new(&three_dimensional).unwrap();
    let mut gains = vec![0.0; three_dimensional.len()];
    panner.gains(Position::default(), 0.0, &mut gains);
    assert_unit_power(&gains);
    let expected = (three_dimensional.len() as f32).sqrt().recip();
    assert!(gains.iter().all(|gain| (*gain - expected).abs() <= 1.0e-6));
}

#[test]
fn ceiling_centre_uses_only_the_four_top_directions() {
    let layout = seven_one_four_layout();
    let panner = PanGeometry::new(&layout).unwrap();
    let mut gains = vec![0.0; layout.len()];
    panner.gains(Position::new(0.0, 0.0, 1.0), 0.0, &mut gains);

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
    let panner = PanGeometry::new(&layout).unwrap();
    let mut gains = vec![0.0; layout.len()];
    panner.gains(Position::new(0.0, 0.0, -1.0), 0.0, &mut gains);

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
    let panner = PanGeometry::new(&layout).unwrap();
    let mut gains = vec![0.0; layout.len()];

    for azimuth in (-180..180).step_by(3) {
        panner.gains(Position::from_angles(azimuth as f32, 0.0), 0.0, &mut gains);
        assert_unit_power(&gains);
        assert!(gains.iter().filter(|gain| **gain > 1.0e-6).count() <= 2);
    }
}

#[test]
fn near_horizontal_layout_projects_measured_elevations_to_azimuth() {
    let layout = layout(&[("L", 45.0, 5.0), ("R", -45.0, -5.0)]);
    let panner = PanGeometry::new(&layout).unwrap();
    let mut gains = vec![0.0; layout.len()];
    panner.gains(Position::from_angles(0.0, 40.0), 0.0, &mut gains);
    assert_unit_power(&gains);
    let expected = 2.0_f32.sqrt().recip();
    assert!((gains[0] - expected).abs() <= 1.0e-6);
    assert!((gains[1] - expected).abs() <= 1.0e-6);
}

#[test]
fn two_direction_arc_pans_between_endpoints() {
    let layout = layout(&[("FL", 30.0, 0.0), ("FR", -30.0, 0.0)]);
    let panner = PanGeometry::new(&layout).unwrap();
    let mut gains = vec![0.0; layout.len()];
    panner.gains(Position::from_angles(0.0, 0.0), 0.0, &mut gains);
    assert_unit_power(&gains);
    let expected = 2.0_f32.sqrt().recip();
    assert!((gains[0] - expected).abs() <= 1.0e-6);
    assert!((gains[1] - expected).abs() <= 1.0e-6);
}

#[test]
fn degenerate_layouts_are_rejected() {
    let coincident = layout(&[("A", 0.0, 0.0), ("B", 0.0, 0.0)]);
    assert!(matches!(
        PanGeometry::new(&coincident),
        Err(RenderError::DegenerateLayout(_))
    ));
    let equivalent = layout(&[("A", 0.0, 0.0), ("B", 360.0, 0.0)]);
    assert!(matches!(
        PanGeometry::new(&equivalent),
        Err(RenderError::DegenerateLayout(_))
    ));
}

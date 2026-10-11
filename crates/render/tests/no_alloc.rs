#![forbid(unsafe_code)]

use assert_no_alloc::{AllocDisabler, assert_no_alloc};
use bio_types::{Direction, Feeds, Layout, Object, Position, SceneBlock};
use render::{Panner, PannerConfig};

#[cfg(debug_assertions)]
#[global_allocator]
static ALLOCATOR: AllocDisabler = AllocDisabler;

fn twenty_two_direction_layout() -> Layout {
    Layout::new(
        [
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
        ]
        .into_iter()
        .map(|(name, azimuth, elevation)| Direction::new(name, azimuth, elevation))
        .collect(),
    )
    .unwrap()
}

#[test]
fn steady_state_render_of_thirty_one_objects_into_twenty_two_directions_does_not_allocate() {
    const FRAMES: usize = 480;
    const OBJECTS: usize = 31;

    let layout = twenty_two_direction_layout();
    let mut panner = Panner::new(&layout, PannerConfig::default()).unwrap();
    let signal = [0.25_f32; FRAMES];
    let objects = [Object {
        signal: &signal,
        position: Position::new(0.2, 0.8, 0.4),
        gain: 0.5,
        spread: 0.0,
    }; OBJECTS];
    let scene = SceneBlock {
        epoch: 0,
        frames: FRAMES,
        rate: 48_000,
        source_qpc: 0,
        objects: &objects,
        lfe: None,
    };
    let mut feeds = Feeds::new(layout.len(), FRAMES);

    panner.render(&scene, &mut feeds);
    assert_no_alloc(|| {
        for _ in 0..8 {
            panner.render(&scene, &mut feeds);
        }
        panner.reset();
        panner.render(&scene, &mut feeds);
    });
}

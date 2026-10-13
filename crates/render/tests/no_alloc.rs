#![forbid(unsafe_code)]

use assert_no_alloc::{AllocDisabler, assert_no_alloc};
use bio_types::{Direction, Layout, Position};
use render::PanGeometry;

#[global_allocator]
static ALLOCATOR: AllocDisabler = AllocDisabler;

fn twenty_two_direction_layout() -> Layout {
    let mut directions = Vec::new();
    for elevation in [-45.0, 0.0, 45.0] {
        for azimuth in (0..360).step_by(45) {
            if directions.len() == 22 {
                break;
            }
            directions.push(Direction::new(
                format!("d{}", directions.len()),
                azimuth as f32,
                elevation,
            ));
        }
    }
    Layout::new(directions).unwrap()
}

#[test]
fn pan_geometry_gains_do_not_allocate() {
    let geometry = PanGeometry::new(&twenty_two_direction_layout()).unwrap();
    let mut gains = [0.0_f32; Layout::MAX_DIRECTIONS];
    assert_no_alloc(|| {
        geometry.gains(
            Position::from_angles(27.0, 38.0),
            0.65,
            &mut gains[..geometry.directions()],
        );
    });
}

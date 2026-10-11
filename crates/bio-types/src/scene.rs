//! The scene: one block of positioned signals plus LFE.
//!
//! Every decoder's output and the renderer's only input. Bed channels arrive as
//! objects at fixed positions, so the renderer never sees a channel.

/// A point in the room, each axis normalised to `[-1, 1]`.
///
/// * `x`: left (-1) to right (+1)
/// * `y`: back (-1) to front (+1)
/// * `z`: below the listener (-1), ear level (0), ceiling (+1)
///
/// Decoders convert their format's own convention into this one. The renderer
/// treats the vector from the centre as a direction, so `(0, 1, 0)` and
/// `(0, 0.5, 0)` pan to the same place.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Position {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

impl Position {
    pub const FRONT: Self = Self::new(0.0, 1.0, 0.0);

    pub const fn new(x: f32, y: f32, z: f32) -> Self {
        Self { x, y, z }
    }

    /// The position on the unit sphere at the given angles (degrees).
    ///
    /// Azimuth 0 is straight ahead and grows towards the left (counter-clockwise
    /// seen from above, as in ITU-R BS.2051 and SOFA); elevation grows upwards.
    pub fn from_angles(azimuth_deg: f32, elevation_deg: f32) -> Self {
        let (az, el) = (azimuth_deg.to_radians(), elevation_deg.to_radians());
        Self {
            x: -az.sin() * el.cos(),
            y: az.cos() * el.cos(),
            z: el.sin(),
        }
    }

    /// The unit vector pointing from the listener towards this position, or
    /// `None` at the centre of the room, which has no direction.
    pub fn direction(self) -> Option<[f32; 3]> {
        let norm = (self.x * self.x + self.y * self.y + self.z * self.z).sqrt();
        (norm > f32::EPSILON).then(|| [self.x / norm, self.y / norm, self.z / norm])
    }
}

/// Most objects one [`SceneBlock`] carries: MAT 2.1 has 11 fixed channels and
/// 20 dynamic objects, TrueHD and DD+ JOC fewer. Decoders size their buffers
/// and renderers their gain caches from this.
pub const MAX_OBJECTS: usize = 64;

/// One positioned signal of a [`SceneBlock`].
#[derive(Clone, Copy, Debug)]
pub struct Object<'a> {
    /// `SceneBlock::frames` samples at the scene rate.
    pub signal: &'a [f32],
    pub position: Position,
    /// Linear gain the renderer applies on top of the panning gains.
    pub gain: f32,
    /// Size or spread in `[0, 1]`; 0 when the format does not say.
    pub spread: f32,
}

/// One block of objects and LFE.
///
/// Guarantees a decoder gives every reader:
/// * every signal (each object's and the LFE) is exactly `frames` long and at
///   `rate`; the output device's rate matters only after the crossing ring;
/// * the slices borrow buffers the decoder allocated when it started, so no
///   allocation happens between blocks;
/// * a PCM channel mask is converted to room positions too.
#[derive(Clone, Copy, Debug)]
pub struct SceneBlock<'a> {
    /// Bumps with every format change (see `bio_abi::Record::FormatChange`).
    pub epoch: u32,
    /// Block length chosen by the source: 480 for a MAT half burst, 1536 for DD+.
    pub frames: usize,
    pub rate: u32,
    /// QPC of the first sample's arrival at the virtual device, for latency.
    pub source_qpc: u64,
    pub objects: &'a [Object<'a>],
    pub lfe: Option<&'a [f32]>,
}

impl Object<'static> {
    /// A silent placeholder, for filling fixed-size object arrays on the stack.
    pub const SILENT: Self = Self {
        signal: &[],
        position: Position::FRONT,
        gain: 0.0,
        spread: 0.0,
    };
}

impl SceneBlock<'_> {
    /// True when every signal has `frames` samples; renderers assert this in
    /// debug builds.
    pub fn is_consistent(&self) -> bool {
        self.objects.iter().all(|o| o.signal.len() == self.frames)
            && self.lfe.is_none_or(|lfe| lfe.len() == self.frames)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: [f32; 3], b: [f32; 3]) -> bool {
        a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-6)
    }

    #[test]
    fn angles_follow_the_left_positive_convention() {
        let front = Position::from_angles(0.0, 0.0).direction().unwrap();
        assert!(close(front, [0.0, 1.0, 0.0]));
        let left = Position::from_angles(90.0, 0.0).direction().unwrap();
        assert!(close(left, [-1.0, 0.0, 0.0]));
        let up = Position::from_angles(0.0, 90.0).direction().unwrap();
        assert!(close(up, [0.0, 0.0, 1.0]));
    }

    #[test]
    fn the_centre_has_no_direction() {
        assert_eq!(Position::default().direction(), None);
        assert!(close(
            Position::new(0.0, 0.5, 0.0).direction().unwrap(),
            [0.0, 1.0, 0.0]
        ));
    }

    #[test]
    fn consistency_checks_every_signal_length() {
        let a = [0.0; 4];
        let short = [0.0; 3];
        let objects = [Object {
            signal: &a,
            position: Position::FRONT,
            gain: 1.0,
            spread: 0.0,
        }];
        let block = SceneBlock {
            epoch: 0,
            frames: 4,
            rate: 48_000,
            source_qpc: 0,
            objects: &objects,
            lfe: Some(&short),
        };
        assert!(!block.is_consistent());
        assert!(SceneBlock { lfe: None, ..block }.is_consistent());
    }
}

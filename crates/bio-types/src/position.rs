//! Position conventions shared by source adapters, not measured BRIR layouts.
//!
//! Room coordinates are not direction vectors. Conversion follows ITU-R
//! BS.2127-1 (2023), §10.1 (position conversion), independent of output layout.

use crate::Position;

/// Converts a room-cube point to a direction vector, retaining the BS.2127
/// distance as its length so [`direction_to_room`] can invert it. Inputs use
/// x=right, y=front, z=up, each in `[-1, 1]`. Finite overshoot is clamped;
/// nonfinite input and the room centre return the directionless zero vector.
/// The renderer ignores vector length, not the conversion's angular mapping.
pub fn room_to_direction(x: f32, y: f32, z: f32) -> Position {
    if ![x, y, z].into_iter().all(f32::is_finite) {
        return Position::default();
    }
    let (x, y, z) = (x.clamp(-1.0, 1.0), y.clamp(-1.0, 1.0), z.clamp(-1.0, 1.0));
    let r = x.abs().max(y.abs());
    if r < 1e-10 {
        return Position::new(0.0, 0.0, z);
    }
    // Mirror the five sectors of §10.1 about x=0. The back sector is
    // unwrapped from 110 to 250 degrees before applying the panning curve.
    let az = if y >= x.abs() {
        linear_to_az(0.0, 30.0, x.abs() / r)
    } else if y <= -x.abs() {
        linear_to_az(110.0, 250.0, (1.0 - x.abs() / r) * 0.5)
    } else {
        linear_to_az(30.0, 110.0, (1.0 - y / r) * 0.5)
    };
    let az = if x > 0.0 { -az } else { az };
    let cube_el = z.atan2(r).to_degrees();
    let el = if cube_el.abs() <= 45.0 {
        cube_el * (30.0 / 45.0)
    } else {
        cube_el.signum() * (30.0 + (cube_el.abs() - 45.0) * (60.0 / 45.0))
    };
    let d = r.max(z.abs());
    let p = Position::from_angles(az, el);
    Position::new(p.x * d, p.y * d, p.z * d)
}

/// Inverse of [`room_to_direction`] (§10.1.1). The vector length is the polar
/// distance, clamped to one. A unit direction maps to the cube surface; a
/// converted interior point retains its distance. Nonfinite input returns zero.
pub fn direction_to_room(direction: Position) -> Position {
    let Position { x, y, z } = direction;
    if ![x, y, z].into_iter().all(f32::is_finite) {
        return Position::default();
    }
    let horizontal = x.hypot(y);
    let d = horizontal.hypot(z).min(1.0);
    if horizontal < 1e-10 {
        return Position::new(0.0, 0.0, d * z.signum());
    }
    let az = (-x).atan2(y).to_degrees();
    let el = z.atan2(horizontal).to_degrees();
    let (r, height) = if el.abs() <= 30.0 {
        (d, d * (el * (45.0 / 30.0)).to_radians().tan())
    } else {
        let cube_el = 45.0 + (el.abs() - 30.0) * (45.0 / 60.0);
        (d * (90.0 - cube_el).to_radians().tan(), d * el.signum())
    };
    let (side, front) = if az.abs() <= 30.0 {
        (az_to_linear(0.0, 30.0, az.abs()), 1.0)
    } else if az.abs() <= 110.0 {
        (1.0, 1.0 - 2.0 * az_to_linear(30.0, 110.0, az.abs()))
    } else {
        (1.0 - 2.0 * az_to_linear(110.0, 250.0, az.abs()), -1.0)
    };
    Position::new(-az.signum() * side * r, front * r, height)
}

fn linear_to_az(left: f32, right: f32, p: f32) -> f32 {
    if p <= 0.0 {
        return left;
    }
    if p >= 1.0 {
        return right;
    }
    let mid = (left + right) * 0.5;
    let range = (right - left) * 0.5;
    let (gr, gl) = (p * std::f32::consts::FRAC_PI_2).sin_cos();
    let fraction = gr / (gl + gr);
    mid + ((2.0 * fraction - 1.0) * range.to_radians().tan())
        .atan()
        .to_degrees()
}

fn az_to_linear(left: f32, right: f32, az: f32) -> f32 {
    let mid = (left + right) * 0.5;
    let range = (right - left) * 0.5;
    let gr = 0.5 + (az - mid).to_radians().tan() / (2.0 * range.to_radians().tan());
    gr.atan2(1.0 - gr) / std::f32::consts::FRAC_PI_2
}

/// Named nominal speakers. `Ls/Rs` are DD+ surrounds (110 degrees), while
/// `Lss/Rss` are Windows side speakers (90 degrees); these are not aliases.
/// Measured BRIR angles deliberately do not use this table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Speaker {
    L,
    R,
    C,
    Ls,
    Rs,
    Lrs,
    Rrs,
    Cs,
    Lw,
    Rw,
    Lc,
    Rc,
    Lss,
    Rss,
    Tfl,
    Tfr,
    Tsl,
    Tsr,
    Trl,
    Trr,
    Tc,
    Tfc,
    Trc,
    Lfe,
    Lfe2,
}

impl Speaker {
    /// Nominal azimuth (positive left) and elevation (positive up), degrees.
    /// Both LFE labels are unpositioned.
    pub const fn angles(self) -> Option<(f32, f32)> {
        use Speaker::*;
        Some(match self {
            L => (30.0, 0.0),
            R => (-30.0, 0.0),
            C => (0.0, 0.0),
            Ls => (110.0, 0.0),
            Rs => (-110.0, 0.0),
            Lrs => (150.0, 0.0),
            Rrs => (-150.0, 0.0),
            Cs => (180.0, 0.0),
            Lw => (60.0, 0.0),
            Rw => (-60.0, 0.0),
            Lc => (15.0, 0.0),
            Rc => (-15.0, 0.0),
            Lss => (90.0, 0.0),
            Rss => (-90.0, 0.0),
            Tfl => (45.0, 45.0),
            Tfr => (-45.0, 45.0),
            Tsl => (90.0, 45.0),
            Tsr => (-90.0, 45.0),
            Trl => (135.0, 45.0),
            Trr => (-135.0, 45.0),
            Tc => (0.0, 90.0),
            Tfc => (0.0, 45.0),
            Trc => (180.0, 45.0),
            Lfe | Lfe2 => return None,
        })
    }

    pub fn position(self) -> Option<Position> {
        self.angles().map(|(az, el)| Position::from_angles(az, el))
    }

    /// The speaker of one WAVEFORMATEXTENSIBLE `dwChannelMask` bit
    /// (`SPEAKER_FRONT_LEFT` = 0x1 ...), or `None` for a bit Windows does not
    /// name. `SPEAKER_BACK_*` are `Lrs/Rrs` and `SPEAKER_SIDE_*` are `Lss/Rss`.
    pub const fn from_wave_bit(bit: u32) -> Option<Self> {
        use Speaker::*;
        Some(match bit {
            0x1 => L,
            0x2 => R,
            0x4 => C,
            0x8 => Lfe,
            0x10 => Lrs,
            0x20 => Rrs,
            0x40 => Lc,
            0x80 => Rc,
            0x100 => Cs,
            0x200 => Lss,
            0x400 => Rss,
            0x800 => Tc,
            0x1000 => Tfl,
            0x2000 => Tfc,
            0x4000 => Tfr,
            0x8000 => Trl,
            0x10000 => Trc,
            0x20000 => Trr,
            _ => return None,
        })
    }

    /// The `dwChannelMask` bit of this speaker, or `None` when Windows has no
    /// bit for it (`Ls/Rs`, the wides, the top sides and `Lfe2`).
    pub const fn wave_bit(self) -> Option<u32> {
        use Speaker::*;
        Some(match self {
            L => 0x1,
            R => 0x2,
            C => 0x4,
            Lfe => 0x8,
            Lrs => 0x10,
            Rrs => 0x20,
            Lc => 0x40,
            Rc => 0x80,
            Cs => 0x100,
            Lss => 0x200,
            Rss => 0x400,
            Tc => 0x800,
            Tfl => 0x1000,
            Tfc => 0x2000,
            Tfr => 0x4000,
            Trl => 0x8000,
            Trc => 0x10000,
            Trr => 0x20000,
            Ls | Rs | Lw | Rw | Tsl | Tsr | Lfe2 => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: Position, b: Position) {
        assert!(
            (a.x - b.x)
                .abs()
                .max((a.y - b.y).abs())
                .max((a.z - b.z).abs())
                < 2e-6,
            "{a:?} != {b:?}"
        );
    }

    #[test]
    fn wave_bits_and_speakers_round_trip() {
        let mut named = 0;
        for shift in 0..32 {
            let bit = 1_u32 << shift;
            if let Some(speaker) = Speaker::from_wave_bit(bit) {
                assert_eq!(speaker.wave_bit(), Some(bit), "{speaker:?}");
                named += 1;
            }
        }
        assert_eq!(named, 18);
        assert_eq!(Speaker::from_wave_bit(0x10), Some(Speaker::Lrs));
        assert_eq!(Speaker::from_wave_bit(0x200), Some(Speaker::Lss));
        for speaker in [Speaker::Ls, Speaker::Rs, Speaker::Lfe2] {
            assert_eq!(speaker.wave_bit(), None);
        }
    }

    #[test]
    fn room_corners_follow_bs2127_reference_layout() {
        for (x, y, az) in [
            (-1.0, 1.0, 30.0),
            (1.0, 1.0, -30.0),
            (-1.0, -1.0, 110.0),
            (1.0, -1.0, -110.0),
            (0.0, 1.0, 0.0),
            (0.0, -1.0, 180.0),
        ] {
            close(room_to_direction(x, y, 0.0), Position::from_angles(az, 0.0));
            // §10.1 maps the cube upper edge to 30°, not a geometric 45°.
            close(
                room_to_direction(x, y, 1.0),
                Position::from_angles(az, 30.0),
            );
            close(
                room_to_direction(x, y, -1.0),
                Position::from_angles(az, -30.0),
            );
        }
        close(
            room_to_direction(0.0, 0.0, 1.0),
            Position::new(0.0, 0.0, 1.0),
        );
        close(room_to_direction(0.0, 0.0, 0.0), Position::default());
        // §10.1's example: the midpoint between 0 and -30 maps to x=0.5.
        close(
            direction_to_room(Position::from_angles(-15.0, 0.0)),
            Position::new(0.5, 1.0, 0.0),
        );
    }

    #[test]
    fn room_conversion_is_symmetric_and_invertible_on_dense_grid() {
        for ix in -20..=20 {
            for iy in -20..=20 {
                for iz in -20..=20 {
                    let (x, y, z) = (ix as f32 / 20.0, iy as f32 / 20.0, iz as f32 / 20.0);
                    let p = room_to_direction(x, y, z);
                    close(direction_to_room(p), Position::new(x, y, z));
                    close(room_to_direction(-x, y, z), Position::new(-p.x, p.y, p.z));
                }
            }
        }
        for az in -180..=180 {
            for el in -90..=90 {
                let p = Position::from_angles(az as f32, el as f32);
                let room = direction_to_room(p);
                close(room_to_direction(room.x, room.y, room.z), p);
            }
        }
    }

    #[test]
    fn room_conversion_is_continuous_away_from_directionless_centre() {
        // Cube surfaces include every azimuth/elevation sector boundary.
        // A direction has no continuous extension at the origin, so don't
        // assert a global angular bound across that singular point.
        for axis in 0..3 {
            for sign in [-1.0, 1.0] {
                for i in -100..100 {
                    for j in -100..=100 {
                        let mut a = [0.0; 3];
                        a[axis] = sign;
                        a[(axis + 1) % 3] = i as f32 / 100.0;
                        a[(axis + 2) % 3] = j as f32 / 100.0;
                        let mut b = a;
                        b[(axis + 1) % 3] += 0.01;
                        let p = room_to_direction(a[0], a[1], a[2]).direction().unwrap();
                        let q = room_to_direction(b[0], b[1], b[2]).direction().unwrap();
                        let dot = p.iter().zip(q).map(|(a, b)| a * b).sum::<f32>();
                        assert!(dot.clamp(-1.0, 1.0).acos().to_degrees() < 2.0);
                    }
                }
            }
        }
    }

    #[test]
    fn room_conversion_handles_nonfinite_and_clamped_inputs() {
        assert_eq!(room_to_direction(f32::NAN, 0.0, 0.0), Position::default());
        assert_eq!(
            direction_to_room(Position::new(f32::INFINITY, 0.0, 0.0)),
            Position::default()
        );
        close(
            room_to_direction(-2.0, 2.0, 0.0),
            room_to_direction(-1.0, 1.0, 0.0),
        );
    }
}

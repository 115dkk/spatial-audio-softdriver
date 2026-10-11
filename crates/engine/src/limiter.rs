//! Output gain and peak limiter, the chain's last stage.

/// Stateful linked-stereo feed-forward peak limiter.
///
/// The detector sees the larger absolute sample from both ears. Its gain
/// reduction attacks with a 1 ms time constant and releases with a 50 ms time
/// constant. There is no look-ahead; a final hard clamp guarantees the ceiling
/// while the detector catches a transient.
#[derive(Debug)]
pub(crate) struct Limiter {
    gain: f32,
    ceiling: f32,
    detector_gain: f32,
    attack: f32,
    release: f32,
}

impl Limiter {
    pub(crate) fn new() -> Self {
        Self {
            gain: 1.0,
            ceiling: 1.0,
            detector_gain: 1.0,
            attack: 0.0,
            release: 0.0,
        }
    }

    pub(crate) fn reset(&mut self, rate: u32, gain: f32, ceiling: f32) {
        self.gain = if gain.is_finite() { gain } else { 0.0 };
        self.ceiling = if ceiling.is_finite() {
            ceiling.max(0.0)
        } else {
            0.0
        };
        self.detector_gain = 1.0;
        self.attack = coefficient(rate, 0.001);
        self.release = coefficient(rate, 0.050);
    }

    pub(crate) fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        debug_assert_eq!(left.len(), right.len());
        for (left, right) in left.iter_mut().zip(right) {
            let gained_left = finite_or_zero(*left * self.gain);
            let gained_right = finite_or_zero(*right * self.gain);
            let peak = gained_left.abs().max(gained_right.abs());
            let target = if peak > self.ceiling && peak > 0.0 {
                self.ceiling / peak
            } else {
                1.0
            };
            let coefficient = if target < self.detector_gain {
                self.attack
            } else {
                self.release
            };
            self.detector_gain = coefficient * self.detector_gain + (1.0 - coefficient) * target;
            *left = (gained_left * self.detector_gain).clamp(-self.ceiling, self.ceiling);
            *right = (gained_right * self.detector_gain).clamp(-self.ceiling, self.ceiling);
        }
    }
}

fn coefficient(rate: u32, seconds: f32) -> f32 {
    if rate == 0 {
        0.0
    } else {
        (-1.0 / (seconds * rate as f32)).exp()
    }
}

fn finite_or_zero(value: f32) -> f32 {
    if value.is_nan() { 0.0 } else { value }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    proptest! {
        #[test]
        fn limiter_never_exceeds_ceiling(
            left in proptest::collection::vec(any::<f32>(), 0..4096),
            right in proptest::collection::vec(any::<f32>(), 0..4096),
            ceiling in 0.001_f32..1.0,
            gain in 0.0_f32..100.0,
        ) {
            let frames = left.len().min(right.len());
            let mut left = left[..frames].to_vec();
            let mut right = right[..frames].to_vec();
            let mut limiter = Limiter::new();
            limiter.reset(48_000, gain, ceiling);
            limiter.process(&mut left, &mut right);
            prop_assert!(left.iter().chain(&right).all(|sample| sample.is_finite() && sample.abs() <= ceiling));
        }
    }

    #[test]
    fn limiter_is_transparent_below_ceiling() {
        let mut left = vec![0.25; 4_800];
        let mut right = vec![-0.5; 4_800];
        let expected_left = left.clone();
        let expected_right = right.clone();
        let mut limiter = Limiter::new();
        limiter.reset(48_000, 1.0, 0.9);
        limiter.process(&mut left, &mut right);
        assert!(
            left.iter()
                .zip(expected_left)
                .all(|(a, b)| (*a - b).abs() <= 1e-6)
        );
        assert!(
            right
                .iter()
                .zip(expected_right)
                .all(|(a, b)| (*a - b).abs() <= 1e-6)
        );
    }

    #[test]
    fn limiter_applies_output_gain() {
        let mut left = [0.1, -0.2];
        let mut right = [0.3, -0.4];
        let mut limiter = Limiter::new();
        limiter.reset(48_000, 2.0, 1.0);
        limiter.process(&mut left, &mut right);
        assert_eq!(left, [0.2, -0.4]);
        assert_eq!(right, [0.6, -0.8]);
    }
}

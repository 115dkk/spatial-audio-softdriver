//! Output gain and peak limiter, the chain's last stage.

/// Stateful linked-channel feed-forward peak limiter.
///
/// At each sample time the detector sees the largest absolute sample over every
/// channel and applies one gain to all of them. Its gain reduction attacks with
/// a 1 ms time constant and releases with a 50 ms time constant. There is no
/// look-ahead: while the detector catches a transient, the frame's gain is
/// lowered further so its peak sits at the ceiling, still one gain for every
/// channel, so the image does not shift (ADR 0030).
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

    pub(crate) fn process(&mut self, samples: &mut [f32], channels: usize, frames: usize) {
        debug_assert_eq!(samples.len(), channels * frames);
        for frame in 0..frames {
            let mut peak = 0.0_f32;
            for channel in 0..channels {
                let index = channel * frames + frame;
                let gained = finite_or_zero(samples[index] * self.gain);
                samples[index] = gained;
                peak = peak.max(gained.abs());
            }
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
            let previous_gain = self.detector_gain;
            self.detector_gain = coefficient * previous_gain + (1.0 - coefficient) * target;
            if target == 1.0 && self.detector_gain == previous_gain {
                self.detector_gain = 1.0;
            }
            let applied = if peak * self.detector_gain > self.ceiling {
                self.ceiling / peak
            } else {
                self.detector_gain
            };
            if applied != 1.0 {
                for channel in 0..channels {
                    let index = channel * frames + frame;
                    // The clamp only removes rounding above the ceiling.
                    samples[index] = (samples[index] * applied).clamp(-self.ceiling, self.ceiling);
                }
            }
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
    if value.is_finite() { value } else { 0.0 }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn stereo_planar(left: &[f32], right: &[f32]) -> Vec<f32> {
        left.iter().chain(right).copied().collect()
    }

    proptest! {
        #[test]
        fn limiter_never_exceeds_ceiling(
            left in proptest::collection::vec(any::<f32>(), 0..4096),
            right in proptest::collection::vec(any::<f32>(), 0..4096),
            ceiling in 0.001_f32..1.0,
            gain in 0.0_f32..100.0,
        ) {
            let frames = left.len().min(right.len());
            let mut samples = stereo_planar(&left[..frames], &right[..frames]);
            let mut limiter = Limiter::new();
            limiter.reset(48_000, gain, ceiling);
            limiter.process(&mut samples, 2, frames);
            prop_assert!(samples.iter().all(|sample| sample.is_finite() && sample.abs() <= ceiling));
        }
    }

    #[test]
    fn limiter_is_transparent_below_ceiling() {
        let left = vec![0.25; 4_800];
        let right = vec![-0.5; 4_800];
        let mut samples = stereo_planar(&left, &right);
        let expected = samples.clone();
        let mut limiter = Limiter::new();
        limiter.reset(48_000, 1.0, 0.9);
        limiter.process(&mut samples, 2, 4_800);
        assert_eq!(samples, expected);
    }

    #[test]
    fn limiter_is_bit_exact_below_ceiling_across_block_boundaries() {
        let mut state = 0x9e37_79b9_u32;
        let samples: Vec<f32> = (0..10_003)
            .map(|_| {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                ((state >> 8) as f32 / (1_u32 << 24) as f32 - 0.5) * 0.5
            })
            .collect();
        for block in [1, 7, 480, 1_023] {
            let mut limiter = Limiter::new();
            limiter.reset(48_000, 1.0, 0.9);
            let mut output = Vec::new();
            for chunk in samples.chunks(block) {
                let right: Vec<f32> = chunk.iter().map(|sample| -*sample).collect();
                let mut planar = stereo_planar(chunk, &right);
                limiter.process(&mut planar, 2, chunk.len());
                output.extend_from_slice(&planar[..chunk.len()]);
            }
            assert_eq!(output, samples, "block {block} left");
        }
    }

    #[test]
    fn limiter_is_bit_exact_after_release_recovers() {
        let mut limiter = Limiter::new();
        limiter.reset(48_000, 1.0, 0.9);
        let mut transient = stereo_planar(&[2.0; 16], &[-2.0; 16]);
        limiter.process(&mut transient, 2, 16);
        let mut silence = vec![0.0; 160_000];
        limiter.process(&mut silence, 2, 80_000);
        assert_eq!(limiter.detector_gain, 1.0);

        let left = [0.125, -0.25, 0.375];
        let right = [-0.375, 0.25, -0.125];
        let mut samples = stereo_planar(&left, &right);
        let expected = samples.clone();
        limiter.process(&mut samples, 2, 3);
        assert_eq!(samples, expected);
    }

    #[test]
    fn limiter_applies_output_gain() {
        let mut samples = stereo_planar(&[0.1, -0.2], &[0.3, -0.4]);
        let mut limiter = Limiter::new();
        limiter.reset(48_000, 2.0, 1.0);
        limiter.process(&mut samples, 2, 2);
        assert_eq!(samples, [0.2, -0.4, 0.6, -0.8]);
    }

    #[test]
    fn one_overloaded_channel_drives_the_shared_detector() {
        let mut samples = [1.0, 0.25, -0.5];
        let original = samples;
        let mut limiter = Limiter::new();
        limiter.reset(48_000, 1.0, 0.9);
        limiter.process(&mut samples, 3, 1);

        // Every channel, the overloaded one included, gets the same gain.
        let gain = samples[0] / original[0];
        assert!((gain - 0.9).abs() <= 1e-6, "{gain}");
        for (sample, original) in samples.iter().zip(original) {
            assert!((sample / original - gain).abs() <= 1e-6, "{samples:?}");
        }
        assert!(samples.iter().all(|sample| sample.abs() <= 0.9));
    }

    #[test]
    fn non_finite_samples_become_silence_not_nan() {
        let mut samples = [f32::INFINITY, f32::NAN, 0.5, f32::NEG_INFINITY];
        let mut limiter = Limiter::new();
        limiter.reset(48_000, 1.0, 0.9);
        limiter.process(&mut samples, 2, 2);
        assert!(samples.iter().all(|sample| sample.is_finite()));
        assert_eq!(samples[0], 0.0);
        assert_eq!(samples[3], 0.0);
    }
}

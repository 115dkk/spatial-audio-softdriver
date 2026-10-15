//! The direction test signal (`LiveChange::TestTone`).
//!
//! 400 ms of band-limited noise (about 200 Hz to 8 kHz), generated once when
//! the chain is built and replayed from the start on every trigger. The noise
//! comes from a fixed-seed generator, so every run and every machine plays the
//! same samples. Its peak is scaled to just under -20 dBFS and both ends fade
//! with a 10 ms raised cosine, so the signal starts and stops without a click.

use crate::mixer::MIX_RATE;

/// Length of the test signal in 48 kHz frames (400 ms).
pub const TEST_TONE_FRAMES: usize = MIX_RATE as usize * 2 / 5;
/// Length of each raised-cosine fade in 48 kHz frames (10 ms).
pub const TEST_TONE_FADE_FRAMES: usize = MIX_RATE as usize / 100;
/// Largest absolute sample of the test signal: just under -20 dBFS.
pub const TEST_TONE_PEAK: f32 = 0.099_9;

const LOW_HZ: f64 = 200.0;
const HIGH_HZ: f64 = 8_000.0;
const SEED: u32 = 0x5353_5221;
/// Frames the filters run before the kept signal, so their start-up transient
/// is not part of it.
const SETTLE_FRAMES: usize = MIX_RATE as usize / 10;

/// The raised-cosine fade gain `frame` frames into a fade (0 at the edge).
pub fn test_tone_fade(frame: usize) -> f32 {
    if frame >= TEST_TONE_FADE_FRAMES {
        return 1.0;
    }
    let phase = std::f64::consts::PI * frame as f64 / TEST_TONE_FADE_FRAMES as f64;
    (0.5 - 0.5 * phase.cos()) as f32
}

/// Builds the test signal. Allocates; call off the real-time threads.
pub fn test_tone_samples() -> Vec<f32> {
    let mut random = SEED;
    let mut stages = [
        Biquad::high_pass(LOW_HZ),
        Biquad::high_pass(LOW_HZ),
        Biquad::low_pass(HIGH_HZ),
        Biquad::low_pass(HIGH_HZ),
    ];
    let mut filtered = Vec::with_capacity(TEST_TONE_FRAMES);
    for frame in 0..SETTLE_FRAMES + TEST_TONE_FRAMES {
        // xorshift32: deterministic, uniform in [-1, 1).
        random ^= random << 13;
        random ^= random >> 17;
        random ^= random << 5;
        let mut sample = f64::from(random) / f64::from(u32::MAX) * 2.0 - 1.0;
        for stage in &mut stages {
            sample = stage.process(sample);
        }
        if frame >= SETTLE_FRAMES {
            filtered.push(sample);
        }
    }
    let peak = filtered
        .iter()
        .fold(0.0_f64, |peak, sample| peak.max(sample.abs()));
    let scale = if peak > 0.0 {
        f64::from(TEST_TONE_PEAK) / peak
    } else {
        0.0
    };
    let mut samples: Vec<f32> = filtered
        .into_iter()
        .map(|sample| ((sample * scale) as f32).clamp(-TEST_TONE_PEAK, TEST_TONE_PEAK))
        .collect();
    for frame in 0..TEST_TONE_FADE_FRAMES {
        let gain = test_tone_fade(frame);
        samples[frame] *= gain;
        samples[TEST_TONE_FRAMES - 1 - frame] *= gain;
    }
    samples
}

/// RBJ cookbook Butterworth section (Q = 1/sqrt 2), direct form I in f64.
#[derive(Clone, Copy)]
struct Biquad {
    b: [f64; 3],
    a: [f64; 2],
    x: [f64; 2],
    y: [f64; 2],
}

impl Biquad {
    fn high_pass(frequency: f64) -> Self {
        let (cos, alpha) = Self::angles(frequency);
        Self::normalised(
            [(1.0 + cos) / 2.0, -(1.0 + cos), (1.0 + cos) / 2.0],
            [1.0 + alpha, -2.0 * cos, 1.0 - alpha],
        )
    }

    fn low_pass(frequency: f64) -> Self {
        let (cos, alpha) = Self::angles(frequency);
        Self::normalised(
            [(1.0 - cos) / 2.0, 1.0 - cos, (1.0 - cos) / 2.0],
            [1.0 + alpha, -2.0 * cos, 1.0 - alpha],
        )
    }

    fn angles(frequency: f64) -> (f64, f64) {
        let omega = 2.0 * std::f64::consts::PI * frequency / f64::from(MIX_RATE);
        (omega.cos(), omega.sin() / std::f64::consts::SQRT_2)
    }

    fn normalised(b: [f64; 3], a: [f64; 3]) -> Self {
        Self {
            b: [b[0] / a[0], b[1] / a[0], b[2] / a[0]],
            a: [a[1] / a[0], a[2] / a[0]],
            x: [0.0; 2],
            y: [0.0; 2],
        }
    }

    fn process(&mut self, input: f64) -> f64 {
        let output = self.b[0] * input + self.b[1] * self.x[0] + self.b[2] * self.x[1]
            - self.a[0] * self.y[0]
            - self.a[1] * self.y[1];
        self.x = [input, self.x[0]];
        self.y = [output, self.y[0]];
        output
    }
}

/// Playback state the chain's back end owns. Starting it rewinds; it never
/// allocates after construction.
pub(crate) struct TestTonePlayer {
    samples: Box<[f32]>,
    direction: usize,
    position: usize,
}

impl std::fmt::Debug for TestTonePlayer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TestTonePlayer")
            .field("direction", &self.direction)
            .field("position", &self.position)
            .finish_non_exhaustive()
    }
}

impl TestTonePlayer {
    pub(crate) fn new() -> Self {
        let samples = test_tone_samples().into_boxed_slice();
        let position = samples.len();
        Self {
            samples,
            direction: 0,
            position,
        }
    }

    /// Plays from the beginning on `direction`, cutting off any tone playing.
    pub(crate) fn start(&mut self, direction: usize) {
        self.direction = direction;
        self.position = 0;
    }

    pub(crate) fn stop(&mut self) {
        self.position = self.samples.len();
    }

    /// Adds the next `frames` of the tone to `feed` when `feed` belongs to
    /// the tone's direction. Returns whether any tone was added.
    pub(crate) fn mix_into(&mut self, feeds: &mut ssr_types::Feeds, frames: usize) -> bool {
        if self.position >= self.samples.len() {
            return false;
        }
        if self.direction >= feeds.directions() {
            self.stop();
            return false;
        }
        let count = frames.min(self.samples.len() - self.position);
        let source = &self.samples[self.position..self.position + count];
        for (output, sample) in feeds.channel_mut(self.direction)[..count]
            .iter_mut()
            .zip(source)
        {
            *output += *sample;
        }
        self.position += count;
        true
    }
}

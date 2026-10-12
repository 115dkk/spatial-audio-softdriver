"""Unit tests for loopback_check.compare(); they need numpy, scipy and
soundfile but no audio device, so the Linux lint job runs them."""

import unittest

import numpy as np
from scipy.signal import resample_poly

from loopback_check import compare

RATE = 48_000


def tones(seconds, seed):
    rng = np.random.default_rng(seed)
    t = np.arange(int(seconds * RATE)) / RATE
    left = 0.3 * np.sin(2 * np.pi * 440 * t) + 0.05 * rng.standard_normal(len(t))
    right = 0.3 * np.sin(2 * np.pi * 1250 * t) + 0.05 * rng.standard_normal(len(t))
    return np.stack([left, right], axis=1).astype(np.float32)


def through_the_cable(reference, delay_s, ppm, noise, seed):
    """What the live path does to the signal: latency, a clock offset and noise."""
    rng = np.random.default_rng(seed)
    up, down = 1_000_000 + ppm, 1_000_000
    drifted = resample_poly(reference, up, down, axis=0)
    lead = np.zeros((int(delay_s * RATE), 2))
    tail = np.zeros((RATE // 2, 2))
    captured = np.concatenate([lead, drifted, tail])
    captured += noise * rng.standard_normal(captured.shape)
    return captured.astype(np.float32)


class CompareTest(unittest.TestCase):
    def test_a_delayed_drifting_copy_matches(self):
        reference = tones(2.0, 1)
        captured = through_the_cable(reference, 0.37, 150, 0.002, 2)
        summary = compare(reference, RATE, captured, RATE, 0.95)
        self.assertGreater(summary["windows"], 30)
        self.assertGreaterEqual(summary["matched"], 0.95, summary)

    def test_a_different_signal_does_not_match(self):
        reference = tones(2.0, 1)
        captured = through_the_cable(tones(2.0, 9)[:, ::-1], 0.37, 0, 0.002, 2)
        summary = compare(reference, RATE, captured, RATE, 0.95)
        self.assertLess(summary["matched"], 0.5, summary)

    def test_silence_does_not_match(self):
        reference = tones(1.0, 1)
        captured = np.zeros((2 * RATE, 2), np.float32)
        summary = compare(reference, RATE, captured, RATE, 0.95)
        self.assertEqual(summary["windows"], 0)

    def test_a_reference_at_another_rate_is_resampled(self):
        reference = tones(2.0, 1)
        captured = through_the_cable(reference, 0.2, 0, 0.002, 3)
        at_44k = resample_poly(reference, 441, 480, axis=0).astype(np.float32)
        summary = compare(at_44k, 44_100, captured, RATE, 0.95)
        self.assertGreaterEqual(summary["matched"], 0.95, summary)

    def test_dropouts_lower_the_matched_share(self):
        reference = tones(2.0, 1)
        captured = through_the_cable(reference, 0.3, 0, 0.002, 4)
        start = int(0.3 * RATE)
        for k in range(10):
            at = start + k * RATE // 5
            captured[at:at + 960] = 0.0
        summary = compare(reference, RATE, captured, RATE, 0.95)
        self.assertLess(summary["matched"], 1.0, summary)
        self.assertGreater(summary["matched"], 0.6, summary)


if __name__ == "__main__":
    unittest.main()

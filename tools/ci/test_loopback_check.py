"""Unit tests for loopback_check.compare(); they need numpy, scipy and
soundfile but no audio device, so the Linux lint job runs them."""

import unittest

import numpy as np
from scipy.signal import resample_poly

from loopback_check import binary_fromstring, compare

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

    def test_one_jump_costs_the_windows_around_it_only(self):
        # 20 ms of the recording lost in the middle: every later window is
        # 960 samples early, beyond the running search, and found again.
        reference = tones(2.0, 1)
        captured = through_the_cable(reference, 0.3, 0, 0.002, 5)
        cut = int(1.3 * RATE)
        captured = np.concatenate([captured[:cut], captured[cut + 960:]])
        summary = compare(reference, RATE, captured, RATE, 0.95)
        self.assertGreaterEqual(summary["resyncs"], 1, summary)
        self.assertGreaterEqual(summary["matched"], 0.9, summary)
        self.assertLessEqual(len(summary["missed"]), 2, summary)

    def test_a_nearly_periodic_signal_aligns_under_clock_drift(self):
        # Tones whose whole-signal correlation smears out once the copy runs
        # 600 ppm off: the plain argmax over every lag fell inside the leading
        # silence. The onset guess and a short anchor keep it at the copy.
        t = np.arange(2 * RATE) / RATE
        left = 0.2 * np.sin(2 * np.pi * 440 * t) + 0.1 * np.sin(2 * np.pi * 1250 * t)
        right = 0.2 * np.sin(2 * np.pi * 660 * t) + 0.1 * np.sin(2 * np.pi * 3300 * t)
        reference = np.stack([left, right], axis=1).astype(np.float32)
        for ppm in (-600, 600):
            captured = through_the_cable(reference, 0.65, ppm, 0.002, 8).astype(np.float32)
            summary = compare(reference, RATE, captured, RATE, 0.95)
            self.assertAlmostEqual(summary["offset_s"], 0.65, delta=0.002, msg=summary)
            self.assertGreaterEqual(summary["matched"], 0.95, summary)


class BinaryFromstring(unittest.TestCase):
    def test_reads_a_capture_buffer_as_a_writable_copy(self):
        samples = np.array([0.25, -0.5, 1.0], dtype=np.float32)
        buffer = bytearray(samples.tobytes())
        read = binary_fromstring(memoryview(buffer), dtype="float32")
        buffer[:] = bytes(len(buffer))
        np.testing.assert_array_equal(read, samples)
        read[0] = 2.0

    def test_text_mode_is_numpy_s_own(self):
        np.testing.assert_array_equal(binary_fromstring("1 2", dtype=int, sep=" "), [1, 2])


if __name__ == "__main__":
    unittest.main()

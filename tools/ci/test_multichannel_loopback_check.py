"""Device-free tests for multichannel_loopback_check."""

import struct
import tempfile
import unittest
from pathlib import Path

import numpy as np

from multichannel_loopback_check import (
    LAYOUTS,
    RATE,
    assignment_matrix,
    assignment_problems,
    choose_channels,
    make_dump,
    profile_text,
    steady_span,
    tone_frames,
)


def recording(channels, ppm=0.0, start_s=0.59, seconds=4.0, total_s=6.0, leak=None):
    """A 16-bit capture of the tones, pitch-shifted as the live ASRC would."""
    out = np.zeros((int(total_s * RATE), channels))
    t = np.arange(int(seconds * RATE)) / RATE
    frequencies = np.array([300.0 * 1.25 ** k for k in range(channels)]) * (1 + ppm * 1e-6)
    start = int(start_s * RATE)
    out[start:start + len(t)] = 0.1 * np.sin(2 * np.pi * t[:, None] * frequencies[None, :])
    if leak is not None:
        source, target, db = leak
        out[:, target] += out[:, source] * 10 ** (db / 20)
    return (np.round(out * 32767) / 32768).astype(np.float32)


def probe(*supported):
    return {
        "layouts": [
            {"channels": channels, "channel_mask": LAYOUTS.get(channels, (0,))[0],
             "exclusive": channels in supported}
            for channels in (2, 4, 6, 8, 12)
        ]
    }


class ChannelSelectionTest(unittest.TestCase):
    def test_selects_largest_mutual_layout(self):
        self.assertEqual(choose_channels(probe(4, 6, 8), probe(4, 6)), 6)

    def test_forced_layout_must_be_mutual_and_supported_by_the_tool(self):
        self.assertEqual(choose_channels(probe(4, 6), probe(4, 6), 4), 4)
        self.assertIsNone(choose_channels(probe(4, 6), probe(4), 6))
        self.assertIsNone(choose_channels(probe(12), probe(12), 12))

    def test_returns_none_below_four_channels(self):
        self.assertIsNone(choose_channels(probe(2), probe(2)))


class ProfileTest(unittest.TestCase):
    def test_profile_maps_windows_51_order_without_processing(self):
        text = profile_text(6)
        self.assertIn('name = "CI channel assignment"', text)
        self.assertIn("channel_mask = 63", text)
        self.assertEqual(text.count("[[speaker]]"), 5)
        self.assertEqual(text.count("[[subwoofer]]"), 1)
        self.assertIn('name = "Lrs"\nchannel = 4', text)
        self.assertIn('name = "Rrs"\nchannel = 5', text)
        self.assertNotIn("fir =", text)


class DumpTest(unittest.TestCase):
    def test_dump_records_round_trip_contract_fields(self):
        frames = tone_frames(4, 0.02)
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "tones.biodump"
            make_dump(path, frames, LAYOUTS[4][0])
            data = path.read_bytes()
        magic, abi, header_size, qpc_hz, version, reserved = struct.unpack_from(
            "<8sIIQII", data
        )
        self.assertEqual((magic, abi, header_size, qpc_hz, version, reserved),
                         (b"BIODUMP\0", 1, 32, 10_000_000, 1, 0))
        records = []
        at = header_size
        while at < len(data):
            header = struct.unpack_from("<HHIQQQIHH", data, at)
            kind, _, epoch, byte_pos, qpc, _, length, endpoint, _ = header
            payload = data[at + 40:at + 40 + length]
            records.append((kind, epoch, byte_pos, qpc, endpoint, payload))
            at += 40 + (length + 7) // 8 * 8
        self.assertEqual(records[0][0], 3)
        self.assertEqual(records[1][0], 2)
        wave = struct.unpack("<HHIIHHHHI16s", records[1][-1])
        self.assertEqual((wave[0], wave[1], wave[2], wave[4], wave[5], wave[6], wave[7], wave[8]),
                         (0xFFFE, 4, RATE, 16, 32, 22, 32, 0x33))
        pcm = b"".join(record[-1] for record in records if record[0] == 1)
        decoded = np.frombuffer(pcm, dtype="<f4").reshape(-1, 4)
        np.testing.assert_array_equal(decoded, frames)
        self.assertEqual(records[-1][0], 4)
        self.assertEqual(records[-1][2], frames.nbytes)


class AssignmentTest(unittest.TestCase):
    def setUp(self):
        self.frames = tone_frames(6, 1.0)
        self.frequencies = [300.0 * 1.25 ** k for k in range(6)]

    def test_correct_assignment_passes(self):
        matrix = assignment_matrix(self.frames, RATE, self.frequencies)
        self.assertEqual(assignment_problems(matrix), [])

    def test_swapped_channels_fail(self):
        swapped = self.frames.copy()
        swapped[:, [1, 4]] = swapped[:, [4, 1]]
        problems = assignment_problems(
            assignment_matrix(swapped, RATE, self.frequencies)
        )
        self.assertTrue(any("channel 1" in problem for problem in problems), problems)
        self.assertTrue(any("channel 4" in problem for problem in problems), problems)

    def test_empty_channel_fails(self):
        empty = self.frames.copy()
        empty[:, 3] = 0.0
        problems = assignment_problems(
            assignment_matrix(empty, RATE, self.frequencies)
        )
        self.assertIn("captured channel 3 is silent", problems)

    def test_clock_correction_does_not_read_as_crosstalk(self):
        # The live run on the runners held the clock at +1800 ppm; the exact
        # bins then fell on the tones' sidelobes and read 30-40 dB.
        frequencies = [300.0 * 1.25 ** k for k in range(8)]
        for ppm in (-2000.0, -600.0, 600.0, 1800.0, 2000.0):
            with self.subTest(ppm=ppm):
                matrix = assignment_matrix(recording(8, ppm), RATE, frequencies)
                self.assertEqual(assignment_problems(matrix), [])

    def test_crosstalk_is_measured_through_a_clock_correction(self):
        frequencies = [300.0 * 1.25 ** k for k in range(8)]
        problems = assignment_problems(
            assignment_matrix(recording(8, 1800.0, leak=(1, 0, -30.0)), RATE, frequencies)
        )
        self.assertEqual(problems, ["reference channel 1 separation is 30.0 dB"])
        quiet = assignment_problems(
            assignment_matrix(recording(8, 1800.0, leak=(1, 0, -50.0)), RATE, frequencies)
        )
        self.assertEqual(quiet, [])


class SteadySpanTest(unittest.TestCase):
    def test_span_is_the_sounding_part_without_its_edges(self):
        start, end = steady_span(recording(4, start_s=0.59, seconds=4.0), RATE)
        self.assertLessEqual(abs(start - int(0.69 * RATE)), RATE // 100)
        self.assertLessEqual(abs(end - int(4.49 * RATE)), RATE // 100)

    def test_silence_or_a_short_sound_uses_the_whole_recording(self):
        silent = np.zeros((RATE, 4), dtype=np.float32)
        self.assertEqual(steady_span(silent, RATE), (0, RATE))
        short = recording(4, start_s=0.5, seconds=0.3, total_s=2.0)
        self.assertEqual(steady_span(short, RATE), (0, 2 * RATE))


if __name__ == "__main__":
    unittest.main()

"""Unit tests for driver_flow_check's parsing and checks; numpy only, no
device, so the Linux lint job runs them."""

import struct
import unittest

import numpy as np

from driver_flow_check import (
    ABI_INFO, DEFAULT_ENDPOINT_MASK, find_speaker, FLAG_DISCONTINUITY, FLAG_REPEAT, HEADER, KIND_DATA, KIND_FORMAT_CHANGE, KIND_OVERRUN, KIND_STREAM_START,
    check_endpoint, parse_records, parse_wave_format, parse_abi_info,
)

QPC = 10_000_000
RATE = 48_000


def record(kind, endpoint, payload=b"", byte_pos=0, qpc=0, flags=0, epoch=1):
    head = HEADER.pack(kind, flags, epoch, byte_pos, qpc, 0, len(payload), endpoint, 0)
    pad = (-len(payload)) % 8
    return head + payload + b"\0" * pad


def pcm16_format(channels=2, rate=RATE):
    block = channels * 2
    return struct.pack("<HHIIHHH", 1, channels, rate, rate * block, block, 16, 0)


def float_extensible(channels=8, rate=RATE):
    block = channels * 4
    sub = struct.pack("<I", 3) + bytes.fromhex("00001000800000aa00389b71")
    return struct.pack("<HHIIHHHHI", 0xFFFE, channels, rate, rate * block, block, 32, 22, 32, 0x63F) + sub


def tone_stream(endpoint, hz, seconds, fmt_payload, chunk_ms=1, ppm=0.0, gap_at=None, overrun=False):
    """A record stream as the driver writes it: format, start, then 1 ms data records."""
    fmt = parse_wave_format(fmt_payload)
    frames = int(seconds * fmt["rate"])
    t = np.arange(frames) / fmt["rate"]
    mono = 0.5 * np.sin(2 * np.pi * hz * t)
    if fmt["sample"] == "float":
        samples = np.repeat(mono[:, None], fmt["channels"], axis=1).astype("<f4").tobytes()
    else:
        samples = np.repeat((mono * 32767).astype("<i2")[:, None], fmt["channels"], axis=1).tobytes()
    out = [record(KIND_FORMAT_CHANGE, endpoint, fmt_payload), record(KIND_STREAM_START, endpoint)]
    step = fmt["avg_bytes"] * chunk_ms // 1000
    position = 0
    index = 0
    while position < len(samples):
        payload = samples[position:position + step]
        byte_pos = position + (step if gap_at is not None and index > gap_at else 0)
        qpc = int((position + len(payload)) / fmt["avg_bytes"] * QPC / (1 + ppm * 1e-6))
        out.append(record(KIND_DATA, endpoint, payload, byte_pos, qpc))
        if overrun and index == 10:
            out.append(record(KIND_OVERRUN, endpoint, struct.pack("<QQ", 0, 0)))
        position += len(payload)
        index += 1
    return out


def interleave(*streams):
    merged = []
    for parts in zip(*[s + [b""] * (max(map(len, streams)) - len(s)) for s in streams]):
        merged.extend(p for p in parts if p)
    return b"".join(merged)


class DriverFlowTest(unittest.TestCase):
    def test_two_clean_endpoints_pass(self):
        stream = interleave(tone_stream(0, 1000, 2.0, pcm16_format()),
                            tone_stream(1, 1500, 2.0, float_extensible()))
        records = parse_records(stream)
        for endpoint, own, other in [(0, 1000, 1500), (1, 1500, 1000)]:
            summary, problems = check_endpoint(records, endpoint, QPC, own, [other], 1.5)
            self.assertEqual(problems, [], summary)
            self.assertEqual(summary["gaps"], 0)
            self.assertLess(abs(summary["rate_error_ppm"]), 50)
            self.assertGreater(summary["tone_db"], -10)

    def test_new_layout(self):
        self.assertEqual(ABI_INFO.size, 160)
        raw = bytearray(160)
        struct.pack_into("<IIII", raw, 0, 1, 160, 65536, 8 * 1024 * 1024)
        struct.pack_into("<QIIII", raw, 16, 0x1fe, 0x3f, 0x1f, 18, 0xffffffff)
        struct.pack_into("<QQII", raw, 40, 3, QPC, 7, 0)
        for ep in range(3):
            struct.pack_into("<QIIII", raw, 64 + 24 * ep, 0x6c, 0x3f, 0x1f, 18, 0xffffffff)
        info = parse_abi_info(raw)
        self.assertEqual(info["qpc_frequency"], QPC)
        self.assertEqual(info["endpoint_mask"], 7)
        self.assertEqual(info["compiled"]["codecs"], 0x1fe)
        self.assertEqual(info["endpoints"][2]["pcm"]["direct_channels"], 0xffffffff)
        self.assertEqual(info["endpoints"][3]["pcm"]["rates"], 0)
        with self.assertRaises(ValueError): parse_abi_info(raw[:-1])

    def test_sparse_endpoint_masks_have_distinct_tones(self):
        self.assertEqual(DEFAULT_ENDPOINT_MASK, 7)
        for endpoints in ((0,), (0, 1), (0, 2), (2,), (0, 1, 2)):
            stream = interleave(*[tone_stream(ep, {0: 1000, 1: 1500, 2: 2000}[ep], 2.0, pcm16_format()) for ep in endpoints])
            records = parse_records(stream)
            for ep in endpoints:
                own = {0: 1000, 1: 1500, 2: 2000}[ep]
                others = [{0: 1000, 1: 1500, 2: 2000}[other] for other in endpoints if other != ep]
                _, problems = check_endpoint(records, ep, QPC, own, others, 1.5)
                self.assertEqual(problems, [])

    def test_formats_parse(self):
        self.assertEqual(parse_wave_format(pcm16_format())["sample"], "int")
        extensible = parse_wave_format(float_extensible())
        self.assertEqual((extensible["sample"], extensible["bits"], extensible["channels"]), ("float", 32, 8))

    def test_repeated_state_is_not_counted_as_new_events(self):
        parts = tone_stream(0, 1000, 2.0, pcm16_format())
        parts[2:2] = [record(KIND_FORMAT_CHANGE, 0, pcm16_format(), flags=FLAG_REPEAT),
                      record(KIND_STREAM_START, 0, flags=FLAG_REPEAT)]
        summary, problems = check_endpoint(parse_records(b"".join(parts)), 0, QPC, 1000, [1500], 1.5)
        self.assertEqual(problems, [])
        self.assertEqual(summary["starts"], 1)
        self.assertEqual(summary["repeats"], 2)

    def test_a_gap_in_byte_pos_fails(self):
        records = parse_records(b"".join(tone_stream(0, 1000, 2.0, pcm16_format(), gap_at=100)))
        _, problems = check_endpoint(records, 0, QPC, 1000, [1500], 1.5)
        self.assertTrue(any("gaps" in p for p in problems), problems)

    def test_a_discontinuity_flag_counts_as_a_gap(self):
        parts = tone_stream(0, 1000, 2.0, pcm16_format())
        head = HEADER.unpack_from(parts[50])
        parts[50] = HEADER.pack(head[0], FLAG_DISCONTINUITY, *head[2:]) + parts[50][HEADER.size:]
        _, problems = check_endpoint(parse_records(b"".join(parts)), 0, QPC, 1000, [1500], 1.5)
        self.assertTrue(any("gaps" in p for p in problems), problems)

    def test_an_overrun_fails(self):
        records = parse_records(b"".join(tone_stream(0, 1000, 2.0, pcm16_format(), overrun=True)))
        _, problems = check_endpoint(records, 0, QPC, 1000, [1500], 1.5)
        self.assertTrue(any("overrun" in p for p in problems), problems)

    def test_a_dma_off_its_rate_fails(self):
        records = parse_records(b"".join(tone_stream(0, 1000, 2.0, pcm16_format(), ppm=5000)))
        summary, problems = check_endpoint(records, 0, QPC, 1000, [1500], 1.5)
        self.assertGreater(summary["rate_error_ppm"], 4000)
        self.assertTrue(any("ppm" in p for p in problems), problems)

    def test_the_wrong_tone_fails(self):
        records = parse_records(b"".join(tone_stream(0, 1500, 2.0, pcm16_format())))
        _, problems = check_endpoint(records, 0, QPC, 1000, [1500], 1.5)
        self.assertTrue(any("tone" in p for p in problems), problems)

    def test_data_before_a_format_change_fails(self):
        parts = tone_stream(0, 1000, 2.0, pcm16_format())
        records = parse_records(b"".join(parts[2:4] + parts[:2] + parts[4:]))
        _, problems = check_endpoint(records, 0, QPC, 1000, [1500], 1.5)
        self.assertTrue(any("before any format change" in p for p in problems), problems)

    def test_a_cut_record_ends_the_parse(self):
        stream = b"".join(tone_stream(0, 1000, 0.1, pcm16_format()))
        self.assertLess(len(parse_records(stream[:-3])), len(parse_records(stream)))


if __name__ == "__main__":
    unittest.main()

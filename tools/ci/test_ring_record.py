"""Unit tests for ring_record; synthetic records only, so Linux runs them."""

import struct
import unittest

from driver_flow_check import (
    FLAG_DISCONTINUITY,
    FLAG_REPEAT,
    HEADER,
    KIND_DATA,
    KIND_FORMAT_CHANGE,
    KIND_OVERRUN,
    KIND_STREAM_START,
    KIND_STREAM_STOP,
    parse_records,
)
from ring_record import (
    DUMP_FORMAT_VERSION, DUMP_HEADER, DUMP_MAGIC, KIND_READ_END, RECORDER_ENDPOINT,
    dump_header, format_description, metadata, qpc_now, read_end, summarize_records,
)

QPC = 10_000_000
GUID_TAIL = bytes.fromhex("1000800000aa00389b71")


def record(kind, endpoint=0, payload=b"", byte_pos=0, qpc=0, qpc_written=0, flags=0, epoch=1):
    head = HEADER.pack(kind, flags, epoch, byte_pos, qpc, qpc_written, len(payload), endpoint, 0)
    return head + payload + b"\0" * ((-len(payload)) % 8)


def extensible(data1, family=0x0CEA, channels=8, rate=192_000, bits=16, valid=16, mask=0):
    block = channels * bits // 8
    return struct.pack("<HHIIHHHHIIH", 0xFFFE, channels, rate, rate * block, block, bits,
                       34, valid, mask, data1, family) + GUID_TAIL + struct.pack("<III", 48_000, 8, 0)


class RingRecordTest(unittest.TestCase):
    def test_dump_header_bytes(self):
        raw = dump_header(QPC)
        self.assertEqual(len(raw), 32)
        self.assertEqual(DUMP_HEADER.unpack(raw), (DUMP_MAGIC, 1, 32, QPC, DUMP_FORMAT_VERSION, 0))

    def test_read_end_marker_and_monotonic_qpc(self):
        first = qpc_now(QPC)
        second = qpc_now(QPC)
        self.assertGreaterEqual(second, first)
        marker = read_end(second)
        parsed = HEADER.unpack(marker)
        self.assertEqual(parsed[0], KIND_READ_END)
        self.assertEqual(parsed[4], second)
        self.assertEqual(parsed[6], 0)
        self.assertEqual(parsed[7], RECORDER_ENDPOINT)
        self.assertEqual(metadata(parse_records(marker)), [])

    def test_synthetic_stream_summary(self):
        fmt = struct.pack("<HHIIHHH", 1, 2, 48_000, 192_000, 4, 16, 0)
        parts = [record(KIND_FORMAT_CHANGE, payload=fmt), record(KIND_STREAM_START),
                 record(KIND_FORMAT_CHANGE, payload=fmt, flags=FLAG_REPEAT),
                 record(KIND_STREAM_START, flags=FLAG_REPEAT)]
        parts += [
            record(KIND_DATA, payload=b"a" * 100, byte_pos=0, qpc=1000, qpc_written=1000),
            record(KIND_DATA, payload=b"b" * 100, byte_pos=100, qpc=2000, qpc_written=11000),
            record(KIND_DATA, payload=b"c" * 100, byte_pos=250, qpc=3000,
                   qpc_written=31000, flags=FLAG_DISCONTINUITY),
            record(KIND_OVERRUN, payload=struct.pack("<Q", 75)),
            record(KIND_STREAM_STOP),
        ]
        summary = summarize_records(parse_records(b"".join(parts)), QPC, 2.0, 0x1)
        endpoint = summary["endpoints"][0]
        self.assertEqual(endpoint["data_records"], 3)
        self.assertEqual(endpoint["data_bytes"], 300)
        self.assertEqual(endpoint["bytes_per_second"], 150)
        self.assertEqual(endpoint["overrun_records"], 1)
        self.assertEqual(endpoint["lost_bytes"], 75)
        self.assertEqual(endpoint["starts"], 1)
        self.assertEqual(endpoint["stops"], 1)
        self.assertEqual(endpoint["state_repeats"], 2)
        self.assertEqual(len(endpoint["formats"]), 1)
        self.assertEqual(endpoint["byte_pos_discontinuities"], 1)
        self.assertEqual(endpoint["discontinuity_flags"], 1)
        intervals = endpoint["qpc_written_intervals"]
        self.assertEqual(intervals["min_ms"], 1.0)
        self.assertEqual(intervals["median_ms"], 1.5)
        self.assertEqual(intervals["max_ms"], 2.0)
        self.assertAlmostEqual(intervals["p99_ms"], 1.99)

    def test_per_chunk_metadata_summarizes_like_the_whole_stream(self):
        # The recorder parses each READ on its own and keeps no audio; the
        # summary must not change. Endpoint 1 carries a MAT format.
        fmt = struct.pack("<HHIIHHH", 1, 2, 48_000, 192_000, 4, 16, 0)
        chunks = [
            record(KIND_FORMAT_CHANGE, payload=fmt) + record(KIND_FORMAT_CHANGE, 1, extensible(0x010C)),
            record(KIND_DATA, payload=b"a" * 96, qpc=1000, qpc_written=900)
            + record(KIND_DATA, 1, payload=b"m" * 64, qpc=1000),
            record(KIND_DATA, payload=b"b" * 96, byte_pos=96, qpc=2000, qpc_written=10900)
            + record(KIND_OVERRUN, 1, payload=struct.pack("<Q", 32)),
        ]
        whole = summarize_records(parse_records(b"".join(chunks)), QPC, 1.0, 0x3)
        parts = []
        for chunk in chunks:
            parts.extend(metadata(parse_records(chunk)))
        self.assertEqual(summarize_records(parts, QPC, 1.0, 0x3), whole)
        self.assertTrue(all(r["payload"] == b"" for r in parts if r["kind"] == KIND_DATA))
        self.assertEqual(whole["endpoints"][1]["formats"][0]["name"], "IEC 61937 MAT 2.0")
        self.assertEqual(whole["endpoints"][1]["lost_bytes"], 32)

    def test_endpoint_ids_come_from_mask(self):
        for mask, expected in ((0x1, [0]), (0x3, [0, 1]), (0x5, [0, 2])):
            summary = summarize_records([], QPC, 1.0, mask)
            self.assertEqual([item["endpoint"] for item in summary["endpoints"]], expected)

    def test_pcm_shapes_and_direct_out_names(self):
        for bits, valid, subtype, name in (
                (16, 16, 1, "PCM 16-bit"),
                (24, 24, 1, "PCM 24-bit packed"),
                (32, 24, 1, "PCM 24-in-32-bit"),
                (32, 32, 1, "PCM 32-bit"),
                (32, 32, 3, "PCM 32-bit float")):
            fmt = format_description(extensible(subtype, family=0, channels=32,
                                                bits=bits, valid=valid, mask=0))
            self.assertEqual(fmt["name"], name)
            self.assertEqual(fmt["channel_layout"], "direct-out")
            self.assertEqual(fmt["channels"], 32)
        for channels, mask in ((1, 4), (2, 3)):
            pcm = struct.pack("<HHIIHHH", 1, channels, 48_000,
                              96_000 * channels, 2 * channels, 16, 0)
            fmt = format_description(pcm)
            self.assertEqual(fmt["channel_mask"], hex(mask))
            self.assertEqual(fmt["channel_layout"], "speaker-mask")

    def test_format_names(self):
        pcm = struct.pack("<HHIIHHH", 1, 2, 48_000, 192_000, 4, 16, 0)
        self.assertEqual(format_description(pcm)["name"], "PCM 16-bit")
        self.assertEqual(format_description(extensible(0x000A))["name"], "IEC 61937 E-AC-3")
        self.assertEqual(format_description(extensible(0x070C))["name"], "IEC 61937 MAT 2.1")
        self.assertEqual(format_description(extensible(0x010B))["name"], "IEC 61937 DTS:X E1")


if __name__ == "__main__":
    unittest.main()

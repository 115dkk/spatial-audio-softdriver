import unittest

import ssr_abi
from ssr_control import parse_abi_info, parse_stats


class GeneratedAbiTests(unittest.TestCase):
    def test_known_layout_bytes(self):
        raw = bytes.fromhex(
            "010002000300000004000000000000000500000000000000"
            "06000000000000000700000008000900"
        )
        self.assertEqual(ssr_abi.RECORD_HEADER.size, 40)
        self.assertEqual(ssr_abi.RECORD_HEADER.unpack(raw),
                         (1, 2, 3, 4, 5, 6, 7, 8, 9))

    def test_manually_packed_record_header_parses(self):
        raw = bytes.fromhex(
            "01000000010000000000000000000000"
            "10270000000000000000000000000000"
            "0400000000000000"
        ) + b"abcd" + b"\0" * 4
        record = list(ssr_abi.records(raw))[0]
        self.assertEqual(record["kind"], ssr_abi.RECORD_KIND_DATA)
        self.assertEqual(record["qpc"], 10_000)
        self.assertEqual(record["payload"], b"abcd")

    def test_known_query_info_bytes_parse(self):
        values = [0] * len(ssr_abi.ABI_INFO_FIELDS)
        indexes = {name: index for index, name in enumerate(ssr_abi.ABI_INFO_FIELDS)}
        known = {
            "abi_version": 1, "struct_size": ssr_abi.ABI_INFO.size,
            "driver_version": 0x10203, "ring_bytes": 0x400000,
            "compiled.codecs": 0x12, "compiled.pcm.rates": 0x34,
            "compiled.pcm.shapes": 0x56, "compiled.pcm.max_masked_channels": 8,
            "compiled.pcm.direct_channels": 2, "features": 7,
            "qpc_frequency": 10_000_000, "endpoint_mask": 7,
            "endpoints[2].codecs": 0x80, "endpoints[2].pcm.rates": 0x20,
        }
        for name, value in known.items():
            values[indexes[name]] = value
        info = parse_abi_info(ssr_abi.ABI_INFO.pack(*values))
        self.assertEqual(info["driver_version"], 0x10203)
        self.assertEqual(info["compiled"]["pcm"]["max_masked_channels"], 8)
        self.assertEqual(info["endpoints"][2]["codecs"], 0x80)
        self.assertEqual(info["endpoints"][2]["pcm"]["rates"], 0x20)

    def test_known_stats_bytes_parse(self):
        values = [ssr_abi.DRIVER_STATS.size, 3, 47, 0]
        for endpoint in range(ssr_abi.MAX_ENDPOINTS):
            values.extend((1000 + endpoint, 10 + endpoint, 20 + endpoint,
                           30 + endpoint, 40 + endpoint))
        stats = parse_stats(ssr_abi.DRIVER_STATS.pack(*values))
        self.assertEqual(stats["open_count"], 3)
        self.assertEqual(stats["max_timer_lateness_us"], 47)
        self.assertEqual(stats["endpoints"][3], {
            "total_bytes": 1003, "overruns": 13, "lost_bytes": 23,
            "epoch": 33, "state": 43,
        })

    def test_all_structs_are_explicit_little_endian(self):
        for name in dir(ssr_abi):
            value = getattr(ssr_abi, name)
            if hasattr(value, "format") and isinstance(value.format, str):
                self.assertTrue(value.format.startswith("<"), name)


if __name__ == "__main__":
    unittest.main()

import unittest

import numpy as np

from ring_stats import endpoint_stats, threshold_problems
from ssr_abi import (OVERRUN_PAYLOAD, RECORD_FLAG_DISCONTINUITY,
                     RECORD_KIND_DATA, RECORD_KIND_OVERRUN)


def data(pos, qpc, payload, flags=0):
    return {"kind": RECORD_KIND_DATA, "flags": flags, "endpoint": 0,
            "byte_pos": pos, "qpc": qpc, "len": len(payload), "payload": payload}


class RingStatsTests(unittest.TestCase):
    def test_gap_overrun_and_wrong_rate(self):
        records = [data(0, 0, b"a" * 100),
                   data(120, 800_000, b"b" * 100, RECORD_FLAG_DISCONTINUITY),
                   {"kind": RECORD_KIND_OVERRUN, "flags": 0, "endpoint": 0,
                    "byte_pos": 0, "qpc": 0, "len": 8,
                    "payload": OVERRUN_PAYLOAD.pack(20)}]
        fmt = {"avg_bytes": 100, "rate": 100, "channels": 1}
        stats = endpoint_stats(records, 0, 1_000_000, fmt)
        self.assertEqual((stats["gaps"], stats["overruns"], stats["lost_bytes"]), (1, 1, 20))
        self.assertGreater(abs(stats["rate_error_ppm"]), 100)
        found = threshold_problems(0, stats)
        self.assertTrue(any("gaps" in item for item in found))
        self.assertTrue(any("overrun" in item for item in found))
        self.assertTrue(any("ppm" in item for item in found))

    def test_leaked_tone_is_rejected(self):
        rate = 48_000
        t = np.arange(rate) / rate
        samples = (0.5 * np.sin(2 * np.pi * 1000 * t) +
                   0.01 * np.sin(2 * np.pi * 1500 * t)).astype("<f4")
        payload = samples.tobytes()
        records = [data(0, 0, payload), data(len(payload), rate, payload)]
        fmt = {"avg_bytes": rate * 4, "rate": rate, "channels": 1,
               "bits": 32, "sample": "float", "block_align": 4}
        decode = lambda raw, _: np.frombuffer(raw, dtype="<f4").reshape(-1, 1)
        stats = endpoint_stats(records, 0, rate, fmt, decode, 1000, [1500])
        self.assertGreater(stats["other_tone_db"], -60)
        self.assertTrue(any("leaked" in item for item in threshold_problems(0, stats)))


if __name__ == "__main__":
    unittest.main()

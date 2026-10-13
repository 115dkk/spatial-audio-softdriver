import unittest

import numpy as np

from driver_e2e_check import check_report, tone_margin_db


class DriverE2eCheckTests(unittest.TestCase):
    def test_tone_margin_finds_both_brir_preserved_frequencies(self):
        rate = 48_000
        t = np.arange(rate * 2) / rate
        signal = 0.1 * np.sin(2 * np.pi * 1000 * t)
        signal += 0.03 * np.sin(2 * np.pi * 1500 * t)
        rng = np.random.default_rng(7)
        signal += rng.normal(0, 1e-4, len(signal))
        for frequency in (1000, 1500):
            _, _, margin = tone_margin_db(signal, rate, frequency)
            self.assertGreater(margin, 20.0)

    @staticmethod
    def clean_report():
        return {
            "underruns_while_streaming": 0,
            "output_reconnects": 0,
            "output_cadence_faults": 0,
            "dropped_frames": 0,
            "lane_underrun_frames": 0,
            "output_overflow_frames": 0,
            "dropped_events": 0,
            "output_fell_back": False,
            "endpoints": [
                {
                    "endpoint": endpoint,
                    "format": None,
                    "epoch": 0,
                    "unsupported": None,
                }
                for endpoint in range(4)
            ],
            "driver": {"endpoints": [{"endpoint": 0, "lost_bytes": 0}]},
        }

    def test_report_check_accepts_zero_discontinuity_counters(self):
        self.assertEqual(check_report(self.clean_report()), [])

    def test_rejects_a_latency_no_chain_has(self):
        report = self.clean_report()
        report.update(latency_p50_ms=60.0, latency_p99_ms=429384.7)
        self.assertEqual(
            check_report(report), ["latency_p99_ms is 429384.7 ms, above 1000 ms"]
        )

    def test_report_check_rejects_a_missing_required_counter(self):
        report = self.clean_report()
        del report["output_overflow_frames"]
        self.assertIn(
            "report is missing output_overflow_frames", check_report(report)
        )

    def test_report_check_names_every_failed_counter(self):
        report = self.clean_report()
        report.update(
            underruns_while_streaming=2,
            output_reconnects=8,
            output_cadence_faults=9,
            dropped_frames=3,
            lane_underrun_frames=4,
            output_overflow_frames=5,
            dropped_events=6,
        )
        report["driver"]["endpoints"][0] = {"endpoint": 1, "lost_bytes": 7}
        failures = check_report(report)
        self.assertEqual(len(failures), 8)
        self.assertTrue(any("endpoint 1" in failure for failure in failures))

    def test_report_check_rejects_shared_fallback(self):
        report = self.clean_report()
        report["output_fell_back"] = True
        self.assertIn(
            "exclusive output fell back to shared mode", check_report(report)
        )

    def test_report_check_rejects_endpoint_unsupported_state(self):
        report = self.clean_report()
        report["endpoints"][2]["unsupported"] = "unsupported PCM sample type"
        self.assertIn(
            "endpoint 2 has unsupported format: unsupported PCM sample type",
            check_report(report),
        )

    def test_report_check_requires_endpoint_schema(self):
        report = self.clean_report()
        del report["endpoints"][3]["format"]
        self.assertIn("report endpoint 3 is missing format", check_report(report))


if __name__ == "__main__":
    unittest.main()

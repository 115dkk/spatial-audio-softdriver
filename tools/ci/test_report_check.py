from dataclasses import replace
import unittest

from report_check import PRESETS, problems


def report():
    return {
        "mode": "driver", "output_mode": "shared", "output_fell_back": False,
        "output_kind": "speakers", "clock_strategy": "trim",
        "output_sample_rate": 48000, "trim_available": True,
        "output_reconnects": 0, "output_cadence_ratio": 1.0, "faults": [],
        "latency_p50_ms": 20.0, "latency_p99_ms": 30.0,
        "underruns": 0, "underruns_while_streaming": 0, "output_restarts": 0,
        "output_cadence_faults": 0, "dropped_frames": 0,
        "lane_underrun_frames": 0, "output_overflow_frames": 0,
        "dropped_events": 0,
        "endpoints": [{"endpoint": n, "format": None, "epoch": 0,
                       "unsupported": None} for n in range(4)],
        "driver": {"endpoints": [{"endpoint": n, "lost_bytes": 0} for n in range(4)]},
    }


class ReportCheckTests(unittest.TestCase):
    def good(self, preset):
        value = report()
        expect = PRESETS[preset]
        if expect.source:
            value["mode"] = expect.source
        if expect.output_mode:
            value["output_mode"] = expect.output_mode
        if expect.output_kind:
            value["output_kind"] = expect.output_kind
        if expect.reconnects == "required":
            value["output_reconnects"] = 1
        return value

    def test_every_preset_accepts_a_good_report(self):
        for name in PRESETS:
            with self.subTest(name=name):
                self.assertEqual(problems(self.good(name), PRESETS[name]), [])

    def test_every_zero_counter_is_named(self):
        for name, expect in PRESETS.items():
            for key in expect.zero_counters:
                value = self.good(name)
                value[key] = 7
                with self.subTest(preset=name, key=key):
                    self.assertIn(f"{key} is 7", problems(value, expect))

    def test_missing_fields_are_problems_not_exceptions(self):
        for name, expect in PRESETS.items():
            value = self.good(name)
            value.pop("faults")
            with self.subTest(name=name):
                self.assertIn("report is missing faults", problems(value, expect))

    def test_clock_auto_and_explicit_rules(self):
        expect = PRESETS["listen"]
        value = self.good("listen")
        self.assertEqual(problems(value, expect), [])
        value["trim_available"] = False
        value["clock_strategy"] = "asrc"
        self.assertEqual(problems(value, expect), [])
        value["clock_strategy"] = "trim"
        self.assertIn("clock_strategy is trim, auto requires asrc", problems(value, expect))
        del value["output_sample_rate"]
        self.assertIn("report is missing output_sample_rate", problems(value, expect))
        explicit = replace(expect, clock="asrc")
        value = self.good("listen")
        value["clock_strategy"] = "trim"
        self.assertIn("clock_strategy is trim, expected asrc", problems(value, explicit))

    def test_replug_allows_only_output_faults(self):
        value = self.good("replug")
        value["faults"] = ["OutputDeviceLost"]
        self.assertEqual(problems(value, PRESETS["replug"]), [])
        value["faults"].append("DecodeFailed")
        self.assertTrue(any("expected output outage" in item
                            for item in problems(value, PRESETS["replug"])))

    def test_cadence_driver_and_endpoint_failures_are_named(self):
        value = self.good("driver_e2e")
        value["output_cadence_ratio"] = 1.03
        value["driver"]["endpoints"][2]["lost_bytes"] = 4
        value["endpoints"][1]["unsupported"] = "bad format"
        found = problems(value, PRESETS["driver_e2e"])
        self.assertTrue(any("1.03" in item for item in found))
        self.assertTrue(any("endpoint 2" in item and "4" in item for item in found))
        self.assertTrue(any("endpoint 1" in item and "bad format" in item for item in found))


if __name__ == "__main__":
    unittest.main()

import itertools
import json
import tempfile
import unittest
from pathlib import Path
from unittest.mock import Mock, patch

from driver_flow_check import (FLAG_REPEAT, HEADER, KIND_VOLUME, LEGACY_VOLUME, VOLUME,
                               check_endpoint, parse_records, parse_volume)
from ring_record import metadata, summarize_records
from volume_check import capture, matches, verify


def snapshot(db=-20, mute=False):
    return {"master_db": db, "channels_db": [db] * 8, "mute": mute, "hardware_support": 3}


def record(qpc, mute=False, repeat=False):
    return {"endpoint": 0, "qpc": qpc, "repeat": repeat, "channel_count": 8,
            "mute_mask": 255 if mute else 0, "level_q16": [-20 * 65536] * 8 + [0] * 24}


def evidence():
    stages = [{"name": name, "start_qpc": i * 10, "end_qpc": i * 10 + 9,
               "expected": snapshot(mute=(name == "mute_on"))}
              for i, name in enumerate(["level", "mute_on", "mute_off"], 1)]
    captured = {"query_info": {"endpoint_mask": 1}, "errors": [],
                "records": [record(11), record(21, True), record(31)]}
    actions = {"errors": [], "endpoints": [{"endpoint": 0, "name": "Spatial SoftReceiver",
               "original": snapshot(-40), "restored": snapshot(-40), "stages": stages}]}
    return captured, actions


class VolumeCheckTests(unittest.TestCase):
    def test_capture_stops_even_while_other_audio_keeps_arriving(self):
        device = Mock()
        device.query_info.return_value = {"endpoint_mask": 1}
        device.read.return_value = HEADER.pack(1, 0, 0, 0, 0, 0, 0, 0, 0)
        with tempfile.TemporaryDirectory() as folder:
            paths = [Path(folder) / name for name in ("out.json", "ready.json", "stop")]
            paths[2].touch()
            with patch("volume_check.ControlDevice", return_value=device), patch(
                    "volume_check.time.monotonic", side_effect=itertools.count(0, 0.1)):
                self.assertEqual(capture(*paths, 120), 0)
            self.assertEqual(json.loads(paths[0].read_text())["errors"], [])
        device.close.assert_called_once()
        self.assertLess(device.read.call_count, 10)

    def test_capture_failure_writes_evidence_and_closes_handle(self):
        device = Mock()
        device.query_info.side_effect = OSError("query failed")
        with tempfile.TemporaryDirectory() as folder:
            paths = [Path(folder) / name for name in ("out.json", "ready.json", "stop")]
            with patch("volume_check.ControlDevice", return_value=device):
                self.assertEqual(capture(*paths, 120), 1)
            self.assertIn("query failed", json.loads(paths[0].read_text())["errors"])
            self.assertFalse(paths[1].exists())
        device.close.assert_called_once()

    def test_valid_hardware_changes_and_restore(self):
        self.assertTrue(verify(*evidence())["passed"])

    def test_repeats_cannot_pass_a_changed_value_test(self):
        captured, actions = evidence()
        captured["records"][0]["repeat"] = True
        self.assertFalse(verify(captured, actions)["passed"])

    def test_outside_time_window_or_wrong_final_state_fails(self):
        captured, actions = evidence()
        captured["records"][0]["qpc"] = 9
        self.assertFalse(verify(captured, actions)["passed"])
        captured, actions = evidence()
        captured["records"].insert(1, record(12, True))
        self.assertFalse(verify(captured, actions)["passed"])

    def test_hardware_endpoint_set_restore_and_capture_errors_fail(self):
        for problem in ("hardware", "endpoint", "restore", "capture", "stages"):
            captured, actions = evidence()
            item = actions["endpoints"][0]
            if problem == "hardware": item["original"]["hardware_support"] = 1
            if problem == "endpoint": captured["query_info"]["endpoint_mask"] = 3
            if problem == "restore": item["restored"]["channels_db"][1] = -1
            if problem == "capture": captured["errors"].append("read failed")
            if problem == "stages": item["stages"].pop()
            self.assertFalse(verify(captured, actions)["passed"], problem)

    def test_every_channel_matters_with_half_db_tolerance(self):
        r = record(11)
        self.assertTrue(matches(r, snapshot()))
        r["level_q16"][1] -= 32768
        self.assertTrue(matches(r, snapshot()))
        r["level_q16"][1] -= 1
        self.assertFalse(matches(r, snapshot()))
        self.assertFalse(matches(record(11), snapshot(mute=True)))

    def test_volume_payload_rejects_invalid_state(self):
        for payload in [b"", VOLUME.pack(0, 0, *([0] * 32)),
                        VOLUME.pack(33, 0, *([0] * 32)), VOLUME.pack(2, 4, *([0] * 32)),
                        VOLUME.pack(2, 0, *([1] + [0] * 31)),
                        VOLUME.pack(2, 0, *([0, 0, -1] + [0] * 29))]:
            with self.assertRaises(ValueError): parse_volume(payload)

    def test_volume_32_channels_and_high_mute_bit(self):
        value = parse_volume(VOLUME.pack(32, 0x80000000, *([0] * 32)))
        self.assertEqual(value["channel_count"], 32)
        self.assertEqual(value["mute_mask"], 0x80000000)

    def test_legacy_volume_extends_sixteen_levels_with_zeroes(self):
        value = parse_volume(LEGACY_VOLUME.pack(2, 1, -20 * 65536, 0, *([0] * 14)))
        self.assertEqual(value["channel_count"], 2)
        self.assertEqual(value["mute_mask"], 1)
        self.assertEqual(value["level_q16"], [-20 * 65536] + [0] * 31)
        muted = parse_volume(LEGACY_VOLUME.pack(16, 0xffff, *([0] * 16)))
        self.assertEqual(muted["mute_mask"], 0xffff)
        with self.assertRaises(ValueError):
            parse_volume(LEGACY_VOLUME.pack(17, 0, *([0] * 16)))

    def test_default_device_volume_channels(self):
        captured, actions = evidence()
        actions["endpoints"][0]["original"]["channels_db"] = [-40] * 32
        self.assertFalse(verify(captured, actions)["passed"])

    def test_ring_and_flow_summaries_keep_volume_payload_and_repeats(self):
        payload = VOLUME.pack(2, 1, -20 * 65536, 0, *([0] * 30))
        stream = b"".join(HEADER.pack(KIND_VOLUME, flags, 0, 0, 10, 0, len(payload), 0, 0) + payload
                          for flags in (0, FLAG_REPEAT))
        records = parse_records(stream)
        self.assertEqual(metadata(records)[0]["payload"], payload)
        summary = summarize_records(metadata(records), 1000, 1, 1)["endpoints"][0]
        self.assertEqual((summary["volume_records"], summary["volume_changes"], summary["volume_repeats"]), (2, 1, 1))
        self.assertEqual(summary["volume"]["levels_db"], [-20, 0])
        flow, _ = check_endpoint(records, 0, 1000, 1000, [], 0)
        self.assertEqual((flow["volume_records"], flow["volume_changes"], flow["volume_repeats"]), (2, 1, 1))


if __name__ == "__main__":
    unittest.main()

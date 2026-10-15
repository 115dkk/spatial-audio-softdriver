"""Unit tests for the ADR 0033 guard check and its fake pipe peer."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

from fake_app import restore_roles, take_over
from guard_check import (baseline_roles, check, fallback_id, launch_verdict,
                         run_guard_process, verdict)
from host_role_check import ROLES


OURS = "{0.0.0.00000000}.{6c1e7a1d-0000-4000-8000-000000000000}"
CABLE = "{0.0.0.00000000}.{0c0ab1e0-0000-4000-8000-000000000000}"
SPEAKERS = "{0.0.0.00000000}.{cc529431-0000-4000-8000-000000000000}"


def roles(device):
    return dict.fromkeys(ROLES, device)


class FakeHost:
    def __init__(self, defaults):
        self.defaults = dict(defaults)
        self.outputs = [
            {"id": OURS, "name": "Spatial SoftReceiver", "state": "active", "endpoint": 0},
            {"id": CABLE, "name": "Speakers (VB-Audio Virtual Cable)", "state": "active"},
            {"id": SPEAKERS, "name": "Desk Speakers", "state": "active"},
        ]

    def __call__(self, args):
        if args == ["host", "show"]:
            return {"default_roles": dict(self.defaults), "outputs": self.outputs}
        if args[:2] == ["host", "set-default"]:
            role = args[args.index("--role") + 1]
            device = args[args.index("--id") + 1]
            self.defaults[role] = device
            return {"ok": True, "role": role}
        raise AssertionError(args)


class GuardVerdictTests(unittest.TestCase):
    def test_exit_and_stall_require_restoration(self):
        restored = roles(CABLE)
        for reason in ("app_exited", "restart_failed"):
            self.assertEqual(verdict(1, {"reason": reason}, False,
                                     restored, restored, reason), [])
        self.assertIn("restore-point.json remains",
                      verdict(1, {"reason": "app_exited"}, True,
                              restored, restored, "app_exited"))
        self.assertIn("marker reason is 'restart_failed', expected 'app_exited'",
                      verdict(1, {"reason": "restart_failed"}, False,
                              restored, restored, "app_exited"))
        self.assertIn("guard timed out",
                      verdict(1, {"reason": "app_exited"}, False,
                              restored, restored, "app_exited", timed_out=True))

    def test_clean_exit_restores_baseline_and_has_no_marker(self):
        baseline = roles(CABLE)
        self.assertEqual(verdict(0, None, False, baseline, baseline, None), [])
        self.assertIn("clean exit wrote a marker",
                      verdict(0, {"reason": "app_exited"}, False,
                              baseline, baseline, None))

    def test_launch_verdict_requires_guarded_resume_order_and_baseline(self):
        baseline = roles(CABLE)
        first = {"guarded": True, "resume": False, "roles_at_start": None}
        resumed = {"guarded": True, "resume": True, "roles_at_start": baseline}
        self.assertEqual(launch_verdict("stall", [first, resumed], baseline), [])
        self.assertEqual(launch_verdict("exit", [first], baseline), [])
        self.assertTrue(launch_verdict("stall", [resumed, first], baseline))
        resumed["roles_at_start"] = roles(OURS)
        self.assertIn(f"resumed console started as {OURS!r}, expected {CABLE!r}",
                      launch_verdict("stall", [first, resumed], baseline))
        self.assertTrue(launch_verdict("exit-clean", [{"guarded": False,
                                                        "resume": False}], baseline))

    def test_fallback_accepts_runner_virtual_cable_name(self):
        show = FakeHost(roles(SPEAKERS))(["host", "show"])
        self.assertEqual(fallback_id(show, OURS), CABLE)
        for name in ("VB-CABLE Input", "CABLE Input (VB-Audio)"):
            show["outputs"][1]["name"] = name
            self.assertEqual(fallback_id(show, OURS), CABLE)

    def test_baseline_replaces_ours_and_empty_roles_with_fallback(self):
        original = {"console": OURS, "multimedia": None, "communications": SPEAKERS}
        self.assertEqual(baseline_roles(original, OURS.lower(), CABLE), {
            "console": CABLE,
            "multimedia": CABLE,
            "communications": SPEAKERS,
        })


class GuardCheckTests(unittest.TestCase):
    def test_check_drives_all_scenarios_with_fake_host_and_guard(self):
        original = {"console": OURS, "multimedia": None, "communications": SPEAKERS}
        baseline = {"console": CABLE, "multimedia": CABLE, "communications": SPEAKERS}
        host = FakeHost(original)
        with tempfile.TemporaryDirectory() as folder:
            state_dir = Path(folder)

            def run_guard(mode, env):
                self.assertEqual(env["SSR_FAKE_OURS"], OURS)
                self.assertEqual(env["SSR_FAKE_STATE_DIR"], str(state_dir))
                point = json.loads(env["SSR_FAKE_RESTORE_POINT"])
                self.assertEqual(point["previous"], baseline)
                launches = [{"mode": mode, "guarded": True, "resume": False,
                             "roles_at_start": None}]
                if mode == "stall":
                    host.defaults = dict(point["previous"])
                    launches.append({"mode": mode, "guarded": True, "resume": True,
                                     "roles_at_start": dict(host.defaults)})
                    host.defaults = roles(OURS)
                host.defaults = dict(point["previous"])
                (state_dir / "restore-point.json").unlink()
                reason = {"exit": "app_exited", "stall": "restart_failed"}.get(mode)
                if reason:
                    (state_dir / "guard-restored.json").write_text(
                        json.dumps({"reason": reason}), encoding="utf-8")
                with (state_dir / "fake_app.log").open("w", encoding="utf-8") as stream:
                    for launch in launches:
                        stream.write(json.dumps(launch) + "\n")
                result = subprocess.CompletedProcess([], 0 if mode == "exit-clean" else 1,
                                                     "out", "err")
                result.timed_out = False
                result.cleanup_failed = False
                return result

            summary = check(host, run_guard, state_dir)

        self.assertEqual(summary["problems"], [])
        self.assertEqual(summary["original_roles"], original)
        self.assertEqual(summary["baseline_roles"], baseline)
        self.assertEqual(summary["final_roles"], baseline)
        self.assertTrue(all(not scenario["timed_out"]
                            for scenario in summary["scenarios"].values()))

    def test_process_runner_marks_and_kills_timeout(self):
        with tempfile.TemporaryDirectory() as folder:
            result = run_guard_process(
                [sys.executable, "-c", "import time; time.sleep(10)"],
                os.environ.copy(), Path(folder), timeout=0.01,
            )
        self.assertTrue(result.timed_out)
        self.assertFalse(result.cleanup_failed)
        self.assertNotEqual(result.returncode, 0)

    def test_process_runner_collects_output_from_files(self):
        with tempfile.TemporaryDirectory() as folder:
            result = run_guard_process(
                [sys.executable, "-c",
                 "import sys; print('out'); print('err', file=sys.stderr); sys.exit(3)"],
                os.environ.copy(), Path(folder), timeout=10,
            )
            self.assertTrue((Path(folder) / "guard-stdout.txt").exists())
        self.assertEqual(result.returncode, 3)
        self.assertEqual(result.stdout.strip(), "out")
        self.assertEqual(result.stderr.strip(), "err")
        self.assertFalse(result.timed_out)


class FakeAppHelperTests(unittest.TestCase):
    def test_takeover_claims_roles_and_records_own_pid(self):
        host = FakeHost(roles(CABLE))
        point = {"version": 1, "ours": OURS, "previous": roles(CABLE),
                 "fallback": CABLE, "app_pid": 0}
        with tempfile.TemporaryDirectory() as folder:
            restore_path = Path(folder) / "restore-point.json"
            take_over(host, restore_path, json.dumps(point), OURS)
            written = json.loads(restore_path.read_text(encoding="utf-8"))
        self.assertEqual(host.defaults, roles(OURS))
        self.assertEqual(written["app_pid"], os.getpid())

    def test_restore_roles_uses_previous_or_fallback_only_for_ours(self):
        host = FakeHost({"console": OURS, "multimedia": OURS,
                         "communications": SPEAKERS})
        point = {"ours": OURS,
                 "previous": {"console": CABLE, "multimedia": OURS,
                              "communications": CABLE},
                 "fallback": CABLE}
        restore_roles(host, point)
        self.assertEqual(host.defaults, {
            "console": CABLE,
            "multimedia": CABLE,
            "communications": SPEAKERS,
        })


class FakeAppTests(unittest.TestCase):
    def run_fake(self, mode, *extra):
        fake = Path(__file__).with_name("fake_app.py")
        with tempfile.TemporaryDirectory() as folder:
            folder = Path(folder)
            result = subprocess.run(
                [sys.executable, str(fake), mode, "--guarded", "--interval", "0", *extra],
                capture_output=True, encoding="utf-8", timeout=5,
                env={"SSR_GUARD_STATE_DIR": str(folder)},
            )
            heartbeats = [json.loads(line) for line in result.stdout.splitlines()]
            launches = [json.loads(line) for line in
                        (folder / "fake_app.log").read_text(encoding="utf-8").splitlines()]
            restore_exists = (folder / "restore-point.json").exists()
        return result, heartbeats, launches, restore_exists

    def test_exit_emits_three_heartbeats_and_exits(self):
        result, beats, launches, _ = self.run_fake("exit")
        self.assertEqual(result.returncode, 0)
        self.assertEqual([beat["output_periods"] for beat in beats], [1, 2, 3])
        self.assertTrue(all(beat["running"] for beat in beats))
        self.assertEqual(launches[0]["argv"], ["--guarded"])
        self.assertIsNone(launches[0]["roles_at_start"])

    def test_stall_can_be_bounded_for_tests_without_live(self):
        result, beats, launches, _ = self.run_fake(
            "stall", "--resume", "--stall-seconds", "0")
        self.assertEqual(result.returncode, 18)
        self.assertEqual(len(beats), 3)
        self.assertIn("--resume", launches[0]["argv"])
        self.assertIsNone(launches[0]["roles_at_start"])

    def test_exit_clean_removes_the_restore_point(self):
        fake = Path(__file__).with_name("fake_app.py")
        with tempfile.TemporaryDirectory() as folder:
            folder = Path(folder)
            (folder / "restore-point.json").write_text("{}", encoding="utf-8")
            result = subprocess.run(
                [sys.executable, str(fake), "exit-clean", "--interval", "0"],
                capture_output=True, encoding="utf-8", timeout=5,
                env={"SSR_GUARD_STATE_DIR": str(folder)},
            )
            self.assertEqual(result.returncode, 0)
            self.assertFalse((folder / "restore-point.json").exists())


if __name__ == "__main__":
    unittest.main()

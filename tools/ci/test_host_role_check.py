"""host_role_check driven by a scripted host instead of ssr-live."""
import contextlib
import io
import json
import os
from pathlib import Path
import sys
import tempfile
import textwrap
import unittest

from host_role_check import RunError, main as check_main, our_id, round_trip

OURS = "{0.0.0.00000000}.{6c1e7a1d-0000-4000-8000-000000000000}"
MUSIC = "{0.0.0.00000000}.{6c1e7a1d-0000-4000-8000-000000000001}"
SPEAKERS = "{0.0.0.00000000}.{5be4e7a1-0000-4000-8000-000000000000}"
HEADSET = "{0.0.0.00000000}.{5be4e7a1-0000-4000-8000-000000000001}"


class FakeHost:
    """`ssr-live host` over a dictionary of roles. `ignored` roles accept
    set-default but keep their device; `refused` roles fail it."""

    def __init__(self, roles, ignored=(), refused=()):
        self.roles = dict(roles)
        self.ignored, self.refused = set(ignored), set(refused)
        self.calls = []

    def show(self):
        return {
            "platform": {"os_build": 26100, "arch": "x64", "test_signing": True,
                         "secure_boot": "disabled"},
            "driver": {"state": "installed", "problem_code": 0, "service_running": True,
                       "exclusive_policy": "applied"},
            "outputs": [
                {"id": SPEAKERS, "name": "Speakers", "endpoint": None, "state": "active"},
                {"id": HEADSET, "name": "Headset", "endpoint": None, "state": "active"},
                {"id": OURS, "name": "공간 소프트리시버", "endpoint": 0, "state": "active"},
                {"id": MUSIC, "name": "공간 소프트리시버 음악", "endpoint": 1, "state": "active"},
            ],
            "default_roles": dict(self.roles),
        }

    def run(self, args):
        self.calls.append(list(args))
        if args == ["host", "show"]:
            return self.show()
        assert args[:3] == ["host", "set-default", "--role"] and args[4] == "--id", args
        role, device = args[3], args[5]
        if role in self.refused:
            raise RunError(f"host set-default exited with 1: the host refused: {role}")
        if role not in self.ignored:
            self.roles[role] = device
        return {"ok": True, "role": role, "id": device}

    def sets(self):
        return [(call[3], call[5]) for call in self.calls if call[1] == "set-default"]


BEFORE = {"console": SPEAKERS, "multimedia": SPEAKERS, "communications": HEADSET}


class HostRoleCheckTests(unittest.TestCase):
    def test_round_trip_takes_every_role_and_gives_it_back(self):
        host = FakeHost(BEFORE)
        summary = round_trip(host.run)
        self.assertEqual(summary["problems"], [])
        self.assertEqual(summary["notes"], [])
        self.assertEqual(summary["ours"], OURS)
        self.assertEqual(summary["before"], BEFORE)
        self.assertEqual(summary["after"], BEFORE)
        self.assertEqual(host.sets(), [
            ("console", OURS), ("multimedia", OURS), ("communications", OURS),
            ("console", SPEAKERS), ("multimedia", SPEAKERS), ("communications", HEADSET),
        ])
        self.assertEqual(host.roles, BEFORE)

    def test_ids_compare_without_case(self):
        host = FakeHost(BEFORE)
        show = host.show

        def upper_show():
            document = show()
            document["default_roles"] = {role: device and device.upper()
                                         for role, device in document["default_roles"].items()}
            return document

        host.show = upper_show
        self.assertEqual(round_trip(host.run)["problems"], [])

    def test_a_set_that_does_not_take_names_its_role(self):
        host = FakeHost(BEFORE, ignored={"multimedia"})
        summary = round_trip(host.run)
        self.assertEqual(summary["problems"], [
            f"multimedia: host show reports {SPEAKERS} after set-default, not ours {OURS}",
        ])
        self.assertEqual(summary["after"], BEFORE)

    def test_a_refused_set_names_its_role_and_the_others_still_run(self):
        host = FakeHost(BEFORE, refused={"console"})
        summary = round_trip(host.run)
        self.assertEqual(len(summary["problems"]), 2)
        self.assertTrue(summary["problems"][0].startswith("console: host set-default exited"))
        self.assertTrue(summary["problems"][1].startswith(f"console: restoring {SPEAKERS}: "))
        self.assertIn(("multimedia", OURS), host.sets())
        self.assertIn(("communications", HEADSET), host.sets())
        self.assertEqual(host.roles, BEFORE)

    def test_a_role_that_was_null_is_left_alone(self):
        before = dict(BEFORE, communications=None)
        host = FakeHost(before)
        summary = round_trip(host.run)
        self.assertEqual(summary["problems"], [])
        self.assertEqual(summary["notes"],
                         ["communications had no default device before; left as it is"])
        self.assertNotIn(("communications", None), host.sets())
        self.assertEqual([role for role, _ in host.sets()].count("communications"), 1)
        self.assertEqual(summary["after"], dict(before, communications=OURS))

    def test_a_role_not_put_back_is_named(self):
        host = FakeHost(BEFORE)
        run = host.run

        def run_losing_console_restore(args):
            if args[:4] == ["host", "set-default", "--role", "console"] and args[5] == SPEAKERS:
                return {"ok": True, "role": "console", "id": SPEAKERS}
            return run(args)

        summary = round_trip(run_losing_console_restore)
        self.assertEqual(summary["problems"],
                         [f"console: host show reports {OURS} after restoring, not {SPEAKERS}"])

    def test_without_our_active_endpoint_nothing_is_changed(self):
        host = FakeHost(BEFORE)
        show = host.show

        def show_disabled():
            document = show()
            document["outputs"][2]["state"] = "disabled"
            return document

        host.show = show_disabled
        summary = round_trip(host.run)
        self.assertEqual(summary["problems"],
                         ["host show: 0 active outputs carry endpoint 0, expected exactly one"])
        self.assertEqual(host.sets(), [])
        with self.assertRaises(ValueError):
            our_id({"outputs": [show()["outputs"][2]] * 2})

    @unittest.skipUnless(os.name == "posix", "the fake ssr-live is a script with a shebang")
    def test_main_drives_a_binary_and_writes_the_summary(self):
        with tempfile.TemporaryDirectory() as folder:
            folder = Path(folder)
            state = folder / "state"
            state.mkdir()
            (state / "roles.json").write_text(json.dumps(BEFORE), encoding="utf-8")
            fake = folder / "ssr-live"
            fake.write_text(f"#!{sys.executable}\n" + textwrap.dedent(f"""
                import json, sys
                from pathlib import Path
                args = sys.argv[1:]
                assert args[-2] == "--state-dir", args
                path = Path(args[-1]) / "roles.json"
                roles = json.loads(path.read_text())
                if args[:2] == ["host", "show"]:
                    print(json.dumps({{"outputs": [
                        {{"id": {OURS!r}, "name": "x", "endpoint": 0, "state": "active"}}],
                        "default_roles": roles}}))
                elif args[:2] == ["host", "set-default"]:
                    roles[args[3]] = args[5]
                    path.write_text(json.dumps(roles))
                    print(json.dumps({{"ok": True, "role": args[3], "id": args[5]}}))
                else:
                    sys.exit("unknown command")
                """), encoding="utf-8")
            fake.chmod(0o755)
            summary_path = folder / "roles-summary.json"
            argv = ["--ssr-live", str(fake), "--summary", str(summary_path),
                    "--state-dir", str(state)]
            with contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(check_main(argv), 0)
            summary = json.loads(summary_path.read_text(encoding="utf-8"))
            self.assertEqual((summary["before"], summary["after"], summary["ours"]),
                             (BEFORE, BEFORE, OURS))

            (state / "roles.json").write_text("not json", encoding="utf-8")
            with contextlib.redirect_stdout(io.StringIO()), \
                    contextlib.redirect_stderr(io.StringIO()):
                self.assertEqual(check_main(argv), 1)
            summary = json.loads(summary_path.read_text(encoding="utf-8"))
            self.assertTrue(summary["problems"][0].startswith("host show: host show exited with 1"))


if __name__ == "__main__":
    unittest.main()

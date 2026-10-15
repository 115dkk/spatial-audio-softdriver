"""host_check against hand-made `ssr-live host show` and endpoints.ps1 documents."""
import contextlib
import copy
import io
import json
from pathlib import Path
import tempfile
import unittest

from host_check import check, main as check_main


def main(argv):
    with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
        return check_main(argv)

PRIMARY = "{0.0.0.00000000}.{6c1e7a1d-0000-4000-8000-000000000000}"
MUSIC = "{0.0.0.00000000}.{6c1e7a1d-0000-4000-8000-000000000001}"
PASS = "{0.0.0.00000000}.{6c1e7a1d-0000-4000-8000-000000000002}"
OLD_MUSIC = "{0.0.0.00000000}.{6c1e7a1d-0000-4000-8000-0000000000ff}"
CABLE = "{0.0.0.00000000}.{0c0ab1e0-0000-4000-8000-000000000000}"


def host():
    return {
        "platform": {"os_build": 26100, "arch": "x64", "test_signing": True,
                     "secure_boot": "disabled"},
        "driver": {"state": "installed", "problem_code": 0, "service_running": True,
                   "exclusive_policy": "applied"},
        "outputs": [
            {"id": CABLE, "name": "CABLE Input", "endpoint": None, "state": "active"},
            {"id": PRIMARY, "name": "공간 소프트리시버", "endpoint": 0, "state": "active"},
            {"id": MUSIC, "name": "공간 소프트리시버 음악", "endpoint": 1, "state": "active"},
            {"id": PASS, "name": "공간 소프트리시버 PASS", "endpoint": 2, "state": "active"},
            # A stale instance of ours that both helpers still see.
            {"id": OLD_MUSIC, "name": "old", "endpoint": 1, "state": "disabled"},
        ],
        "default_roles": {"console": PRIMARY, "multimedia": CABLE, "communications": None},
    }


def endpoints():
    # endpoints.ps1 order and id case are its own; states are DEVICE_STATE_*.
    return [
        {"endpoint": 2, "id": PASS.upper(), "name": "PASS", "state": 1},
        {"endpoint": 1, "id": OLD_MUSIC, "name": "old", "state": 2},
        {"endpoint": 0, "id": PRIMARY, "name": "primary", "state": 1},
        {"endpoint": 1, "id": MUSIC, "name": "music", "state": 1},
    ]


def output(document, endpoint, state="active"):
    return next(item for item in document["outputs"]
                if item["endpoint"] == endpoint and item["state"] == state)


class HostCheckTests(unittest.TestCase):
    def test_agreeing_documents_pass(self):
        self.assertEqual(check(host(), endpoints(), 7, "installed", "applied"), [])
        self.assertEqual(check(host(), endpoints(), 7, "installed", "any"), [])

    def test_a_wrong_id_is_named_with_both_ids(self):
        rows = endpoints()
        rows[3]["id"] = CABLE
        problems = check(host(), rows, 7, "installed", "applied")
        self.assertIn(f"endpoint 1: host reports {MUSIC}, endpoints.json {CABLE}", problems)
        self.assertIn(f"output {MUSIC} carries endpoint 1, but endpoints.json does not list it",
                      problems)
        self.assertIn(f"endpoints.json maps {CABLE} to endpoint 1, the host gives it no endpoint "
                      "number", problems)

    def test_a_number_the_two_mappings_disagree_on_is_named(self):
        document = host()
        output(document, 2)["endpoint"] = 0
        problems = check(document, endpoints(), 7, "installed", "applied")
        self.assertIn(f"output {PASS} carries endpoint 0, endpoints.json says 2", problems)
        self.assertIn("endpoint 0: 2 active outputs carry it, expected exactly one", problems)

    def test_a_missing_endpoint_is_named(self):
        document = host()
        document["outputs"].remove(output(document, 2))
        rows = [row for row in endpoints() if row["endpoint"] != 2]
        self.assertEqual(check(document, rows, 7, "installed", "applied"), [
            "endpoint 2: 0 active outputs carry it, expected exactly one",
            "endpoint 2: endpoints.json has no active endpoint 2",
        ])
        # The same documents are right for a driver whose mask leaves 2 out.
        self.assertEqual(check(document, rows, 3, "installed", "applied"), [])

    def test_an_endpoint_the_mask_disables_must_not_be_active(self):
        problems = check(host(), endpoints(), 3, "installed", "applied")
        self.assertEqual(problems, [f"endpoint 2 is outside the mask 0x3 but output {PASS} is active"])

    def test_an_endpoint_disabled_in_windows_but_active_in_the_host_is_named(self):
        rows = endpoints()
        rows[0]["state"] = 2
        problems = check(host(), rows, 7, "installed", "applied")
        self.assertIn("endpoint 2: endpoints.json has no active endpoint 2", problems)
        self.assertIn(f"output {PASS} (endpoint 2) is active, endpoints.json says disabled", problems)

    def test_the_policy_must_be_the_expected_one(self):
        document = host()
        document["driver"]["exclusive_policy"] = "missing"
        self.assertEqual(check(document, endpoints(), 7, "installed", "applied"),
                         ["exclusive policy is missing, expected applied"])
        self.assertEqual(check(document, endpoints(), 7, "installed", "missing"), [])
        self.assertEqual(check(document, endpoints(), 7, "installed", "any"), [])
        document["driver"]["exclusive_policy"] = "partial"
        self.assertEqual(check(document, endpoints(), 7, "installed", "any"),
                         ["exclusive_policy 'partial' is not one of applied, missing, unknown"])

    def test_an_installed_driver_must_be_healthy(self):
        document = host()
        document["driver"].update(problem_code=52, service_running=False)
        self.assertEqual(check(document, endpoints(), 7, "installed", "applied"), [
            "driver problem code is 52, expected 0",
            "driver service_running is False, expected true",
        ])
        document["driver"] = {"state": "reboot-required"}
        self.assertIn("driver is reboot-required, expected installed",
                      check(document, endpoints(), 7, "installed", "applied"))

    def test_an_unknown_role_id_is_named(self):
        document = host()
        document["default_roles"]["communications"] = "{0.0.0.00000000}.{gone}"
        self.assertEqual(check(document, endpoints(), 7, "installed", "applied"),
                         ["default communications device {0.0.0.00000000}.{gone} is not among "
                          "the host outputs"])
        del document["default_roles"]["console"]
        self.assertIn("default_roles has no console",
                      check(document, endpoints(), 7, "installed", "applied"))

    def test_not_installed_allows_no_endpoint_numbers(self):
        document = host()
        document["driver"] = {"state": "not-installed"}
        problems = check(document, [], 7, "not-installed", "any")
        for endpoint, device in ((0, PRIMARY), (1, MUSIC), (2, PASS), (1, OLD_MUSIC)):
            self.assertIn(f"output {device} carries endpoint {endpoint} although the driver is "
                          "expected not installed", problems)
        self.assertFalse(any("mask" in problem for problem in problems))

        clean = host()
        clean["driver"] = {"state": "not-installed"}
        clean["outputs"] = [item for item in clean["outputs"] if item["endpoint"] is None]
        clean["default_roles"] = {"console": CABLE, "multimedia": CABLE, "communications": None}
        self.assertEqual(check(clean, [], 7, "not-installed", "any"), [])
        self.assertEqual(check(clean, [], 7, "installed", "any")[0],
                         "driver is not-installed, expected installed")

    def test_platform_facts_are_checked(self):
        document = host()
        document["platform"] = {"os_build": 18363, "arch": "x86", "test_signing": 1,
                                "secure_boot": "off"}
        self.assertEqual(check(document, endpoints(), 7, "installed", "applied"), [
            "os_build 18363 is not a build number of at least 19041",
            "arch 'x86' is not one of x64, arm64, other",
            "test_signing 1 is not a boolean",
            "secure_boot 'off' is not one of enabled, disabled, unsupported",
        ])
        for arch in ("x64", "arm64", "other"):
            for secure_boot in ("enabled", "disabled", "unsupported"):
                document["platform"] = {"os_build": 19041, "arch": arch, "test_signing": False,
                                        "secure_boot": secure_boot}
                self.assertEqual(check(document, endpoints(), 7, "installed", "applied"), [])

    def test_malformed_outputs_are_problems_not_crashes(self):
        document = host()
        document["outputs"] += [
            "x",
            {"id": "", "name": "n", "endpoint": None, "state": "active"},
            {"id": "a", "name": "n", "endpoint": True, "state": "active"},
            {"id": "b", "name": "n", "endpoint": 3, "state": "active"},
            {"id": "c", "name": "n", "endpoint": None, "state": "gone"},
        ]
        problems = check(document, endpoints(), 7, "installed", "applied")
        self.assertEqual(len(problems), 5)
        self.assertEqual(check([], endpoints(), 7, "installed", "applied"),
                         ["host document is not an object"])
        self.assertIn("mask 0x8 does not name endpoints among 0, 1, 2",
                      check(host(), endpoints(), 8, "installed", "applied"))

    def test_main_reads_powershell_encodings_and_writes_the_summary(self):
        with tempfile.TemporaryDirectory() as folder:
            folder = Path(folder)
            host_path, rows_path, summary_path = (folder / name for name in
                                                  ("host.json", "endpoints.json", "summary.json"))
            host_path.write_bytes(json.dumps(host(), ensure_ascii=False).encode("utf-16"))
            rows_path.write_text("﻿" + json.dumps(endpoints()), encoding="utf-8")
            argv = ["--host", str(host_path), "--endpoints", str(rows_path), "--expect-mask",
                    "0x7", "--expect-driver", "installed", "--expect-policy", "applied",
                    "--summary", str(summary_path)]
            self.assertEqual(main(argv), 0)
            summary = json.loads(summary_path.read_text(encoding="utf-8"))
            self.assertEqual(summary["problems"], [])
            self.assertEqual([item["id"] for item in summary["ours"]],
                             [PRIMARY, MUSIC, PASS, OLD_MUSIC])
            self.assertEqual(summary["expect"], {"mask": 7, "driver": "installed",
                                                 "policy": "applied"})
            self.assertEqual(len(summary["endpoints"]), 4)

            broken = copy.deepcopy(host())
            broken["driver"]["service_running"] = False
            host_path.write_text(json.dumps(broken), encoding="utf-8")
            self.assertEqual(main(argv), 1)
            summary = json.loads(summary_path.read_text(encoding="utf-8"))
            self.assertEqual(summary["problems"], ["driver service_running is False, expected true"])

            rows_path.write_text("{}", encoding="utf-8")
            self.assertEqual(main(argv), 1)
            summary = json.loads(summary_path.read_text(encoding="utf-8"))
            self.assertEqual(summary["problems"],
                             [f"{rows_path}: endpoint JSON must be an array"])


if __name__ == "__main__":
    unittest.main()

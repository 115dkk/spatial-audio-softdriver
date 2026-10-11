"""Unit tests for release_gate.py (python -m unittest, no dependencies)."""
import unittest

import release_gate as g


class ShipsTest(unittest.TestCase):
    def test_build_inputs_ship(self):
        for path in [
            "crates/engine/src/lib.rs",
            "apps/bio-replay/src/main.rs",
            "Cargo.toml",
            "Cargo.lock",
            "rust-toolchain.toml",
            "crates/bio-abi/include/bioatmos_abi.h",
        ]:
            self.assertTrue(g.ships(path), path)

    def test_tests_docs_policy_and_ci_do_not_ship(self):
        for path in [
            "crates/engine/tests/clock.rs",
            "crates/convolver/benches/perf.rs",
            "crates/bio-abi/fuzz/fuzz_targets/records.rs",
            "crates/engine/README.md",
            "crates/bio-policy/tests/gates.rs",
            "features.toml",
            "unsafe-budget.toml",
            "docs/adr/0001-shell.md",
            ".github/workflows/ci.yml",
            "CONTEXT.md",
            "",
        ]:
            self.assertFalse(g.ships(path), path)


class VersionTest(unittest.TestCase):
    CARGO = """[workspace]
members = ["crates/*"]

[workspace.package]
version = "0.1.0"
edition = "2024"

[workspace.dependencies]
serde = { version = "1.0.229" }
"""

    def test_workspace_version_reads_only_the_package_table(self):
        self.assertEqual(g.workspace_version(self.CARGO), "0.1.0")
        with self.assertRaises(ValueError):
            g.workspace_version("[package]\nversion = \"1.0.0\"\n")

    def test_prerelease_tag_appends_height(self):
        self.assertEqual(g.prerelease_tag("0.1.0", 42), "v0.1.0-pre.42")
        with self.assertRaises(ValueError):
            g.prerelease_tag("0.1.0-alpha.1", 3)

    def test_latest_tag_orders_numerically(self):
        tags = ["v0.1.0-pre.9", "v0.1.0-pre.10", "v0.0.9", "not-a-tag", "v0.1.0-pre.2"]
        self.assertEqual(g.latest_tag(tags), "v0.1.0-pre.10")
        self.assertEqual(g.latest_tag(tags + ["v0.1.0"]), "v0.1.0")
        self.assertEqual(g.latest_tag(tags + ["v0.2.0-pre.3"]), "v0.2.0-pre.3")
        self.assertIsNone(g.latest_tag(["x", "y"]))


class DecideTest(unittest.TestCase):
    def test_policy(self):
        self.assertTrue(g.decide(False, None, [])[0])
        self.assertTrue(g.decide(True, "v0.1.0-pre.3", [])[0])
        self.assertTrue(g.decide(False, "v0.1.0-pre.3", ["Cargo.lock"])[0])
        self.assertFalse(g.decide(False, "v0.1.0-pre.3", [])[0])


if __name__ == "__main__":
    unittest.main()

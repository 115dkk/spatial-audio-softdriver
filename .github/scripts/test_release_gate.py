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

    LOCK = """version = 4

[[package]]
name = "bio-abi"
version = "0.1.0"
dependencies = [
 "zerocopy",
]

[[package]]
name = "unrelated"
version = "0.1.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "00"
"""

    def test_workspace_version_reads_and_writes_only_the_package_table(self):
        self.assertEqual(g.workspace_version(self.CARGO), "0.1.0")
        bumped = g.set_workspace_version(self.CARGO, "0.2.0")
        self.assertEqual(g.workspace_version(bumped), "0.2.0")
        self.assertIn('serde = { version = "1.0.229" }', bumped)
        with self.assertRaises(ValueError):
            g.workspace_version('[package]\nversion = "1.0.0"\n')

    def test_lock_rewrite_leaves_registry_packages_alone(self):
        lock = g.set_lock_versions(self.LOCK, "0.1.0", "0.2.0")
        self.assertIn('name = "bio-abi"\nversion = "0.2.0"', lock)
        self.assertIn('name = "unrelated"\nversion = "0.1.0"', lock)

    def test_latest_tag_ignores_suffixed_tags(self):
        tags = ["v0.1.0-pre.11", "v0.1.9", "v0.1.10", "v0.2.0-rc.1", "x"]
        self.assertEqual(g.latest_tag(tags), "v0.1.10")
        self.assertIsNone(g.latest_tag(["v0.1.0-pre.11"]))

    def test_bump_kind_follows_commit_subjects(self):
        self.assertEqual(g.bump_kind(["fix: a", "docs: b"]), "patch")
        self.assertEqual(g.bump_kind(["fix: a", "feat(engine): b"]), "minor")
        self.assertEqual(g.bump_kind(["refactor!: drop the old API"]), "minor")
        self.assertEqual(g.bump_kind(["fix: x\n\nBREAKING CHANGE: y"]), "minor")
        self.assertEqual(g.bump_kind(["chore: feature flags"]), "patch")
        self.assertEqual(g.bump_kind([]), "patch")

    def test_prerelease_means_major_zero(self):
        self.assertTrue(g.is_prerelease("0.9.3"))
        self.assertFalse(g.is_prerelease("1.0.0"))
        with self.assertRaises(ValueError):
            g.parse_version("0.1.0-pre.11")


class DecideTest(unittest.TestCase):
    def test_untagged_version_releases_as_is(self):
        self.assertEqual(g.decide("0.1.0", {"v0.1.0-pre.11"}, [], [], False),
                         ("release", "0.1.0"))

    def test_shipped_changes_bump(self):
        tags = {"v0.1.0"}
        self.assertEqual(g.decide("0.1.0", tags, ["Cargo.lock"], ["fix: a"], False),
                         ("bump", "0.1.1"))
        self.assertEqual(g.decide("0.1.3", {"v0.1.3"}, ["crates/x/src/lib.rs"],
                                  ["feat: b"], False),
                         ("bump", "0.2.0"))

    def test_nothing_shipped_does_nothing_unless_forced(self):
        self.assertEqual(g.decide("0.1.0", {"v0.1.0"}, [], [], False), ("none", "0.1.0"))
        self.assertEqual(g.decide("0.1.0", {"v0.1.0"}, [], [], True), ("bump", "0.1.1"))

    def test_bump_skips_taken_versions(self):
        self.assertEqual(g.decide("0.1.0", {"v0.1.0", "v0.1.1"}, ["Cargo.toml"],
                                  ["fix: a"], False),
                         ("bump", "0.1.2"))


if __name__ == "__main__":
    unittest.main()

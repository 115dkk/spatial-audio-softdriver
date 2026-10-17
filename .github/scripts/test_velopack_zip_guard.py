"""Unit tests for velopack_zip_guard.py (python -m unittest, no dependencies)."""
import io
import json
import subprocess
import tempfile
import unittest
from contextlib import redirect_stdout
from pathlib import Path
from unittest import mock

import velopack_zip_guard as g


def index(*entries):
    return [json.dumps({"vers": vers, "yanked": yanked}) for vers, yanked in entries]


class NewestStableTest(unittest.TestCase):
    def test_skips_yanked_and_prereleases_and_compares_numerically(self):
        lines = index(
            ("1.2.9", False),
            ("1.2.161", False),
            ("1.2.200", True),
            ("1.3.0-g1234567", False),
            ("1.10.0", False),
        )
        self.assertEqual(g.newest_stable(lines + [""]), "1.10.0")

    def test_no_release_is_an_error(self):
        with self.assertRaises(ValueError):
            g.newest_stable(index(("1.0.0", True), ("2.0.0-pre1", False)))


class LockTest(unittest.TestCase):
    LOCK = (
        '[[package]]\nname = "zip"\nversion = "2.4.2"\n\n'
        '[[package]]\nname = "zip"\nversion = "8.6.0"\nsource = "registry"\n\n'
        '[[package]]\nname = "zip-extensions"\nversion = "8.6.0"\n'
    )

    def test_finds_every_version_of_exactly_that_crate(self):
        self.assertEqual(g.locked_versions(self.LOCK, "zip"), ["2.4.2", "8.6.0"])
        self.assertEqual(g.locked_versions(self.LOCK, "velopack"), [])


class PatchedTest(unittest.TestCase):
    def test_nine_and_later_are_patched_but_not_nines_prereleases(self):
        for version in ["9.0.0", "9.0.4", "9.1.0", "10.0.0", "9.0.0+build"]:
            self.assertTrue(g.patched(version), version)
        for version in ["8.6.0", "8.99.0", "2.4.2", "9.0.0-pre2"]:
            self.assertFalse(g.patched(version), version)

    def test_garbage_is_an_error(self):
        with self.assertRaises(ValueError):
            g.patched("nine")


class BlockedTest(unittest.TestCase):
    def test_yanked_and_unmatched_zip_block_resolution(self):
        yanked = (
            "error: failed to select a version for the requirement `zip = \"^8.6\"`\n"
            "  version 8.6.0 is yanked\n"
        )
        unmatched = (
            "error: failed to select a version for the requirement `zip = \"^8.7\"`\n"
            "candidate versions found which didn't match: 9.0.3\n"
        )
        self.assertTrue(g.blocked_by_zip(yanked))
        self.assertTrue(g.blocked_by_zip(unmatched))

    def test_other_failures_give_no_verdict(self):
        for stderr in [
            "error: failed to get `velopack` as a dependency\n  network failure",
            "error: failed to select a version for the requirement `zipper = \"^1\"`",
        ]:
            self.assertFalse(g.blocked_by_zip(stderr), stderr)


class HealEditsTest(unittest.TestCase):
    def test_removes_exactly_the_marked_block(self):
        text = (
            'yanked = "deny"\n'
            f"# {g.BEGIN}\n"
            'ignore = [{ id = "RUSTSEC-2026-0367", reason = "r" }]\n'
            f"# {g.END}\n"
            "\n[bans]\n"
        )
        self.assertEqual(g.remove_block(text, "deny.toml"), 'yanked = "deny"\n\n[bans]\n')

    def test_a_missing_or_doubled_block_is_an_error(self):
        for text in [
            "no markers\n",
            f"# {g.BEGIN}\n# {g.END}\n# {g.BEGIN}\n# {g.END}\n",
            f"# {g.END}\n# {g.BEGIN}\n",
        ]:
            with self.assertRaises(ValueError):
                g.remove_block(text, "x")

    def test_moves_only_the_velopack_requirement(self):
        manifest = 'velopack-extra = "1"\nvelopack = "1.2.161"\nzip = "2"\n'
        self.assertEqual(
            g.set_velopack(manifest, "2.0.0"),
            'velopack-extra = "1"\nvelopack = "2.0.0"\nzip = "2"\n',
        )
        with self.assertRaises(ValueError):
            g.set_velopack('velopack = { version = "1" }\n', "2.0.0")


class CheckTest(unittest.TestCase):
    """check's verdicts, with the index and cargo replaced."""

    LOCK = '[[package]]\nname = "zip"\nversion = "8.6.0"\n'
    DENY = "".join(f'{{ id = "{advisory}", reason = "r" }},\n' for advisory in g.ADVISORIES)

    def verdict(self, lock=LOCK, deny=DENY, index_error=None, resolved=None):
        with tempfile.TemporaryDirectory() as root:
            Path(root, "Cargo.lock").write_text(lock)
            Path(root, "deny.toml").write_text(deny)
            response = mock.MagicMock()
            response.__enter__.return_value.read.return_value = (
                "\n".join(index(("1.2.161", False))).encode()
            )
            urlopen = mock.Mock(side_effect=index_error, return_value=response)
            with mock.patch.object(g, "ROOT", Path(root)), mock.patch.object(
                g.urllib.request, "urlopen", urlopen
            ), mock.patch.object(g, "resolve", return_value=resolved), redirect_stdout(
                io.StringIO()
            ):
                return g.check()

    @staticmethod
    def cargo(code, stdout="", stderr=""):
        return subprocess.CompletedProcess(["cargo"], code, stdout, stderr)

    def test_a_blocked_zip_keeps_the_exception(self):
        blocked = self.cargo(101, stderr="failed to select a version for the requirement `zip = \"^8.6\"`")
        self.assertEqual(self.verdict(resolved=blocked), g.HOLDS)

    def test_a_velopack_still_on_zip_8_keeps_the_exception(self):
        for zip in ["8.6.0", "9.0.0-pre2"]:
            lock = f'[[package]]\nname = "zip"\nversion = "{zip}"\n'
            self.assertEqual(self.verdict(resolved=self.cargo(0, stdout=lock)), g.HOLDS, zip)

    def test_a_velopack_on_a_patched_zip_asks_for_the_update(self):
        lock = '[[package]]\nname = "zip"\nversion = "9.0.3"\n'
        self.assertEqual(self.verdict(resolved=self.cargo(0, stdout=lock)), g.BROKEN)

    def test_other_failures_give_no_verdict(self):
        self.assertEqual(self.verdict(index_error=OSError("offline")), g.NO_VERDICT)
        network = self.cargo(101, stderr="failed to get `velopack` as a dependency")
        self.assertEqual(self.verdict(resolved=network), g.NO_VERDICT)

    def test_an_exception_that_matches_nothing_is_stale(self):
        moved = '[[package]]\nname = "zip"\nversion = "9.0.3"\n'
        self.assertEqual(self.verdict(lock=moved), g.STALE)
        self.assertEqual(self.verdict(deny="ignore = []\n"), g.STALE)
        one_missing = self.DENY.replace(g.ADVISORIES[-1], "RUSTSEC-0000-0000")
        self.assertEqual(self.verdict(deny=one_missing), g.STALE)


class RepositoryTest(unittest.TestCase):
    """The files heal edits carry exactly what it expects."""

    def test_deny_has_one_block_and_the_manifest_one_line(self):
        deny = (g.ROOT / "deny.toml").read_text()
        g.remove_block(deny, "deny.toml")
        for advisory in g.ADVISORIES:
            self.assertIn(f'id = "{advisory}"', deny)
        g.set_velopack((g.ROOT / "Cargo.toml").read_text(), "0.0.0")
        for relative in g.GUARD_FILES:
            self.assertTrue((g.ROOT / relative).is_file(), relative)

    def test_the_workflows_stop_when_the_script_is_gone(self):
        # heal deletes the script; workflow files stay, so every step that
        # runs it must depend on the script being there.
        condition = "hashFiles('.github/scripts/velopack_zip_guard.py') != ''"
        ci = (g.ROOT / ".github/workflows/ci.yml").read_text()
        step = ci[ci.index("The zip advisory exception still stands") :]
        self.assertIn(condition, step[: step.index("run:")])
        guard = (g.ROOT / ".github/workflows/zip-advisory-guard.yml").read_text()
        self.assertIn(condition, guard)
        self.assertIn("gh workflow disable zip-advisory-guard.yml", guard)


if __name__ == "__main__":
    unittest.main()

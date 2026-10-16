#!/usr/bin/env python3
"""Proof behind deny.toml's exception for the yanked zip 8.6.0, and its cure.

The zip maintainer yanked every release but the newest after three extraction
advisories (October 2026). velopack, the app's updater, needs ``zip ^8.6``, so
Cargo.lock keeps the yanked 8.6.0 and cargo-deny's ``yanked = "deny"`` fails.
deny.toml excuses exactly that version, and the excuse holds only while it is
true that the newest velopack cannot be built with a zip that is not yanked.

``check`` proves it with cargo's own resolver: a scratch crate that depends on
the newest stable velopack only, ``cargo generate-lockfile``.

* resolution fails on velopack's zip requirement   -> exit 0, the exception
                                                       stands
* resolution succeeds (the zip it picks is never    -> exit 3, velopack can
  yanked: cargo skips yanked versions)                 now leave 8.6.0 behind
* Cargo.lock or deny.toml no longer match the       -> exit 1, the exception
  exception                                            is stale
* anything else (network, index)                    -> exit 2, no verdict

``heal`` is what velopack-zip-guard.yml runs on exit 3: it moves the velopack
requirement to the version ``check`` found, removes the exception from
deny.toml and deletes this script and its tests. The workflow then updates
Cargo.lock, runs cargo-deny without the exception, opens the pull request
and disables itself. It cannot delete workflow files (a push with the
Actions token may not touch .github/workflows), so ci.yml's proof step and
velopack-zip-guard.yml run only while this script exists: deleting it turns
both off.

The pure functions are unit-tested in ``test_velopack_zip_guard.py``.
"""
from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
import tempfile
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
EXCEPTED_ZIP = "8.6.0"
INDEX = "https://index.crates.io/ve/lo/velopack"
BEGIN = "velopack-zip-guard: begin"
END = "velopack-zip-guard: end"
GUARD_FILES = (
    ".github/scripts/velopack_zip_guard.py",
    ".github/scripts/test_velopack_zip_guard.py",
)

HOLDS, STALE, NO_VERDICT, BROKEN = 0, 1, 2, 3


def stable_key(version: str) -> tuple[int, int, int] | None:
    """(major, minor, patch) of a release version; None for a prerelease."""
    match = re.fullmatch(r"(\d+)\.(\d+)\.(\d+)", version)
    return tuple(int(part) for part in match.groups()) if match else None


def newest_stable(index_lines: list[str]) -> str:
    """The newest version in a sparse-index file that is neither yanked nor
    a prerelease."""
    best: tuple[tuple[int, int, int], str] | None = None
    for line in index_lines:
        if not line.strip():
            continue
        entry = json.loads(line)
        key = stable_key(entry["vers"])
        if entry.get("yanked") or key is None:
            continue
        if best is None or key > best[0]:
            best = (key, entry["vers"])
    if best is None:
        raise ValueError("no stable, unyanked velopack release in the index")
    return best[1]


def locked_versions(lock: str, name: str) -> list[str]:
    """Every version of ``name`` in a Cargo.lock."""
    return re.findall(
        r'^\[\[package\]\]\nname = "' + re.escape(name) + r'"\nversion = "([^"]+)"',
        lock,
        re.MULTILINE,
    )


def blocked_by_zip(stderr: str) -> bool:
    """cargo could not pick any zip for velopack: every candidate is yanked
    or none matches the requirement."""
    return "failed to select a version for the requirement `zip " in stderr


def remove_block(text: str, path: str) -> str:
    """Drop the lines from the BEGIN marker to the END marker, inclusive."""
    lines = text.splitlines(keepends=True)
    starts = [i for i, line in enumerate(lines) if BEGIN in line]
    ends = [i for i, line in enumerate(lines) if END in line]
    if len(starts) != 1 or len(ends) != 1 or ends[0] < starts[0]:
        raise ValueError(f"{path}: expected one {BEGIN!r} ... {END!r} block")
    return "".join(lines[: starts[0]] + lines[ends[0] + 1 :])


def set_velopack(manifest: str, version: str) -> str:
    """Point the workspace's velopack requirement at ``version``."""
    new, count = re.subn(
        r'^velopack = "[^"]+"$', f'velopack = "{version}"', manifest, flags=re.MULTILINE
    )
    if count != 1:
        raise ValueError('Cargo.toml: expected one line velopack = "..."')
    return new


def resolve(velopack: str) -> subprocess.CompletedProcess[str]:
    """Resolve a crate that depends on ``velopack`` and nothing else."""
    with tempfile.TemporaryDirectory(prefix="velopack-zip-guard-") as scratch:
        root = Path(scratch)
        (root / "src").mkdir()
        (root / "src" / "lib.rs").write_text("")
        (root / "Cargo.toml").write_text(
            '[package]\nname = "velopack-zip-guard"\nversion = "0.0.0"\nedition = "2021"\n\n'
            f'[dependencies]\nvelopack = "={velopack}"\n'
        )
        result = subprocess.run(
            ["cargo", "generate-lockfile"],
            cwd=root,
            capture_output=True,
            text=True,
            check=False,
        )
        if result.returncode == 0:
            result.stdout = (root / "Cargo.lock").read_text()
        return result


def output(name: str, value: str) -> None:
    path = os.environ.get("GITHUB_OUTPUT")
    if path:
        with open(path, "a", encoding="utf-8") as handle:
            handle.write(f"{name}={value}\n")


def check() -> int:
    lock = (ROOT / "Cargo.lock").read_text()
    deny = (ROOT / "deny.toml").read_text()
    if EXCEPTED_ZIP not in locked_versions(lock, "zip"):
        print(f"Cargo.lock no longer has zip {EXCEPTED_ZIP}: remove the exception from deny.toml.")
        return STALE
    if f'crate = "zip@{EXCEPTED_ZIP}"' not in deny:
        print(f"deny.toml does not excuse zip@{EXCEPTED_ZIP}: this guard has nothing to prove.")
        return STALE

    try:
        with urllib.request.urlopen(INDEX, timeout=60) as response:
            velopack = newest_stable(response.read().decode().splitlines())
    except Exception as error:  # noqa: BLE001 - any failure is "no verdict"
        print(f"could not read the velopack index: {error}")
        return NO_VERDICT
    output("velopack", velopack)

    result = resolve(velopack)
    if result.returncode != 0:
        if blocked_by_zip(result.stderr):
            print(
                f"velopack {velopack} (the newest) cannot be built with a zip that is not "
                f"yanked, so the exception for zip {EXCEPTED_ZIP} stands.\n{result.stderr.strip()}"
            )
            return HOLDS
        print(f"cargo could not resolve velopack {velopack}:\n{result.stderr.strip()}")
        return NO_VERDICT

    zips = locked_versions(result.stdout, "zip")
    output("zip", ",".join(zips))
    print(
        f"velopack {velopack} resolves with zip {', '.join(zips) or '(none)'}, which is not "
        f"yanked: the exception for zip {EXCEPTED_ZIP} is no longer needed. "
        "velopack-zip-guard.yml updates velopack, removes the exception and this guard, and "
        "opens the pull request from auto/velopack-zip-guard; merge it."
    )
    return BROKEN


def heal(velopack: str) -> int:
    edits = {
        "Cargo.toml": lambda text: set_velopack(text, velopack),
        "deny.toml": lambda text: remove_block(text, "deny.toml"),
    }
    for relative, edit in edits.items():
        path = ROOT / relative
        path.write_text(edit(path.read_text()))
    for relative in GUARD_FILES:
        (ROOT / relative).unlink()
    print(f"velopack -> {velopack}; the zip {EXCEPTED_ZIP} exception and its guard are removed.")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    commands = parser.add_subparsers(dest="command", required=True)
    commands.add_parser("check", help="prove the exception still stands")
    healing = commands.add_parser("heal", help="update velopack and remove the guard")
    healing.add_argument("--velopack", required=True)
    args = parser.parse_args()
    if args.command == "check":
        return check()
    return heal(args.velopack)


if __name__ == "__main__":
    sys.exit(main())

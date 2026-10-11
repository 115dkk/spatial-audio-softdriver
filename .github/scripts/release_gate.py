#!/usr/bin/env python3
"""Release gate of release.yml: decides whether a push to master publishes.

Every release is a GitHub prerelease for now (until the app reaches users in
M5). The tag is ``v<workspace version>-pre.<height>``, where ``height`` is the
number of commits reachable from the released commit, so a commit always maps
to the same tag and no bot commit is needed.

Decision policy:

* ``--force`` (a manual dispatch asked for it)      -> release
* no earlier ``v*`` tag                              -> release
* shipped files changed since the newest ``v*`` tag -> release
* otherwise                                          -> do nothing

A file ships when it is a build input of the released binaries: ``SHIP``
patterns minus ``NOT_SHIPPED`` and anything under a ``tests``, ``benches`` or
``fuzz`` directory.

The pure functions are unit-tested in ``test_release_gate.py``; ``main`` wires
them to git and ``$GITHUB_OUTPUT``.
"""
from __future__ import annotations

import argparse
import fnmatch
import os
import re
import subprocess
from pathlib import Path

SHIP = (
    "crates/*",
    "apps/*",
    "Cargo.toml",
    "Cargo.lock",
    "rust-toolchain.toml",
    "LICENSE",
)
NOT_SHIPPED = ("*.md", "crates/bio-policy/*")
NOT_SHIPPED_DIRS = ("tests", "benches", "fuzz")

TAG_RE = re.compile(r"^v\d+\.\d+\.\d+(?:-[0-9A-Za-z.]+)?$")


def _matches(path: str, patterns) -> bool:
    base = path.rsplit("/", 1)[-1]
    for pat in patterns:
        if pat.endswith("/*"):
            prefix = pat[:-2]
            if path == prefix or path.startswith(prefix + "/"):
                return True
        elif fnmatch.fnmatch(base, pat) or fnmatch.fnmatch(path, pat):
            return True
    return False


def ships(path: str) -> bool:
    if not path or not _matches(path, SHIP) or _matches(path, NOT_SHIPPED):
        return False
    return not any(part in NOT_SHIPPED_DIRS for part in path.split("/")[:-1])


def shipped_files(changed) -> list:
    return [f for f in changed if ships(f)]


def workspace_version(cargo_toml: str) -> str:
    """``[workspace.package].version`` of the root Cargo.toml."""
    section = None
    for line in cargo_toml.splitlines():
        header = re.match(r"^\s*\[([^\]]+)\]\s*$", line)
        if header:
            section = header.group(1).strip()
            continue
        m = re.match(r'^\s*version\s*=\s*"([^"]+)"', line)
        if m and section == "workspace.package":
            return m.group(1)
    raise ValueError("no [workspace.package] version in Cargo.toml")


def prerelease_tag(version: str, height: int) -> str:
    if "-" in version:
        raise ValueError(
            f"workspace version {version} already carries a suffix; "
            "the gate appends -pre.<height> itself"
        )
    return f"v{version}-pre.{height}"


def version_key(tag: str):
    """Sort key for release tags: numeric core, then height (stable last)."""
    m = re.match(r"^v(\d+)\.(\d+)\.(\d+)(?:-pre\.(\d+))?", tag)
    if not m:
        return (-1, -1, -1, -1)
    major, minor, patch, height = m.groups()
    return (int(major), int(minor), int(patch),
            int(height) if height is not None else 1 << 62)


def latest_tag(tags) -> str | None:
    valid = [t for t in tags if TAG_RE.match(t)]
    return max(valid, key=version_key) if valid else None


def decide(force: bool, previous: str | None, shipped: list) -> tuple[bool, str]:
    if force:
        return True, "forced by dispatch"
    if previous is None:
        return True, "first release"
    if shipped:
        return True, f"{len(shipped)} shipped file(s) changed since {previous}"
    return False, f"no shipped file changed since {previous}"


def _git(*args) -> str:
    out = subprocess.run(
        ["git", *args], check=True, capture_output=True, text=True,
        encoding="utf-8", errors="replace",
    )
    return out.stdout.strip()


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--force", action="store_true")
    args = parser.parse_args(argv)

    head = os.environ.get("GITHUB_SHA") or _git("rev-parse", "HEAD")
    subprocess.run(["git", "fetch", "--tags", "--force", "--quiet", "origin"],
                   check=False)
    previous = latest_tag(_git("tag", "-l", "v*").splitlines())
    changed = (_git("diff", "--name-only", previous, head).splitlines()
               if previous else [])
    shipped = shipped_files(changed)
    version = workspace_version(Path("Cargo.toml").read_text(encoding="utf-8"))
    height = int(_git("rev-list", "--count", head))
    tag = prerelease_tag(version, height)
    should, reason = decide(args.force, previous, shipped)

    print(f"head={head[:12]} version={version} height={height} previous={previous}")
    print(f"shipped changes: {shipped[:20]}")
    print(f"decision: release={should} ({reason}) tag={tag}")

    payload = (
        f"should_release={'true' if should else 'false'}\n"
        f"version={tag[1:]}\n"
        f"tag={tag}\n"
        f"previous_tag={previous or ''}\n"
        f"release_sha={head}\n"
    )
    out = os.environ.get("GITHUB_OUTPUT")
    if out:
        with open(out, "a", encoding="utf-8") as fh:
            fh.write(payload)
    print(payload, end="")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

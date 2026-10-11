#!/usr/bin/env python3
"""Release gate of release.yml: decides whether a push to master publishes,
and with which version (ADR 0017).

Versions are plain semver tags ``v<major>.<minor>.<patch>``. While the major
version is 0 every release is a GitHub prerelease; 0.x itself is what says
"prerelease", so no suffix is ever appended. 1.0 is declared by hand.

Decision policy:

* the workspace version has no tag yet             -> release it as is
                                                      (first release, or a
                                                      manual bump)
* tagged, shipped files changed since that tag     -> bump, commit the bump to
                                                      master, release it
* tagged, nothing shipped changed                  -> do nothing
  (``--force`` from a manual dispatch bumps anyway)

The bump is minor when a commit that touched shipped files has a ``feat``
subject (``feat:``, ``feat(scope):``, ``feat!:``) or says ``BREAKING CHANGE``,
and patch otherwise. Before 1.0 a breaking change is still a minor bump.

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
import time
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

TAG_RE = re.compile(r"^v(\d+)\.(\d+)\.(\d+)$")
MINOR_SUBJECT_RE = re.compile(r"^(feat|minor)(\([^)]*\))?!?:")
BREAKING_RE = re.compile(r"^[a-z]+(\([^)]*\))?!:|BREAKING[ -]CHANGE", re.MULTILINE)

CARGO_TOML = Path("Cargo.toml")
CARGO_LOCK = Path("Cargo.lock")


# ---------------------------------------------------------------------------
# Pure logic (unit-tested)
# ---------------------------------------------------------------------------
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


def parse_version(v: str) -> tuple[int, int, int]:
    m = re.match(r"^(\d+)\.(\d+)\.(\d+)$", v.strip())
    if not m:
        raise ValueError(f"not a plain major.minor.patch version: {v!r}")
    return tuple(int(x) for x in m.groups())


def latest_tag(tags) -> str | None:
    """Highest plain ``vX.Y.Z`` tag; suffixed tags are ignored."""
    valid = [t for t in tags if TAG_RE.match(t)]
    return max(valid, key=lambda t: parse_version(t[1:])) if valid else None


def bump_kind(messages) -> str:
    """``minor`` or ``patch`` from the full messages of the shipped commits."""
    for msg in messages:
        subject = msg.strip().splitlines()[0] if msg.strip() else ""
        if MINOR_SUBJECT_RE.match(subject) or BREAKING_RE.search(msg):
            return "minor"
    return "patch"


def bumped(version: str, kind: str) -> str:
    major, minor, patch = parse_version(version)
    if kind == "minor":
        return f"{major}.{minor + 1}.0"
    return f"{major}.{minor}.{patch + 1}"


def decide(version: str, tags, shipped: list, messages, force: bool):
    """Return ``(action, version)``: action is ``release``, ``bump`` or ``none``."""
    if f"v{version}" not in tags:
        return "release", version
    if not shipped and not force:
        return "none", version
    candidate = bumped(version, bump_kind(messages))
    while f"v{candidate}" in tags:
        candidate = bumped(candidate, "patch")
    return "bump", candidate


def is_prerelease(version: str) -> bool:
    return parse_version(version)[0] == 0


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


def set_workspace_version(cargo_toml: str, new: str) -> str:
    lines = cargo_toml.splitlines(keepends=True)
    section = None
    for i, line in enumerate(lines):
        header = re.match(r"^\s*\[([^\]]+)\]\s*$", line)
        if header:
            section = header.group(1).strip()
            continue
        if section == "workspace.package" and re.match(r'^\s*version\s*=\s*"', line):
            lines[i] = re.sub(r'"[^"]+"', f'"{new}"', line, count=1)
            return "".join(lines)
    raise ValueError("no [workspace.package] version in Cargo.toml")


def set_lock_versions(lock: str, old: str, new: str) -> str:
    """Rewrite ``old`` to ``new`` for the workspace packages of a Cargo.lock.

    Registry and git packages carry a ``source`` line right after ``version``;
    workspace members do not, so a dependency that happens to share the
    version is left alone.
    """
    pattern = re.compile(
        r'(?m)^(\[\[package\]\]\r?\nname = "[^"]+"\r?\nversion = ")'
        + re.escape(old) + r'("\r?\n)(?!source = )'
    )
    new_lock, n = pattern.subn(lambda m: m.group(1) + new + m.group(2), lock)
    if n == 0:
        raise ValueError(f"no workspace package at {old} in Cargo.lock")
    return new_lock


# ---------------------------------------------------------------------------
# I/O wrapper (runs in CI)
# ---------------------------------------------------------------------------
def _run(*args, check=True):
    return subprocess.run(
        list(args), check=check, capture_output=True, text=True,
        encoding="utf-8", errors="replace",
    )


def _git(*args) -> str:
    return _run("git", *args).stdout.strip()


def _read(path: Path) -> str:
    with open(path, encoding="utf-8", newline="") as fh:
        return fh.read()


def _write(path: Path, text: str) -> None:
    with open(path, "w", encoding="utf-8", newline="") as fh:
        fh.write(text)


def _commit_bump(old: str, new: str) -> str:
    _write(CARGO_TOML, set_workspace_version(_read(CARGO_TOML), new))
    _write(CARGO_LOCK, set_lock_versions(_read(CARGO_LOCK), old, new))
    _run("git", "config", "user.name", "github-actions[bot]")
    _run("git", "config", "user.email",
         "41898282+github-actions[bot]@users.noreply.github.com")
    _run("git", "add", str(CARGO_TOML), str(CARGO_LOCK))
    _run("git", "commit", "-m", f"chore(release): v{new}")
    for attempt in range(1, 4):
        # A push made with the workflow token starts no workflow, so the bump
        # commit does not release itself.
        push = _run("git", "push", "origin", "HEAD:master", check=False)
        if push.returncode == 0:
            return _git("rev-parse", "HEAD")
        print(f"::warning::bump push rejected (attempt {attempt})\n{push.stderr}")
        _run("git", "fetch", "origin", "master", check=False)
        if _run("git", "rebase", "FETCH_HEAD", check=False).returncode != 0:
            _run("git", "rebase", "--abort", check=False)
            raise SystemExit("master moved in a way the bump cannot rebase onto")
        time.sleep(5)
    raise SystemExit("bump push failed after 3 attempts")


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--force", action="store_true",
                        help="bump and release even when nothing shipped changed")
    args = parser.parse_args(argv)

    head = os.environ.get("GITHUB_SHA") or _git("rev-parse", "HEAD")
    _run("git", "fetch", "--tags", "--force", "--quiet", "origin", check=False)
    tags = set(_git("tag", "-l", "v*").splitlines())
    version = workspace_version(_read(CARGO_TOML))
    current_tag = f"v{version}"
    previous = latest_tag(tags)

    shipped, messages = [], []
    if current_tag in tags:
        shipped = shipped_files(_git("diff", "--name-only", current_tag, head).splitlines())
        if shipped:
            log = _git("log", "--no-merges", "--format=%B%x00", f"{current_tag}..{head}",
                       "--", *shipped)
            messages = [m for m in log.split("\x00") if m.strip()]

    action, new_version = decide(version, tags, shipped, messages, args.force)
    print(f"head={head[:12]} version={version} tagged={current_tag in tags} "
          f"previous={previous}")
    print(f"shipped changes: {shipped[:20]}")
    print(f"decision: {action} {new_version}")

    release_sha = head
    if action == "bump":
        if os.environ.get("GITHUB_REF", "refs/heads/master") != "refs/heads/master":
            raise SystemExit("a bump commits to master; dispatch on master")
        release_sha = _commit_bump(version, new_version)
        print(f"bumped to v{new_version}; release_sha={release_sha}")

    payload = (
        f"should_release={'false' if action == 'none' else 'true'}\n"
        f"version={new_version}\n"
        f"tag=v{new_version}\n"
        f"prerelease={'true' if is_prerelease(new_version) else 'false'}\n"
        f"previous_tag={previous or ''}\n"
        f"release_sha={release_sha}\n"
    )
    out = os.environ.get("GITHUB_OUTPUT")
    if out:
        with open(out, "a", encoding="utf-8") as fh:
            fh.write(payload)
    print(payload, end="")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

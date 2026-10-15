"""Round-trips the default device roles through ``ssr-live host``.

Reads ``host show``, picks our primary endpoint (the active output with
``endpoint`` 0) and remembers the default device of each role. For console,
multimedia and communications in turn, ``host set-default`` makes ours the
default and ``host show`` must then report it. Afterwards every role goes back
to the device it had; a role that had none is left as it is, with a note, since
Windows offers no way to clear a default. ``host show`` must then report the
original ids. This is the ``IPolicyConfig`` path the supervisor and
``ssr-guard`` use to take the roles and give them back (ADR 0023, ADR 0032).

The summary JSON holds ``before``, ``after``, ``ours``, ``problems`` and
``notes``; the exit code is 1 when there is any problem.
"""

import argparse
import json
from pathlib import Path
import subprocess
import sys

from ssr_abi import ENDPOINT_PRIMARY

ROLES = ("console", "multimedia", "communications")


class RunError(Exception):
    """``ssr-live`` failed, or printed no JSON object."""


def same_id(left, right):
    if left is None or right is None:
        return left is right
    return left.casefold() == right.casefold()


def roles_of(show):
    """``default_roles`` of a ``host show`` document: each role's id or None."""
    roles = show.get("default_roles") if isinstance(show, dict) else None
    if not isinstance(roles, dict):
        raise ValueError("host show has no default_roles")
    found = {}
    for role in ROLES:
        if role not in roles:
            raise ValueError(f"default_roles has no {role}")
        device = roles[role]
        if device is not None and (not isinstance(device, str) or not device):
            raise ValueError(f"default {role} device {device!r} is not an endpoint id")
        found[role] = device
    return found


def our_id(show):
    """The id of the one active output that carries our primary endpoint."""
    outputs = show.get("outputs") if isinstance(show, dict) else None
    if not isinstance(outputs, list):
        raise ValueError("host show has no outputs")
    ours = [output for output in outputs if isinstance(output, dict)
            and output.get("endpoint") == ENDPOINT_PRIMARY and output.get("state") == "active"]
    if len(ours) != 1 or not isinstance(ours[0].get("id"), str) or not ours[0]["id"]:
        raise ValueError(f"{len(ours)} active outputs carry endpoint {ENDPOINT_PRIMARY}, "
                         "expected exactly one")
    return ours[0]["id"]


def set_default(run, role, device):
    answer = run(["host", "set-default", "--role", role, "--id", device])
    if answer.get("ok") is not True or answer.get("role") != role:
        raise RunError(f"set-default answered {answer}")


def round_trip(run):
    """Drives ``run(args) -> dict`` (one ``ssr-live`` call, its stdout parsed)
    through the round trip and returns the summary."""
    summary = {"before": None, "after": None, "ours": None, "problems": [], "notes": []}
    problems, notes = summary["problems"], summary["notes"]
    try:
        first = run(["host", "show"])
        before = roles_of(first)
        ours = our_id(first)
    except (RunError, ValueError) as error:
        problems.append(f"host show: {error}")
        return summary
    summary["before"], summary["ours"] = before, ours

    for role in ROLES:
        try:
            set_default(run, role, ours)
            shown = roles_of(run(["host", "show"]))[role]
        except (RunError, ValueError) as error:
            problems.append(f"{role}: {error}")
            continue
        if not same_id(shown, ours):
            problems.append(f"{role}: host show reports {shown} after set-default, not ours {ours}")

    for role in ROLES:
        original = before[role]
        if original is None:
            notes.append(f"{role} had no default device before; left as it is")
            continue
        try:
            set_default(run, role, original)
        except RunError as error:
            problems.append(f"{role}: restoring {original}: {error}")

    try:
        after = roles_of(run(["host", "show"]))
    except (RunError, ValueError) as error:
        problems.append(f"host show after restoring: {error}")
        return summary
    summary["after"] = after
    for role in ROLES:
        if before[role] is not None and not same_id(after[role], before[role]):
            problems.append(f"{role}: host show reports {after[role]} after restoring, "
                            f"not {before[role]}")
    return summary


def make_run(ssr_live, state_dir=None, timeout=120.0):
    """``run(args)`` for the real binary: ``ssr-live <args> [--state-dir DIR]``."""
    def run(args):
        command = [str(ssr_live), *args]
        if state_dir:
            command += ["--state-dir", str(state_dir)]
        name = " ".join(args[:2])
        try:
            result = subprocess.run(command, capture_output=True, encoding="utf-8",
                                    errors="replace", timeout=timeout)
        except (OSError, subprocess.TimeoutExpired) as error:
            raise RunError(f"{name}: {error}") from error
        if result.returncode != 0:
            raise RunError(f"{name} exited with {result.returncode}: {result.stderr.strip()}")
        try:
            document = json.loads(result.stdout)
        except ValueError as error:
            raise RunError(f"{name} printed no JSON: {error}") from error
        if not isinstance(document, dict):
            raise RunError(f"{name} printed {type(document).__name__}, not an object")
        return document
    return run


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--ssr-live", required=True, help="the ssr-live executable")
    parser.add_argument("--summary", required=True, type=Path)
    parser.add_argument("--state-dir", help="passed on to every ssr-live host call")
    args = parser.parse_args(argv)
    summary = round_trip(make_run(args.ssr_live, args.state_dir))
    text = json.dumps(summary, indent=2)
    args.summary.write_text(text, encoding="utf-8")
    print(text)
    for problem in summary["problems"]:
        print(problem, file=sys.stderr)
    return 1 if summary["problems"] else 0


if __name__ == "__main__":
    raise SystemExit(main())

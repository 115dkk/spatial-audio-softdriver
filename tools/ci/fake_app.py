#!/usr/bin/env python3
"""Fake ADR 0033 pipe peer used by the Windows guard check."""

import argparse
import json
import os
import sys
import time
from pathlib import Path

from host_role_check import ROLES, make_run, roles_of, same_id, set_default


def heartbeat(value):
    print(json.dumps({"running": True, "output_periods": value, "capture_reads": value}, separators=(",", ":")), flush=True)


def restore_roles(run, point):
    ours = point["ours"]
    fallback = point.get("fallback")
    current = roles_of(run(["host", "show"]))
    for role in ROLES:
        if not same_id(current[role], ours):
            continue
        previous = point["previous"].get(role)
        target = fallback if previous is None or same_id(previous, ours) else previous
        if target is not None:
            set_default(run, role, target)


def take_over(run, restore_path, restore_text, ours):
    point = json.loads(restore_text)
    point["app_pid"] = os.getpid()
    restore_path.write_text(json.dumps(point, separators=(",", ":")), encoding="utf-8")
    for role in ROLES:
        set_default(run, role, ours)


def main(argv=None):
    parser = argparse.ArgumentParser()
    parser.add_argument("mode", choices=("exit", "stall", "exit-clean"))
    parser.add_argument("--guarded", action="store_true")
    parser.add_argument("--resume", action="store_true")
    parser.add_argument("--minimized", action="store_true")
    parser.add_argument("--interval", type=float, default=1.0)
    parser.add_argument("--stall-seconds", type=float, default=60.0)
    args = parser.parse_args(argv)
    state_dir = Path(os.environ.get("SSR_FAKE_STATE_DIR",
                                    os.environ.get("SSR_GUARD_STATE_DIR", os.getcwd())))
    state_dir.mkdir(parents=True, exist_ok=True)
    restore_path = state_dir / "restore-point.json"
    live = os.environ.get("SSR_FAKE_LIVE")
    ours = os.environ.get("SSR_FAKE_OURS")
    restore_text = os.environ.get("SSR_FAKE_RESTORE_POINT")
    run = make_run(live, state_dir) if live else None
    roles_at_start = roles_of(run(["host", "show"])) if args.resume and run else None
    launch = {
        "pid": os.getpid(),
        "mode": args.mode,
        "guarded": args.guarded,
        "resume": args.resume,
        "minimized": args.minimized,
        "roles_at_start": roles_at_start,
        "argv": [flag for flag, present in (
            ("--guarded", args.guarded),
            ("--resume", args.resume),
            ("--minimized", args.minimized),
        ) if present],
    }
    with (state_dir / "fake_app.log").open("a", encoding="utf-8") as stream:
        stream.write(json.dumps(launch, separators=(",", ":")) + "\n")
    if args.resume and run is not None:
        if not ours or not restore_text:
            raise ValueError("resumed fake app needs SSR_FAKE_OURS and SSR_FAKE_RESTORE_POINT")
        take_over(run, restore_path, restore_text, ours)

    for value in range(1, 4):
        heartbeat(value)
        time.sleep(args.interval)
    if args.mode == "exit-clean":
        if run is not None and restore_text:
            restore_roles(run, json.loads(restore_text))
        try:
            restore_path.unlink()
        except FileNotFoundError:
            pass
        return 0
    if args.mode == "exit":
        return 0
    started = time.monotonic()
    while time.monotonic() - started < args.stall_seconds:
        time.sleep(max(args.interval, 0.01))
    return 18


if __name__ == "__main__":
    sys.exit(main())

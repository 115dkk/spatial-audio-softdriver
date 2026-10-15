#!/usr/bin/env python3
"""Exercise ssr-guard exit, stall/restart, and clean-exit scenarios on Windows."""

import argparse
import json
import os
from pathlib import Path
import subprocess
import sys

from host_role_check import ROLES, RunError, make_run, our_id, roles_of, same_id, set_default


def verdict(returncode, marker, restore_exists, after, expected_roles, reason,
            timed_out=False, cleanup_failed=False):
    problems = []
    expected_code = 0 if reason is None else 1
    if returncode != expected_code:
        problems.append(f"guard exited {returncode}, expected {expected_code}")
    if timed_out:
        problems.append("guard timed out")
    if cleanup_failed:
        problems.append("the guard's process tree could not be ended")
    if restore_exists:
        problems.append("restore-point.json remains")
    if reason is None:
        if marker is not None:
            problems.append("clean exit wrote a marker")
    elif not isinstance(marker, dict) or marker.get("reason") != reason:
        actual = marker.get("reason") if isinstance(marker, dict) else marker
        problems.append(f"marker reason is {actual!r}, expected {reason!r}")
    for role in ROLES:
        if not same_id(after.get(role), expected_roles.get(role)):
            problems.append(f"{role} is {after.get(role)!r}, expected {expected_roles.get(role)!r}")
    return problems


def fallback_id(show, ours):
    outputs = show.get("outputs", []) if isinstance(show, dict) else []
    candidates = [item for item in outputs if isinstance(item, dict)
                  and item.get("state") == "active" and isinstance(item.get("id"), str)
                  and not same_id(item["id"], ours)]
    cables = [item for item in candidates
              if "VB-CABLE" in str(item.get("name", "")).upper()
              or "CABLE INPUT" in str(item.get("name", "")).upper()
              or "VB-AUDIO VIRTUAL CABLE" in str(item.get("name", "")).upper()]
    if cables:
        return cables[0]["id"]
    raise ValueError("no active VB-CABLE render output is available as fallback")


def baseline_roles(original, ours, fallback):
    return {role: fallback if device is None or same_id(device, ours) else device
            for role, device in original.items()}


def launch_verdict(mode, launched, expected_roles=None):
    problems = []
    if mode == "stall":
        if len(launched) != 2 or launched[0].get("resume") or not launched[1].get("resume"):
            problems.append("stall did not launch once normally and once with --resume")
        elif expected_roles is not None:
            roles_at_start = launched[1].get("roles_at_start")
            if not isinstance(roles_at_start, dict):
                problems.append("resumed fake app did not record roles_at_start")
            else:
                for role in ROLES:
                    if not same_id(roles_at_start.get(role), expected_roles.get(role)):
                        problems.append(
                            f"resumed {role} started as {roles_at_start.get(role)!r}, "
                            f"expected {expected_roles.get(role)!r}")
    elif len(launched) != 1 or launched[0].get("resume"):
        problems.append(f"{mode} did not launch exactly once without --resume")
    if any(not item.get("guarded") for item in launched):
        problems.append("a fake app launch lacked --guarded")
    return problems


def set_roles(run, roles, device):
    for role in roles:
        set_default(run, role, device)


def read_json(path):
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except FileNotFoundError:
        return None


def launches(path):
    try:
        return [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines() if line]
    except FileNotFoundError:
        return []


def kill_tree(pid):
    """Ends a process and everything it started. Errors are swallowed: the
    caller checks whether the process is gone."""
    if os.name == "nt":
        try:
            subprocess.run(["taskkill", "/T", "/F", "/PID", str(pid)],
                           capture_output=True, timeout=15, check=False)
        except (OSError, subprocess.SubprocessError):
            pass
    else:
        try:
            os.kill(pid, 9)
        except OSError:
            pass


def kill_pids(pids):
    """Ends the processes the fake app recorded, after a timeout left them."""
    for pid in pids:
        if isinstance(pid, int) and pid > 0:
            kill_tree(pid)


def run_guard_process(command, env, output_dir, timeout=30):
    """Runs the guard with its stdout and stderr in files, never pipes: the
    fake app inherits the guard's stderr, and a pipe a grandchild keeps open
    would leave the harness waiting for it after the guard is gone."""
    output_dir.mkdir(parents=True, exist_ok=True)
    out_path = output_dir / "guard-stdout.txt"
    err_path = output_dir / "guard-stderr.txt"
    timed_out = False
    cleanup_failed = False
    with out_path.open("w", encoding="utf-8") as out, err_path.open("w", encoding="utf-8") as err:
        process = subprocess.Popen(command, stdout=out, stderr=err, stdin=subprocess.DEVNULL,
                                   env=env)
        try:
            process.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            timed_out = True
            kill_tree(process.pid)
            if process.poll() is None:
                process.kill()
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                cleanup_failed = True
    stdout = out_path.read_text(encoding="utf-8", errors="replace")
    stderr = err_path.read_text(encoding="utf-8", errors="replace")
    result = subprocess.CompletedProcess(command, process.returncode, stdout, stderr)
    result.timed_out = timed_out
    result.cleanup_failed = cleanup_failed
    return result


def check(run_host, run_guard, state_dir, ssr_live=None):
    summary = {"scenarios": {}, "problems": []}
    problems = summary["problems"]
    try:
        show = run_host(["host", "show"])
        original = roles_of(show)
        ours = our_id(show)
        fallback = fallback_id(show, ours)
        baseline = baseline_roles(original, ours, fallback)
    except (RunError, ValueError) as error:
        problems.append(f"host show: {error}")
        return summary
    summary.update({
        "ours": ours,
        "fallback": fallback,
        "original_roles": original,
        "baseline_roles": baseline,
    })
    marker_path = state_dir / "guard-restored.json"
    restore_path = state_dir / "restore-point.json"
    log_path = state_dir / "fake_app.log"
    try:
        for mode, reason in (("exit", "app_exited"), ("stall", "restart_failed"),
                             ("exit-clean", None)):
            for role, device in baseline.items():
                set_default(run_host, role, device)
            before = roles_of(run_host(["host", "show"]))
            for path in (marker_path, restore_path, log_path):
                try:
                    path.unlink()
                except FileNotFoundError:
                    pass
            restore_point = {
                "version": 1,
                "ours": ours,
                "previous": before,
                "fallback": fallback,
                "app_pid": 0,
            }
            restore_text = json.dumps(restore_point, separators=(",", ":"))
            restore_path.write_text(restore_text, encoding="utf-8")
            set_roles(run_host, ROLES, ours)
            fake_env = {
                "SSR_FAKE_OURS": ours,
                "SSR_FAKE_RESTORE_POINT": restore_text,
                "SSR_FAKE_STATE_DIR": str(state_dir),
            }
            if ssr_live is not None:
                fake_env["SSR_FAKE_LIVE"] = str(ssr_live)
            result = run_guard(mode, fake_env)
            launched = launches(log_path)
            timed_out = bool(getattr(result, "timed_out", False))
            cleanup_failed = bool(getattr(result, "cleanup_failed", False))
            if timed_out:
                # Whatever the guard left running must not take the roles
                # again under the checks and the cleanup below.
                kill_pids(launch.get("pid") for launch in launched)
            after = roles_of(run_host(["host", "show"]))
            marker = read_json(marker_path)
            scenario_problems = verdict(
                result.returncode, marker, restore_path.exists(), after, baseline, reason,
                timed_out=timed_out, cleanup_failed=cleanup_failed,
            )
            scenario_problems.extend(launch_verdict(mode, launched, baseline))
            summary["scenarios"][mode] = {
                "returncode": result.returncode,
                "timed_out": timed_out,
                "cleanup_failed": cleanup_failed,
                "after": after,
                "marker": marker,
                "launches": launched,
                "stdout": result.stdout,
                "stderr": result.stderr,
                "problems": scenario_problems,
            }
            problems.extend(f"{mode}: {problem}" for problem in scenario_problems)
    except (RunError, ValueError, OSError, subprocess.SubprocessError) as error:
        problems.append(str(error))
    finally:
        for role, device in baseline.items():
            try:
                set_default(run_host, role, device)
            except RunError as error:
                problems.append(f"cleanup {role}: {error}")
        try:
            summary["final_roles"] = roles_of(run_host(["host", "show"]))
        except (RunError, ValueError) as error:
            problems.append(f"cleanup show: {error}")
    return summary


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--ssr-guard", required=True)
    parser.add_argument("--ssr-live", required=True)
    parser.add_argument("--state-dir", required=True, type=Path)
    parser.add_argument("--summary", required=True, type=Path)
    args = parser.parse_args(argv)
    args.state_dir.mkdir(parents=True, exist_ok=True)
    # The guard runs the fake app with the state directory as its working
    # directory, so every path it is handed must be absolute.
    args.ssr_live = str(Path(args.ssr_live).resolve())
    args.ssr_guard = str(Path(args.ssr_guard).resolve())
    args.state_dir = args.state_dir.resolve()
    fake = Path(__file__).with_name("fake_app.py").resolve()
    run_host = make_run(args.ssr_live, args.state_dir)

    def run_guard(mode, fake_env):
        command = [
            args.ssr_guard, "--state-dir", str(args.state_dir), "--app", sys.executable,
            "--", str(fake), mode,
        ]
        env = {
            **os.environ,
            "SSR_GUARD_STATE_DIR": str(args.state_dir),
            **fake_env,
        }
        return run_guard_process(command, env, args.state_dir / "guard-output")

    summary = check(run_host, run_guard, args.state_dir, args.ssr_live)
    text = json.dumps(summary, indent=2)
    args.summary.write_text(text, encoding="utf-8")
    print(text)
    for problem in summary["problems"]:
        print(problem, file=sys.stderr)
    return 1 if summary["problems"] else 0


if __name__ == "__main__":
    raise SystemExit(main())

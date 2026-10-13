"""Separate-process BIO volume capture and pure comparison for volume_check.ps1."""

import argparse
import json
import os
import time
from pathlib import Path

from driver_flow_check import ControlDevice, FLAG_REPEAT, KIND_VOLUME, parse_records, parse_volume


def write_json(path, value):
    # Ready/stop coordination never exposes a half-written JSON file.
    path = Path(path)
    temporary = path.with_suffix(path.suffix + ".tmp")
    temporary.write_text(json.dumps(value, indent=2) + "\n", encoding="utf-8")
    os.replace(temporary, path)


def capture(output, ready, stop, seconds):
    device = None
    result = {"records": [], "errors": []}
    try:
        device = ControlDevice()
        result["query_info"] = device.query_info()
        deadline = time.monotonic() + seconds
        write_json(ready, result["query_info"])
        stop_deadline = None
        while time.monotonic() < deadline:
            # Drain briefly after stop, but do not require silence: other apps
            # may keep producing DATA indefinitely while the test restores.
            if stop_deadline is None and Path(stop).exists():
                stop_deadline = time.monotonic() + 0.25
            chunk = device.read(1, 50)
            for record in parse_records(chunk):
                if record["kind"] == KIND_VOLUME:
                    result["records"].append({"endpoint": record["endpoint"],
                        "qpc": record["qpc"], "repeat": bool(record["flags"] & FLAG_REPEAT),
                        **parse_volume(record["payload"])})
            if stop_deadline is not None and (not chunk or time.monotonic() >= stop_deadline):
                break
        else:
            raise TimeoutError("volume capture deadline expired before stop/drain")
    except Exception as error:
        result["errors"].append(str(error))
    finally:
        if device is not None:
            device.close()
        write_json(output, result)
    return 1 if result["errors"] else 0


def matches(record, expected):
    channels = expected["channels_db"]
    count = record["channel_count"]
    mask = (1 << count) - 1 if expected["mute"] else 0
    return (count == len(channels) and record["mute_mask"] == mask and
            all(abs(level / 65536.0 - db) <= 0.5
                for level, db in zip(record["level_q16"][:count], channels)))


def verify(captured, actions):
    errors = list(captured.get("errors", [])) + list(actions.get("errors", []))
    mask = captured.get("query_info", {}).get("endpoint_mask", 0)
    expected_endpoints = [ep for ep in range(4) if mask & (1 << ep)]
    endpoints = actions.get("endpoints", [])
    if not expected_endpoints or sorted(item["endpoint"] for item in endpoints) != expected_endpoints:
        errors.append("tested endpoints do not match QUERY_INFO endpoint_mask")
    records = captured.get("records", [])
    results = []
    for item in endpoints:
        endpoint = item["endpoint"]
        problems = []
        original = item.get("original")
        restored = item.get("restored")
        if not original or original["hardware_support"] & 3 != 3:
            problems.append("QueryHardwareSupport lacks VOLUME or MUTE")
        if not restored or not original or original["mute"] != restored["mute"] or \
                len(original["channels_db"]) != len(restored["channels_db"]) or \
                abs(original["master_db"] - restored["master_db"]) > 0.5 or \
                any(abs(a - b) > 0.5 for a, b in zip(original["channels_db"], restored["channels_db"])):
            problems.append("original master/channel volumes and mute were not restored")
        expected_channels = 8 if endpoint == 0 else 2
        if original and len(original["channels_db"]) != expected_channels:
            problems.append("volume node channel count differs from default device format")
        stages = item.get("stages", [])
        if [stage["name"] for stage in stages] != ["level", "mute_on", "mute_off"]:
            problems.append("missing volume/mute test stages")
        for stage in stages:
            expected = stage["expected"]
            if abs(expected["master_db"] + 20) > 0.5 or expected["mute"] != (stage["name"] == "mute_on"):
                problems.append(stage["name"] + ": endpoint API did not reach requested state")
            eligible = [record for record in records if record["endpoint"] == endpoint and
                        not record["repeat"] and stage["start_qpc"] <= record["qpc"] <= stage["end_qpc"]]
            # Compare the final state, not an intermediate channel SET that
            # happened to match before a later incorrect update.
            if not eligible or not matches(eligible[-1], expected):
                problems.append(stage["name"] + ": final changed VOLUME record missing or incorrect")
        mine = [record for record in records if record["endpoint"] == endpoint]
        results.append({"endpoint": endpoint, "name": item.get("name"),
                        "volume_records": len(mine), "volume_repeats": sum(r["repeat"] for r in mine),
                        "original": original, "restored": restored, "stages": stages, "errors": problems})
        errors.extend("endpoint {0}: {1}".format(endpoint, error) for error in problems)
    return {"passed": not errors, "errors": errors, "query_info": captured.get("query_info"), "endpoints": results}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    modes = parser.add_subparsers(dest="mode", required=True)
    reader = modes.add_parser("capture")
    reader.add_argument("--out", required=True)
    reader.add_argument("--ready", required=True)
    reader.add_argument("--stop", required=True)
    reader.add_argument("--seconds", type=float, default=120)
    check = modes.add_parser("verify")
    check.add_argument("--capture", required=True)
    check.add_argument("--actions", required=True)
    check.add_argument("--summary", required=True)
    args = parser.parse_args()
    if args.mode == "capture":
        return capture(args.out, args.ready, args.stop, args.seconds)
    try:
        captured = json.loads(Path(args.capture).read_text(encoding="utf-8-sig"))
        actions = json.loads(Path(args.actions).read_text(encoding="utf-8-sig"))
        result = verify(captured, actions)
    except Exception as error:
        result = {"passed": False, "errors": [str(error)]}
    write_json(args.summary, result)
    print(json.dumps(result, indent=2))
    return 0 if result["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())

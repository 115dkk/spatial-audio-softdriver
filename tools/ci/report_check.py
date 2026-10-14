"""Authoritative verdicts for ``ssr-live --report`` JSON files."""

import argparse
from dataclasses import dataclass, replace
import json
from pathlib import Path

from ssr_abi import MAX_ENDPOINTS

# docs/testing/test-signed-pc.md 6-1.
CADENCE_RATIO_MIN = 0.98
CADENCE_RATIO_MAX = 1.02
# Existing CI invalid-clock sentinel; no real audio chain is this slow.
LATENCY_CEILING_MS = 1_000.0
ENDPOINT_COUNT = MAX_ENDPOINTS

COMMON_LOSS_COUNTERS = (
    "underruns_while_streaming", "output_cadence_faults", "dropped_frames",
    "lane_underrun_frames", "output_overflow_frames", "dropped_events",
)


# An expectation of "requested" compares with what the run asked for, which
# ssr-live writes into the report (clock_requested, output_mode_requested), so
# a tester never repeats the command line's options here.
REQUESTED = "requested"


@dataclass(frozen=True)
class Expect:
    source: str | None  # driver, sal or dump
    output_mode: str | None  # exclusive, shared, requested
    clock: str | None  # auto, trim, asrc, requested
    reconnects: str  # required, forbidden, or ignored
    latency_ceiling_ms: float | None
    cadence_band: tuple[float, float] | None
    zero_counters: tuple[str, ...]
    allow_unsupported: bool
    require_driver: bool = False
    output_kind: str | None = None
    require_no_fallback: bool = True
    require_endpoints: bool = True
    faults: str = "forbidden"  # forbidden or output_only


PRESETS = {
    "sal": Expect("sal", REQUESTED, REQUESTED, "forbidden", LATENCY_CEILING_MS, None,
                  ("underruns", "output_restarts"), False),
    "listen": Expect("driver", REQUESTED, REQUESTED, "forbidden", LATENCY_CEILING_MS,
                     (CADENCE_RATIO_MIN, CADENCE_RATIO_MAX), COMMON_LOSS_COUNTERS,
                     False, True),
    "speakers": Expect("driver", REQUESTED, REQUESTED, "forbidden", LATENCY_CEILING_MS,
                       (CADENCE_RATIO_MIN, CADENCE_RATIO_MAX), COMMON_LOSS_COUNTERS,
                       False, True, "speakers"),
    "replug": Expect("driver", REQUESTED, REQUESTED, "required", LATENCY_CEILING_MS,
                     (CADENCE_RATIO_MIN, CADENCE_RATIO_MAX), COMMON_LOSS_COUNTERS,
                     False, True, faults="output_only"),
    "loopback": Expect("dump", None, None, "forbidden", LATENCY_CEILING_MS, None,
                       COMMON_LOSS_COUNTERS + ("output_restarts",), False),
    "driver_e2e": Expect("driver", "shared", None, "forbidden", LATENCY_CEILING_MS,
                         (CADENCE_RATIO_MIN, CADENCE_RATIO_MAX), COMMON_LOSS_COUNTERS,
                         False, True),
    "soak": Expect("dump", None, None, "forbidden", LATENCY_CEILING_MS,
                   (CADENCE_RATIO_MIN, CADENCE_RATIO_MAX),
                   COMMON_LOSS_COUNTERS + ("output_restarts",), False),
    "restart": Expect("dump", None, None, "forbidden", LATENCY_CEILING_MS,
                      (CADENCE_RATIO_MIN, CADENCE_RATIO_MAX),
                      COMMON_LOSS_COUNTERS + ("output_restarts",), False),
}


def _missing(report, key, problems):
    if key not in report:
        problems.append(f"report is missing {key}")
        return True
    return False


def _requested(report, key, expected, problems):
    """The expectation itself, or what the report says the run asked for."""
    if expected != REQUESTED:
        return expected
    if _missing(report, key, problems):
        return None
    return report[key]


def _clock_problems(report, requested):
    problems = []
    requested = _requested(report, "clock_requested", requested, problems)
    if requested is None or _missing(report, "clock_strategy", problems):
        return problems
    actual = report["clock_strategy"]
    if requested in ("asrc", "trim"):
        if actual != requested:
            problems.append(f"clock_strategy is {actual}, expected {requested}")
        return problems
    if requested == "auto":
        for key in ("output_sample_rate", "trim_available"):
            _missing(report, key, problems)
        if problems:
            return problems
        expected = "trim" if report["output_sample_rate"] == 48_000 and report["trim_available"] else "asrc"
        if actual != expected:
            problems.append(f"clock_strategy is {actual}, auto requires {expected}")
    return problems


def problems(report, expect):
    """Return every failed expectation. Unknown future fields are ignored."""
    if not isinstance(report, dict):
        return ["report must be an object"]
    result = []
    if expect.source is not None:
        if not _missing(report, "source", result) and report["source"] != expect.source:
            result.append(f"source is {report['source']}, expected {expect.source}")
    if expect.output_mode is not None:
        mode = _requested(report, "output_mode_requested", expect.output_mode, result)
        if mode is not None and not _missing(report, "output_mode", result) and report["output_mode"] != mode:
            result.append(f"output opened {report['output_mode']}, expected {mode}")
    if expect.require_no_fallback:
        if not _missing(report, "output_fell_back", result) and report["output_fell_back"]:
            result.append("exclusive output fell back to shared mode")
    if expect.output_kind is not None:
        if not _missing(report, "output_kind", result) and report["output_kind"] != expect.output_kind:
            result.append(f"output_kind is {report['output_kind']}, expected {expect.output_kind}")
    for key in expect.zero_counters:
        if not _missing(report, key, result) and report[key] != 0:
            result.append(f"{key} is {report[key]}")
    if expect.reconnects != "ignored":
        if not _missing(report, "output_reconnects", result):
            reconnects = report["output_reconnects"]
            if expect.reconnects == "required" and reconnects < 1:
                result.append("output_reconnects must be at least 1")
            elif expect.reconnects == "forbidden" and reconnects != 0:
                result.append(f"output_reconnects is {reconnects}")
    if expect.cadence_band is not None:
        if not _missing(report, "output_cadence_ratio", result):
            low, high = expect.cadence_band
            ratio = report["output_cadence_ratio"]
            if not isinstance(ratio, (int, float)) or not low <= ratio <= high:
                result.append(f"output_cadence_ratio is {ratio}, expected {low}..{high}")
    if expect.clock is not None:
        result.extend(_clock_problems(report, expect.clock))
    if not _missing(report, "faults", result) and report["faults"]:
        faults = report["faults"]
        if expect.faults == "output_only":
            if not isinstance(faults, list) or any("Output" not in str(fault) for fault in faults):
                result.append(f"faults contain more than the expected output outage: {faults}")
        else:
            result.append(f"faults: {faults}")
    if expect.latency_ceiling_ms is not None:
        for key in ("latency_p50_ms", "latency_p99_ms"):
            value = report.get(key)
            if isinstance(value, (int, float)) and value > expect.latency_ceiling_ms:
                result.append(f"{key} is {value} ms, above {expect.latency_ceiling_ms:g} ms")
    if expect.require_endpoints:
        endpoints = report.get("endpoints")
        if not isinstance(endpoints, list):
            result.append("report endpoints must be an array")
        elif len(endpoints) != ENDPOINT_COUNT:
            result.append(f"report endpoints must contain exactly {ENDPOINT_COUNT} entries")
        else:
            for slot, endpoint in enumerate(endpoints):
                if not isinstance(endpoint, dict):
                    result.append(f"report endpoint {slot} must be an object")
                    continue
                for key in ("endpoint", "format", "epoch", "unsupported"):
                    if key not in endpoint:
                        result.append(f"report endpoint {slot} is missing {key}")
                if endpoint.get("endpoint") != slot:
                    result.append(f"report endpoint slot {slot} names {endpoint.get('endpoint')}")
                if not expect.allow_unsupported and endpoint.get("unsupported") is not None:
                    result.append(f"endpoint {slot} has unsupported format: {endpoint['unsupported']}")
    if expect.require_driver:
        driver = report.get("driver")
        if not isinstance(driver, dict):
            result.append("report is missing driver statistics")
        elif not isinstance(driver.get("endpoints"), list):
            result.append("driver statistics are missing endpoints")
        else:
            for endpoint in driver["endpoints"]:
                if "lost_bytes" not in endpoint:
                    result.append(f"driver endpoint {endpoint.get('endpoint')} is missing lost_bytes")
                elif endpoint["lost_bytes"] != 0:
                    result.append(f"driver endpoint {endpoint.get('endpoint')} lost {endpoint['lost_bytes']} bytes")
    return result


def main(argv=None):
    parser = argparse.ArgumentParser()
    parser.add_argument("--preset", choices=PRESETS, required=True)
    parser.add_argument("report")
    # Overrides; by default the presets compare with what the run requested.
    parser.add_argument("--clock", choices=("auto", "trim", "asrc"))
    parser.add_argument("--output-mode", choices=("exclusive", "shared"))
    args = parser.parse_args(argv)
    try:
        report = json.loads(Path(args.report).read_text(encoding="utf-8-sig"))
        expect = PRESETS[args.preset]
        if args.clock is not None:
            expect = replace(expect, clock=args.clock)
        if args.output_mode is not None:
            expect = replace(expect, output_mode=args.output_mode)
        found = problems(report, expect)
    except Exception as error:
        found = [str(error)]
    if found:
        for problem in found:
            print(problem)
        return 1
    print("pass")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

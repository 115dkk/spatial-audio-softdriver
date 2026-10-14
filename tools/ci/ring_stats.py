"""Shared per-endpoint record statistics and calibrated test tones."""

import numpy as np

from ssr_abi import (RECORD_FLAG_DISCONTINUITY, RECORD_KIND_DATA,
                     RECORD_KIND_OVERRUN, OVERRUN_PAYLOAD)

# Keep endpoint 2 away from harmonics and difference products of endpoints 0/1.
ENDPOINT_TONES = {0: 1000.0, 1: 1500.0, 2: 2250.0}
FLOW_TONE_AMPLITUDE = 0.5
FLOW_TONE_DBFS = 20 * np.log10(FLOW_TONE_AMPLITUDE)  # -6.0206 dBFS
# docs/testing/test-signed-pc.md step 2.
RATE_ERROR_LIMIT_PPM = 100.0
OTHER_TONE_LIMIT_DBFS = -60.0
OWN_TONE_TOLERANCE_DB = 1.0


def tone_db(signal, rate, hz):
    if len(signal) == 0:
        return -200.0
    time = np.arange(len(signal)) / rate
    amplitude = 2 * abs(np.dot(signal, np.exp(-2j * np.pi * hz * time))) / len(signal)
    return 20 * np.log10(max(amplitude, 1e-10))


def endpoint_stats(records, endpoint, qpc_frequency, fmt=None, decode=None,
                   tone_hz=None, other_hz=()):
    mine = [record for record in records if record["endpoint"] == endpoint]
    data = [record for record in mine if record["kind"] == RECORD_KIND_DATA]
    overruns = [record for record in mine if record["kind"] == RECORD_KIND_OVERRUN]
    gaps = sum(record["flags"] & RECORD_FLAG_DISCONTINUITY or
               record["byte_pos"] != previous["byte_pos"] + previous["len"]
               for previous, record in zip(data, data[1:]))
    gap_bytes = sum(max(0, record["byte_pos"] - previous["byte_pos"] - previous["len"])
                    for previous, record in zip(data, data[1:]))
    overrun_bytes = sum(OVERRUN_PAYLOAD.unpack(record["payload"])[0]
                        for record in overruns if len(record["payload"]) == OVERRUN_PAYLOAD.size)
    result = {"gaps": gaps, "overruns": len(overruns),
              "lost_bytes": max(gap_bytes, overrun_bytes),
              "bytes": sum(record["len"] for record in data)}
    if fmt and data:
        result["seconds"] = result["bytes"] / fmt["avg_bytes"] if fmt["avg_bytes"] else 0.0
        span = (data[-1]["qpc"] - data[0]["qpc"]) / qpc_frequency
        if span > 0.5 and fmt["avg_bytes"]:
            measured = (result["bytes"] - data[0]["len"]) / span
            result["rate_error_ppm"] = (measured / fmt["avg_bytes"] - 1) * 1e6
        if decode and tone_hz is not None:
            samples = decode(b"".join(record["payload"] for record in data), fmt)
            if samples is not None:
                front = samples[:, 0]
                loud = np.flatnonzero(np.abs(front) > 1e-3)
                front = front[loud[0]:loud[-1] + 1] if len(loud) else front[:0]
                result["tone_db"] = tone_db(front, fmt["rate"], tone_hz)
                result["other_tone_db"] = max(
                    (tone_db(front, fmt["rate"], hz) for hz in other_hz), default=-200.0)
    return result


def threshold_problems(endpoint, stats):
    problems = []
    if stats["gaps"]:
        problems.append(f"endpoint {endpoint}: {stats['gaps']} gaps in byte_pos")
    if stats["overruns"]:
        problems.append(f"endpoint {endpoint}: {stats['overruns']} overrun records")
    ppm = stats.get("rate_error_ppm")
    if ppm is not None and abs(ppm) > RATE_ERROR_LIMIT_PPM:
        problems.append(f"endpoint {endpoint}: DMA moved {ppm:+.0f} ppm off the format's byte rate")
    own = stats.get("tone_db")
    if own is not None and abs(own - FLOW_TONE_DBFS) > OWN_TONE_TOLERANCE_DB:
        problems.append(f"endpoint {endpoint}: its own tone is at {own:.1f} dBFS")
    other = stats.get("other_tone_db")
    if other is not None and other > OTHER_TONE_LIMIT_DBFS:
        problems.append(f"endpoint {endpoint}: another endpoint's tone leaked in ({other:.1f} dBFS)")
    return problems

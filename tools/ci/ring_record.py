"""Records the BIO Atmos driver ring into a .biodump diagnostic file.

The dump is a 32-byte DumpHeader followed by the exact bytes returned by READ.
Parsing and Windows device access are shared with driver_flow_check.py.
"""

import argparse
import datetime as dt
import json
import math
import os
import statistics
import struct
import sys
import time
from pathlib import Path

from driver_flow_check import (
    FLAG_DISCONTINUITY,
    KIND_DATA,
    KIND_FORMAT_CHANGE,
    KIND_OVERRUN,
    KIND_STREAM_START,
    KIND_STREAM_STOP,
    ControlDevice,
    parse_records,
    parse_wave_format,
)

DUMP_HEADER = struct.Struct("<8sIIQII")
DUMP_MAGIC = b"BIODUMP\0"
ABI_VERSION = 1
DUMP_FORMAT_VERSION = 1
ERROR_SHARING_VIOLATION = 32
DEFAULT_TIMEOUT_MS = 50

# GUID Data1 values from crates/bio-abi/src/wave.rs and ksmedia.h.
IEC_NAMES = {
    0x000A: "IEC 61937 E-AC-3",
    0x010A: "IEC 61937 E-AC-3 Atmos",
    0x000B: "IEC 61937 DTS-HD",
    0x010B: "IEC 61937 DTS:X E1",
    0x030B: "IEC 61937 DTS:X E2",
    0x000C: "IEC 61937 TrueHD",
    0x010C: "IEC 61937 MAT 2.0",
    0x030C: "IEC 61937 MAT 2.1",
    0x070C: "IEC 61937 MAT 2.1",
}
GUID_TAIL = bytes.fromhex("1000800000aa00389b71")


def dump_header(qpc_frequency):
    return DUMP_HEADER.pack(DUMP_MAGIC, ABI_VERSION, DUMP_HEADER.size, qpc_frequency,
                            DUMP_FORMAT_VERSION, 0)


def format_description(payload):
    """Returns a JSON-safe, human-readable WAVEFORMATEX description."""
    fmt = parse_wave_format(payload)
    tag = fmt["tag"]
    name = None
    channel_mask = 0
    valid_bits = fmt["bits"]
    if tag == 0x0001:
        name = "PCM {0}-bit".format(fmt["bits"])
    elif tag == 0x0003:
        name = "PCM 32-bit float" if fmt["bits"] == 32 else "PCM float"
    elif tag == 0x0092:
        name = "IEC 61937 AC-3"
    elif tag == 0x0008:
        name = "IEC 61937 DTS"
    elif tag == 0xFFFE and len(payload) >= 40:
        valid_bits, channel_mask = struct.unpack_from("<HI", payload, 18)
        data1, family = struct.unpack_from("<IH", payload, 24)
        if payload[30:40] == GUID_TAIL:
            if family == 0:
                if data1 == 1:
                    name = "PCM {0}-bit".format(valid_bits or fmt["bits"])
                elif data1 == 3:
                    name = "PCM 32-bit float"
                elif data1 == 0x92:
                    name = "IEC 61937 AC-3"
                elif data1 == 8:
                    name = "IEC 61937 DTS"
            elif family == 0x0CEA:
                name = IEC_NAMES.get(data1)
        if name is None:
            name = "WAVEFORMATEXTENSIBLE {0:#x}/{1:#x}".format(family, data1)
    else:
        name = "WAVEFORMATEX tag {0:#06x}".format(tag)
    return {
        "name": name,
        "channels": fmt["channels"],
        "container_rate": fmt["rate"],
        "container_bits": fmt["bits"],
        "valid_bits": valid_bits,
        "channel_mask": "0x{0:X}".format(channel_mask),
        "avg_bytes_per_second": fmt["avg_bytes"],
    }


def percentile(values, fraction):
    """Linear-interpolated percentile, matching common p99 reporting."""
    if not values:
        return None
    ordered = sorted(values)
    position = (len(ordered) - 1) * fraction
    lower = int(math.floor(position))
    upper = int(math.ceil(position))
    if lower == upper:
        return ordered[lower]
    return ordered[lower] + (ordered[upper] - ordered[lower]) * (position - lower)


def summarize_records(records, qpc_frequency, elapsed_seconds, stats_start=None, stats_end=None):
    endpoints = []
    endpoint_ids = sorted(set([0, 1] + [r["endpoint"] for r in records]))
    for endpoint in endpoint_ids:
        mine = [r for r in records if r["endpoint"] == endpoint]
        data = [r for r in mine if r["kind"] == KIND_DATA]
        formats = []
        for record in mine:
            if record["kind"] == KIND_FORMAT_CHANGE:
                item = format_description(record["payload"])
                item.update({"qpc": record["qpc"], "epoch": record["epoch"]})
                formats.append(item)
        discontinuities = 0
        discontinuity_flags = 0
        lost_from_gaps = 0
        previous = None
        for record in data:
            if record["flags"] & FLAG_DISCONTINUITY:
                discontinuity_flags += 1
            if previous is not None:
                expected = previous["byte_pos"] + previous["len"]
                if record["byte_pos"] != expected:
                    discontinuities += 1
                    if record["byte_pos"] > expected:
                        lost_from_gaps += record["byte_pos"] - expected
            previous = record
        overrun_lost = 0
        overruns = [r for r in mine if r["kind"] == KIND_OVERRUN]
        for record in overruns:
            if len(record["payload"]) >= 8:
                overrun_lost += struct.unpack_from("<Q", record["payload"])[0]
        written = [r["qpc_written"] for r in data if r.get("qpc_written")]
        intervals = []
        if qpc_frequency:
            intervals = [(b - a) * 1000.0 / qpc_frequency for a, b in zip(written, written[1:]) if b >= a]
        interval_summary = None
        if intervals:
            interval_summary = {
                "count": len(intervals),
                "min_ms": min(intervals),
                "median_ms": statistics.median(intervals),
                "p99_ms": percentile(intervals, 0.99),
                "max_ms": max(intervals),
            }
        total_bytes = sum(r["len"] for r in data)
        endpoint_summary = {
            "endpoint": endpoint,
            "formats": formats,
            "data_records": len(data),
            "data_bytes": total_bytes,
            "bytes_per_second": total_bytes / elapsed_seconds if elapsed_seconds > 0 else None,
            "byte_pos_discontinuities": discontinuities,
            "discontinuity_flags": discontinuity_flags,
            "overrun_records": len(overruns),
            "lost_bytes": max(lost_from_gaps, overrun_lost),
            "starts": sum(r["kind"] == KIND_STREAM_START for r in mine),
            "stops": sum(r["kind"] == KIND_STREAM_STOP for r in mine),
            "qpc_written_intervals": interval_summary,
        }
        if stats_start and stats_end and endpoint < len(stats_end.get("endpoints", [])):
            endpoint_summary["stats_start"] = stats_start["endpoints"][endpoint]
            endpoint_summary["stats_end"] = stats_end["endpoints"][endpoint]
        endpoints.append(endpoint_summary)
    return {
        "elapsed_seconds": elapsed_seconds,
        "record_count": len(records),
        "endpoints": endpoints,
        "stats_start": stats_start,
        "stats_end": stats_end,
        "max_timer_lateness_us": {
            "start": stats_start.get("max_timer_lateness_us") if stats_start else None,
            "end": stats_end.get("max_timer_lateness_us") if stats_end else None,
        },
    }


def metadata(records):
    """Drops data payloads, keeping what the summary reads: READ returns whole
    records, so each chunk parses on its own and a long recording does not
    hold its audio in memory."""
    kept = []
    for record in records:
        if record["kind"] not in (KIND_FORMAT_CHANGE, KIND_OVERRUN):
            record = dict(record, payload=b"")
        kept.append(record)
    return kept


def default_output():
    return "bio-{0}.biodump".format(dt.datetime.now().strftime("%Y%m%d-%H%M%S"))


def summary_path(dump_path):
    path = Path(dump_path)
    return path.with_name(path.stem + "-summary.json")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--seconds", type=float, default=30.0,
                        help="recording duration; 0 records until Ctrl+C")
    parser.add_argument("--out", default=default_output())
    parser.add_argument("--min-bytes", type=int, default=4096)
    parser.add_argument("--timeout-ms", type=int, default=DEFAULT_TIMEOUT_MS)
    args = parser.parse_args()
    if args.seconds < 0 or args.min_bytes < 0 or args.timeout_ms < 0:
        parser.error("--seconds, --min-bytes, and --timeout-ms must not be negative")

    device = None
    try:
        device = ControlDevice()
    except OSError as error:
        if error.errno == ERROR_SHARING_VIOLATION:
            print("다른 프로그램(재생 시험, bio-live 등)이 장치를 열고 있습니다.", file=sys.stderr)
            return 2
        print("BIO Atmos 제어 장치를 열 수 없습니다: {0}".format(error), file=sys.stderr)
        return 1

    out_path = Path(args.out).resolve()
    started = time.monotonic()
    records = []
    try:
        info = device.query_info()
        stats_start = device.stats()
        with open(out_path, "wb") as output:
            output.write(dump_header(info["qpc_frequency"]))
            try:
                while args.seconds == 0 or time.monotonic() - started < args.seconds:
                    chunk = device.read(args.min_bytes, args.timeout_ms)
                    output.write(chunk)
                    records.extend(metadata(parse_records(chunk)))
            except KeyboardInterrupt:
                print("녹화를 중지합니다.")
        elapsed = time.monotonic() - started
        stats_end = device.stats()
    except OSError as error:
        print("BIO Atmos 장치 읽기 오류: {0}".format(error), file=sys.stderr)
        return 1
    finally:
        device.close()

    summary = summarize_records(records, info["qpc_frequency"], elapsed, stats_start, stats_end)
    summary.update({"dump": str(out_path), "query_info": info, "time": dt.datetime.now().astimezone().isoformat()})
    sidecar = summary_path(out_path)
    with open(sidecar, "w", encoding="utf-8") as output:
        json.dump(summary, output, ensure_ascii=False, indent=2)
        output.write("\n")
    print(json.dumps(summary, ensure_ascii=False, indent=2))
    print("덤프: {0}\n요약: {1}".format(out_path, sidecar))
    return 0


if __name__ == "__main__":
    sys.exit(main())

"""Records the Spatial SoftReceiver driver ring into a .ssrdump diagnostic file.

The dump is a 32-byte DumpHeader followed by the exact bytes returned by READ.
Parsing and Windows device access are shared with driver_flow_check.py.
"""

import argparse
import ctypes
import datetime as dt
import json
import math
import os
import statistics
import sys
import time
from pathlib import Path

from driver_flow_check import parse_records, parse_volume, parse_wave_format
from ring_stats import endpoint_stats
from ssr_abi import (
    ABI_VERSION, DUMP_FORMAT_VERSION, DUMP_HEADER, DUMP_MAGIC,
    RECORD_ENDPOINT_RECORDER as RECORDER_ENDPOINT,
    RECORD_FLAG_DISCONTINUITY as FLAG_DISCONTINUITY,
    RECORD_FLAG_REPEAT as FLAG_REPEAT, RECORD_HEADER as HEADER,
    RECORD_KIND_DATA as KIND_DATA, RECORD_KIND_FORMAT_CHANGE as KIND_FORMAT_CHANGE,
    RECORD_KIND_OVERRUN as KIND_OVERRUN, RECORD_KIND_READ_END as KIND_READ_END,
    RECORD_KIND_STREAM_START as KIND_STREAM_START,
    RECORD_KIND_STREAM_STOP as KIND_STREAM_STOP, RECORD_KIND_VOLUME as KIND_VOLUME,
    IEC61937_FAMILY, IEC61937_NAMES, WAVEFORMATEXTENSIBLE,
    WAVE_FAMILY, WAVE_FORMAT_DOLBY_AC3_SPDIF, WAVE_FORMAT_DTS,
    WAVE_FORMAT_EXTENSIBLE, WAVE_FORMAT_IEEE_FLOAT, WAVE_FORMAT_PCM,
    WAVE_GUID_TAIL,
)
from ssr_control import ControlDevice

ERROR_SHARING_VIOLATION = 32
DEFAULT_TIMEOUT_MS = 50

def dump_header(qpc_frequency):
    return DUMP_HEADER.pack(DUMP_MAGIC, ABI_VERSION, DUMP_HEADER.size, qpc_frequency,
                            DUMP_FORMAT_VERSION, 0)


def qpc_now(qpc_frequency):
    if os.name == "nt":
        value = ctypes.c_longlong()
        if not ctypes.windll.kernel32.QueryPerformanceCounter(ctypes.byref(value)):
            raise ctypes.WinError()
        return value.value
    return time.monotonic_ns() * qpc_frequency // 1_000_000_000


def read_end(qpc):
    return HEADER.pack(KIND_READ_END, 0, 0, 0, qpc, 0, 0, RECORDER_ENDPOINT, 0)


def format_description(payload):
    """Returns a JSON-safe, human-readable WAVEFORMATEX description."""
    fmt = parse_wave_format(payload)
    tag = fmt["tag"]
    name = None
    channel_mask = 0
    valid_bits = fmt["bits"]
    if tag == WAVE_FORMAT_PCM:
        name = "PCM {0}-bit".format(fmt["bits"])
    elif tag == WAVE_FORMAT_IEEE_FLOAT:
        name = "PCM 32-bit float" if fmt["bits"] == 32 else "PCM float"
    elif tag == WAVE_FORMAT_DOLBY_AC3_SPDIF:
        name = "IEC 61937 AC-3"
    elif tag == WAVE_FORMAT_DTS:
        name = "IEC 61937 DTS"
    elif tag == WAVE_FORMAT_EXTENSIBLE and len(payload) >= WAVEFORMATEXTENSIBLE.size:
        values = WAVEFORMATEXTENSIBLE.unpack_from(payload)
        valid_bits, channel_mask, subformat = values[7:]
        data1 = int.from_bytes(subformat[:4], "little")
        family = int.from_bytes(subformat[4:6], "little")
        if subformat[6:] == WAVE_GUID_TAIL:
            if family == WAVE_FAMILY:
                if data1 == 1:
                    name = ("PCM 24-in-32-bit" if valid_bits == 24 and fmt["bits"] == 32
                            else "PCM 24-bit packed" if fmt["bits"] == 24
                            else "PCM {0}-bit".format(valid_bits or fmt["bits"]))
                elif data1 == 3:
                    name = "PCM 32-bit float"
                elif data1 == 0x92:
                    name = "IEC 61937 AC-3"
                elif data1 == 8:
                    name = "IEC 61937 DTS"
            elif family == IEC61937_FAMILY:
                name = IEC61937_NAMES.get(data1)
        if name is None:
            name = "WAVEFORMATEXTENSIBLE {0:#x}/{1:#x}".format(family, data1)
    else:
        name = "WAVEFORMATEX tag {0:#06x}".format(tag)
    if tag in (1, 3) and fmt["channels"] in (1, 2):
        channel_mask = 4 if fmt["channels"] == 1 else 3
    return {
        "name": name,
        "channel_layout": "direct-out" if channel_mask == 0 and fmt["sample"] else "speaker-mask",
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


def summarize_records(records, qpc_frequency, elapsed_seconds, endpoint_mask, stats_start=None, stats_end=None):
    endpoints = []
    endpoint_ids = [endpoint for endpoint in range(4) if endpoint_mask & (1 << endpoint)]
    for endpoint in endpoint_ids:
        mine = [r for r in records if r["endpoint"] == endpoint]
        data = [r for r in mine if r["kind"] == KIND_DATA]
        formats = []
        for record in mine:
            if record["kind"] == KIND_FORMAT_CHANGE and not record["flags"] & FLAG_REPEAT:
                item = format_description(record["payload"])
                item.update({"qpc": record["qpc"], "epoch": record["epoch"]})
                formats.append(item)
        shared = endpoint_stats(records, endpoint, qpc_frequency)
        discontinuities = sum(
            record["byte_pos"] != previous["byte_pos"] + previous["len"]
            for previous, record in zip(data, data[1:]))
        discontinuity_flags = sum(bool(record["flags"] & FLAG_DISCONTINUITY) for record in data)
        overruns = [r for r in mine if r["kind"] == KIND_OVERRUN]
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
        volumes = [r for r in mine if r["kind"] == KIND_VOLUME]
        endpoint_summary = {
            "endpoint": endpoint,
            "formats": formats,
            "volume_records": len(volumes),
            "volume_changes": sum(not r["flags"] & FLAG_REPEAT for r in volumes),
            "volume_repeats": sum(bool(r["flags"] & FLAG_REPEAT) for r in volumes),
            "volume": parse_volume(volumes[-1]["payload"]) if volumes else None,
            "data_records": len(data),
            "data_bytes": total_bytes,
            "bytes_per_second": total_bytes / elapsed_seconds if elapsed_seconds > 0 else None,
            "byte_pos_discontinuities": discontinuities,
            "discontinuity_flags": discontinuity_flags,
            "overrun_records": len(overruns),
            "lost_bytes": shared["lost_bytes"],
            "rate_error_ppm": shared.get("rate_error_ppm"),
            "starts": sum(r["kind"] == KIND_STREAM_START and not r["flags"] & FLAG_REPEAT for r in mine),
            "stops": sum(r["kind"] == KIND_STREAM_STOP and not r["flags"] & FLAG_REPEAT for r in mine),
            "state_repeats": sum(bool(r["flags"] & FLAG_REPEAT) for r in mine),
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
        if record["kind"] >= 0x8000:
            continue
        if record["kind"] not in (KIND_FORMAT_CHANGE, KIND_OVERRUN, KIND_VOLUME):
            record = dict(record, payload=b"")
        kept.append(record)
    return kept


def default_output():
    return "ssr-{0}.ssrdump".format(dt.datetime.now().strftime("%Y%m%d-%H%M%S"))


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
            print("다른 프로그램(재생 시험, ssr-live 등)이 장치를 열고 있습니다.", file=sys.stderr)
            return 2
        print("Spatial SoftReceiver 제어 장치를 열 수 없습니다: {0}".format(error), file=sys.stderr)
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
                    output.write(read_end(qpc_now(info["qpc_frequency"])))
                    records.extend(metadata(parse_records(chunk)))
            except KeyboardInterrupt:
                print("녹화를 중지합니다.")
        elapsed = time.monotonic() - started
        stats_end = device.stats()
    except OSError as error:
        print("Spatial SoftReceiver 장치 읽기 오류: {0}".format(error), file=sys.stderr)
        return 1
    finally:
        device.close()

    summary = summarize_records(records, info["qpc_frequency"], elapsed, info["endpoint_mask"], stats_start, stats_end)
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

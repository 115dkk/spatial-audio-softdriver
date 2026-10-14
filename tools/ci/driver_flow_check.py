"""Plays a tone into each Spatial SoftReceiver endpoint and reads it back from the driver.

The driver test job installs the test-signed package (ADR 0027). When the
runner's kernel loads it, this script plays a different tone into each render
endpoint at the same time through WASAPI shared mode, reads the record stream
from \\\\.\\SpatialSoftReceiver with IOCTL_SSR_READ meanwhile, and checks per endpoint:

* a format change and a stream start came before the data,
* the data bytes follow each other (byte_pos) with no gap and no overrun,
* the virtual DMA moved at the format's byte rate (QPC of the last byte of
  each record against the bytes in between), and
* the endpoint's own tone is in its data and the other active endpoints are not.

Across endpoints it checks that every READ is one snapshot: the endpoints that
delivered data in a READ end it at the same QPC, within one frame.

    python driver_flow_check.py --summary out.json

Exit status 0 when every check passes, 1 otherwise. main() is CI only; the
parsing and checks are pure and tested on Linux (test_driver_flow_check.py).
"""

import argparse
import json
import os
import struct
import sys
import threading
import time

import numpy as np

from endpoints import ENDPOINT_NUMBERS, enumerate_endpoints, find_speaker
from ring_stats import (ENDPOINT_TONES, FLOW_TONE_AMPLITUDE, endpoint_stats,
                        threshold_problems)
from ssr_abi import (
    ABI_INFO, DEFAULT_ENDPOINT_MASK, MAX_DATA_PAYLOAD, MAX_VOLUME_CHANNELS,
    READ_MAX_BYTES, RECORD_ALIGN, RECORD_FLAG_DISCONTINUITY as FLAG_DISCONTINUITY,
    RECORD_FLAG_REPEAT as FLAG_REPEAT, RECORD_HEADER as HEADER,
    RECORD_KIND_DATA as KIND_DATA, RECORD_KIND_FORMAT_CHANGE as KIND_FORMAT_CHANGE,
    RECORD_KIND_OVERRUN as KIND_OVERRUN, RECORD_KIND_STREAM_START as KIND_STREAM_START,
    RECORD_KIND_STREAM_STOP as KIND_STREAM_STOP, RECORD_KIND_VOLUME as KIND_VOLUME,
    VOLUME_PAYLOAD as VOLUME,
)
from ssr_control import ControlDevice, parse_abi_info

# Dump format versions before v3 carried sixteen volume levels.
LEGACY_VOLUME = struct.Struct("<II16i")

WAVE_FORMAT_PCM = 0x0001
WAVE_FORMAT_IEEE_FLOAT = 0x0003
WAVE_FORMAT_EXTENSIBLE = 0xFFFE
# Data1 of the KSDATAFORMAT_SUBTYPE_* GUIDs {0000xxxx-0000-0010-8000-00aa00389b71}.
SUBTYPE_TAIL = bytes.fromhex("000010008000 00aa00389b71".replace(" ", ""))

TONES = ENDPOINT_TONES
# Exclusive permission/priority do not prohibit shared mode on an idle device.
# ADR 0028 reserves endpoint 2 by routing policy, not a KS shared-mode ban.
# https://learn.microsoft.com/en-us/windows/win32/coreaudio/exclusive-mode-streams


def parse_records(stream):
    """The records of a READ stream, as dicts; stops at a cut record."""
    records = []
    offset = 0
    while offset + HEADER.size <= len(stream):
        kind, flags, epoch, byte_pos, qpc, qpc_written, length, endpoint, _ = HEADER.unpack_from(stream, offset)
        end = offset + HEADER.size + length
        if end > len(stream):
            break
        records.append({
            "kind": kind, "flags": flags, "epoch": epoch, "byte_pos": byte_pos,
            "qpc": qpc, "qpc_written": qpc_written, "len": length, "endpoint": endpoint,
            "payload": bytes(stream[offset + HEADER.size:end]),
        })
        offset = (end + RECORD_ALIGN - 1) // RECORD_ALIGN * RECORD_ALIGN
    return records


# The driver brings every running stream to the same QPC under one lock
# (CAdapterCommon::UpdatePositions), so the last data records of the endpoints
# in one READ end at most a frame apart (22.7 us at 44.1 kHz). When a position
# query advanced only the asking stream, a READ could hold one endpoint up to
# now and another a timer tick or more behind, and the mixer counted the
# difference as a lane underrun.
SNAPSHOT_SKEW_LIMIT_US = 100.0
SNAPSHOT_EXCLUDED_KINDS = (KIND_FORMAT_CHANGE, KIND_STREAM_START, KIND_STREAM_STOP, KIND_OVERRUN)


def snapshot_skew_us(reads, qpc_frequency, capacity=READ_MAX_BYTES):
    """The largest spread, in microseconds, between the last data QPCs of the
    endpoints in one READ, and how many READs were compared. A READ counts when
    it holds data from two or more endpoints, no format, start, stop or overrun
    record (a stream entering or leaving RUN), and was not cut short by its
    buffer."""
    worst = 0.0
    compared = 0
    for chunk in reads:
        # A READ whose buffer could not take one more record may end mid-tick.
        if len(chunk) + HEADER.size + MAX_DATA_PAYLOAD > capacity:
            continue
        records = parse_records(chunk)
        if any(r["kind"] in SNAPSHOT_EXCLUDED_KINDS for r in records):
            continue
        last = {}
        for r in records:
            if r["kind"] == KIND_DATA:
                last[r["endpoint"]] = r["qpc"]
        if len(last) < 2:
            continue
        compared += 1
        worst = max(worst, (max(last.values()) - min(last.values())) * 1e6 / qpc_frequency)
    return worst, compared


def parse_volume(payload):
    """Read current and legacy dump volume payloads; reject other sizes."""
    if len(payload) == VOLUME.size:
        count, mute_mask, *levels = VOLUME.unpack(payload)
    elif len(payload) == LEGACY_VOLUME.size:
        count, mute_mask, *levels = LEGACY_VOLUME.unpack(payload)
        if count > len(levels):
            raise ValueError("legacy volume channel count exceeds 16")
        levels.extend([0] * (MAX_VOLUME_CHANNELS - len(levels)))
    else:
        raise ValueError("volume payload must be 72 or 136 bytes")
    if not 1 <= count <= MAX_VOLUME_CHANNELS or mute_mask >> count:
        raise ValueError("invalid volume channel count or mute mask")
    if any(not -96 * 65536 <= level <= 0 for level in levels[:count]) or any(levels[count:]):
        raise ValueError("invalid volume levels or nonzero unused channels")
    return {"channel_count": count, "mute_mask": mute_mask, "level_q16": levels,
            "levels_db": [level / 65536.0 for level in levels[:count]]}


def parse_wave_format(payload):
    """WAVEFORMATEX(TENSIBLE): tag, channels, rate, byte rate, block, bits, sample kind."""
    tag, channels, rate, avg, block, bits = struct.unpack_from("<HHIIHH", payload)
    kind = {WAVE_FORMAT_PCM: "int", WAVE_FORMAT_IEEE_FLOAT: "float"}.get(tag)
    if tag == WAVE_FORMAT_EXTENSIBLE and len(payload) >= 40:
        # The container size (wBitsPerSample) decides the decoding; the
        # SubFormat GUID says integer or float.
        sub = payload[24:40]
        data1 = struct.unpack_from("<I", sub)[0]
        if sub[4:] == SUBTYPE_TAIL:
            kind = {WAVE_FORMAT_PCM: "int", WAVE_FORMAT_IEEE_FLOAT: "float"}.get(data1)
    return {"tag": tag, "channels": channels, "rate": rate, "avg_bytes": avg,
            "block_align": block, "bits": bits, "sample": kind}


def decode(data, fmt):
    """Frames x channels as float64, or None for a format this check cannot read."""
    channels, bits, kind = fmt["channels"], fmt["bits"], fmt["sample"]
    if not channels or fmt["block_align"] != channels * bits // 8:
        return None
    frames = len(data) // fmt["block_align"]
    data = data[:frames * fmt["block_align"]]
    if kind == "float" and bits == 32:
        samples = np.frombuffer(data, dtype="<f4").astype(np.float64)
    elif kind == "int" and bits == 16:
        samples = np.frombuffer(data, dtype="<i2") / 32768.0
    elif kind == "int" and bits == 32:
        samples = np.frombuffer(data, dtype="<i4") / 2147483648.0
    elif kind == "int" and bits == 24:
        raw = np.frombuffer(data, dtype=np.uint8).reshape(-1, 3).astype(np.int32)
        value = raw[:, 0] | (raw[:, 1] << 8) | (raw[:, 2] << 16)
        samples = np.where(value >= 1 << 23, value - (1 << 24), value) / float(1 << 23)
    else:
        return None
    return samples.reshape(frames, channels)


def check_endpoint(records, endpoint, qpc_frequency, tone_hz, other_hz, min_seconds):
    """Checks one endpoint's records; returns (summary, problems)."""
    mine = [r for r in records if r["endpoint"] == endpoint]
    problems = []
    summary = {"endpoint": endpoint, "records": len(mine)}
    kinds = [r["kind"] for r in mine]
    events = [r for r in mine if not r["flags"] & FLAG_REPEAT]
    data = [r for r in mine if r["kind"] == KIND_DATA]
    formats = [r for r in mine if r["kind"] == KIND_FORMAT_CHANGE]
    new_formats = [r for r in events if r["kind"] == KIND_FORMAT_CHANGE]
    volumes = [r for r in mine if r["kind"] == KIND_VOLUME]
    summary["volume_records"] = len(volumes)
    summary["volume_changes"] = sum(not r["flags"] & FLAG_REPEAT for r in volumes)
    summary["volume_repeats"] = sum(bool(r["flags"] & FLAG_REPEAT) for r in volumes)
    for volume in volumes:
        try:
            summary["volume"] = parse_volume(volume["payload"])
        except ValueError as error:
            problems.append(f"endpoint {endpoint}: {error}")
    shared = endpoint_stats(records, endpoint, qpc_frequency)
    summary["overruns"] = shared["overruns"]
    summary["starts"] = sum(r["kind"] == KIND_STREAM_START for r in events)
    summary["stops"] = sum(r["kind"] == KIND_STREAM_STOP for r in events)
    summary["repeats"] = sum(bool(r["flags"] & FLAG_REPEAT) for r in mine)
    if not data:
        return summary, problems + [f"endpoint {endpoint}: no data records"]
    first_data = kinds.index(KIND_DATA)
    if KIND_FORMAT_CHANGE not in kinds[:first_data]:
        problems.append(f"endpoint {endpoint}: data before any format change")
    if KIND_STREAM_START not in kinds[:first_data]:
        problems.append(f"endpoint {endpoint}: data before any stream start")
    # The format in force for the data: the last format change before the
    # first data record. A later change would be a test failure of its own.
    fmt = parse_wave_format(formats[-1]["payload"]) if formats else None
    if len(new_formats) > 1:
        problems.append(f"endpoint {endpoint}: {len(new_formats)} format changes while one stream played")
    summary["format"] = fmt

    decoded = decode(b"".join(r["payload"] for r in data), fmt) if fmt else None
    shared = endpoint_stats(records, endpoint, qpc_frequency, fmt,
                            (lambda _data, _fmt: decoded), tone_hz, other_hz)
    summary.update({key: round(value, 1) if key in ("rate_error_ppm", "tone_db", "other_tone_db") else value
                    for key, value in shared.items()})
    if "seconds" in summary:
        seconds = summary["seconds"]
        summary["seconds"] = round(seconds, 3)
        if seconds < min_seconds:
            problems.append(f"endpoint {endpoint}: only {seconds:.2f} s of data")
    if fmt and decoded is None:
        problems.append(f"endpoint {endpoint}: cannot decode {fmt}")
    problems.extend(threshold_problems(endpoint, shared))
    return summary, problems


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--seconds", type=float, default=3.0)
    parser.add_argument("--summary", required=True)
    parser.add_argument("--expect-mask", type=lambda value: int(value, 0), default=DEFAULT_ENDPOINT_MASK)
    args = parser.parse_args()
    available_mask = sum(1 << endpoint for endpoint in ENDPOINT_NUMBERS)
    if args.expect_mask <= 0 or args.expect_mask & ~available_mask:
        parser.error("--expect-mask must select endpoints 0..2")
    active_tones = {e: hz for e, hz in TONES.items() if args.expect_mask & (1 << e)}

    from numpy_compat import binary_fromstring
    np.fromstring = binary_fromstring
    import soundcard as sc

    speakers = {}
    deadline = time.monotonic() + 60
    while time.monotonic() < deadline:
        rows = enumerate_endpoints()
        speakers = {e: find_speaker(sc, e, rows) for e in active_tones}
        if all(speakers.values()):
            break
        time.sleep(1)
    print("speakers:", {e: s.name if s else None for e, s in speakers.items()}, flush=True)
    names = [s.name for s in sc.all_speakers()]
    if not all(speakers.values()):
        raise SystemExit(f"Spatial SoftReceiver endpoints did not appear; render endpoints: {names}")

    device = ControlDevice()
    info = device.query_info()
    print("QUERY_INFO:", info, flush=True)
    reads = []
    stop = threading.Event()
    errors = []
    playback_errors = []

    def reader():
        try:
            while not stop.is_set():
                reads.append(device.read(4096, 50))
        except OSError as error:
            errors.append(str(error))

    def player(endpoint):
        try:
            rate = 48_000
            t = np.arange(int(args.seconds * rate)) / rate
            tone = (FLOW_TONE_AMPLITUDE * np.sin(2 * np.pi * active_tones[endpoint] * t)).astype(np.float32)
            with speakers[endpoint].player(samplerate=rate, channels=2, blocksize=rate // 100) as out:
                out.play(np.stack([tone, tone], axis=1))
        except Exception as error:
            playback_errors.append(f"endpoint {endpoint}: shared playback failed: {error}")

    thread = threading.Thread(target=reader)
    thread.start()
    players = [threading.Thread(target=player, args=(e,)) for e in active_tones]
    for p in players:
        p.start()
    for p in players:
        p.join()
    time.sleep(1.0)
    stop.set()
    thread.join()
    stats = device.stats()
    device.close()

    stream = b"".join(reads)
    records = parse_records(stream)
    skew_us, skew_reads = snapshot_skew_us(reads, info["qpc_frequency"])
    summary = {"render_endpoints": rows, "query_info": info, "stats": stats, "read_errors": errors, "read_bytes": len(stream),
               "snapshot_skew_us": round(skew_us, 1), "snapshot_reads_compared": skew_reads,
               "playback_errors": playback_errors, "playback_mode": "shared",
               "passthrough_shared_policy": "allowed while idle; exclusive permission is not a shared-mode ban",
               "endpoints": []}
    problems = [f"READ failed: {e}" for e in errors] + playback_errors
    if info["abi_version"] != 1 or info["struct_size"] != ABI_INFO.size:
        problems.append(f"QUERY_INFO: abi {info['abi_version']}, size {info['struct_size']}")
    if info["endpoint_mask"] != args.expect_mask:
        problems.append(f"QUERY_INFO: mask 0x{info['endpoint_mask']:X}, expected 0x{args.expect_mask:X}")
    for endpoint, hz in active_tones.items():
        others = [other_hz for other_endpoint, other_hz in active_tones.items() if other_endpoint != endpoint]
        result, issues = check_endpoint(records, endpoint, info["qpc_frequency"], hz, others, args.seconds * 0.8)
        summary["endpoints"].append(result)
        problems += issues
    if len(active_tones) >= 2 and skew_reads == 0:
        problems.append("no READ held data from two endpoints to compare their QPCs")
    if skew_us > SNAPSHOT_SKEW_LIMIT_US:
        problems.append(f"a READ held endpoints {skew_us:.0f} us apart (limit {SNAPSHOT_SKEW_LIMIT_US:.0f} us)")
    with open(args.summary, "w", encoding="utf-8") as out:
        json.dump(summary, out, indent=2, default=str)
    print(json.dumps(summary, indent=2, default=str))
    step = os.environ.get("GITHUB_STEP_SUMMARY")
    if step:
        with open(step, "a", encoding="utf-8") as out:
            out.write("\n### Enabled endpoints through the driver ring\n\n")
            out.write("| endpoint | format | seconds | gaps | overruns | DMA rate error | own tone | other tone |\n")
            out.write("| --- | --- | --- | --- | --- | --- | --- | --- |\n")
            for r in summary["endpoints"]:
                fmt = r.get("format") or {}
                out.write(f"| {r['endpoint']} | {fmt.get('channels')} ch {fmt.get('rate')} Hz "
                          f"{fmt.get('sample')}{fmt.get('bits')} | {r.get('seconds')} | {r.get('gaps')} | "
                          f"{r.get('overruns')} | {r.get('rate_error_ppm')} ppm | {r.get('tone_db')} dBFS | "
                          f"{r.get('other_tone_db')} dBFS |\n")
            out.write(f"\nmax timer lateness {stats['max_timer_lateness_us']} us; "
                      f"READ snapshot skew {skew_us:.1f} us over {skew_reads} READs; "
                      f"{'FAIL: ' + '; '.join(problems) if problems else 'ok'}\n")
    for problem in problems:
        print(problem, file=sys.stderr)
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())

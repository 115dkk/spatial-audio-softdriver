"""Plays a tone into each BIO Atmos endpoint and reads it back from the driver.

The driver test job installs the test-signed package (ADR 0027). When the
runner's kernel loads it, this script plays a different tone into each render
endpoint at the same time through WASAPI shared mode, reads the record stream
from \\\\.\\BioAtmos with IOCTL_BIO_READ meanwhile, and checks per endpoint:

* a format change and a stream start came before the data,
* the data bytes follow each other (byte_pos) with no gap and no overrun,
* the virtual DMA moved at the format's byte rate (QPC of the last byte of
  each record against the bytes in between), and
* the endpoint's own tone is in its data and the other endpoint's is not.

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

IOCTL_BIO_QUERY_INFO = (0x22 << 16) | (1 << 14) | (0x900 << 2)
IOCTL_BIO_READ = (0x22 << 16) | (1 << 14) | (0x901 << 2) | 2
IOCTL_BIO_GET_STATS = (0x22 << 16) | (1 << 14) | (0x903 << 2)
READ_MAX_BYTES = 256 * 1024

KIND_DATA, KIND_FORMAT_CHANGE, KIND_STREAM_START, KIND_STREAM_STOP, KIND_OVERRUN = 1, 2, 3, 4, 5
FLAG_DISCONTINUITY = 0x0001
RECORD_ALIGN = 8

# crates/bio-abi/src/wire.rs: RecordHeader, AbiInfo, BioStats (MAX_ENDPOINTS = 2).
HEADER = struct.Struct("<HHIQQQIHH")
ABI_INFO = struct.Struct("<IIIIQQQII2Q")
ENDPOINT_STATS = struct.Struct("<QQQII")
STATS_HEAD = struct.Struct("<IIII")

WAVE_FORMAT_PCM = 0x0001
WAVE_FORMAT_IEEE_FLOAT = 0x0003
WAVE_FORMAT_EXTENSIBLE = 0xFFFE
# Data1 of the KSDATAFORMAT_SUBTYPE_* GUIDs {0000xxxx-0000-0010-8000-00aa00389b71}.
SUBTYPE_TAIL = bytes.fromhex("000010008000 00aa00389b71".replace(" ", ""))

TONES = {0: 1000.0, 1: 1500.0}


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
            "qpc": qpc, "len": length, "endpoint": endpoint,
            "payload": bytes(stream[offset + HEADER.size:end]),
        })
        offset = (end + RECORD_ALIGN - 1) // RECORD_ALIGN * RECORD_ALIGN
    return records


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


def tone_db(signal, rate, hz):
    """Level of `hz` in a mono signal, dBFS of a full-scale sine."""
    if len(signal) == 0:
        return -200.0
    t = np.arange(len(signal)) / rate
    amplitude = 2 * abs(np.dot(signal, np.exp(-2j * np.pi * hz * t))) / len(signal)
    return 20 * np.log10(max(amplitude, 1e-10))


def check_endpoint(records, endpoint, qpc_frequency, tone_hz, other_hz, min_seconds):
    """Checks one endpoint's records; returns (summary, problems)."""
    mine = [r for r in records if r["endpoint"] == endpoint]
    problems = []
    summary = {"endpoint": endpoint, "records": len(mine)}
    kinds = [r["kind"] for r in mine]
    data = [r for r in mine if r["kind"] == KIND_DATA]
    formats = [r for r in mine if r["kind"] == KIND_FORMAT_CHANGE]
    summary["overruns"] = kinds.count(KIND_OVERRUN)
    summary["starts"] = kinds.count(KIND_STREAM_START)
    summary["stops"] = kinds.count(KIND_STREAM_STOP)
    if not data:
        return summary, [f"endpoint {endpoint}: no data records"]
    first_data = kinds.index(KIND_DATA)
    if KIND_FORMAT_CHANGE not in kinds[:first_data]:
        problems.append(f"endpoint {endpoint}: data before any format change")
    if KIND_STREAM_START not in kinds[:first_data]:
        problems.append(f"endpoint {endpoint}: data before any stream start")
    if summary["overruns"]:
        problems.append(f"endpoint {endpoint}: {summary['overruns']} overrun records")

    # The format in force for the data: the last format change before the
    # first data record. A later change would be a test failure of its own.
    fmt = parse_wave_format(formats[-1]["payload"]) if formats else None
    if len(formats) > 1:
        problems.append(f"endpoint {endpoint}: {len(formats)} format changes while one stream played")
    summary["format"] = fmt

    gaps = 0
    for previous, record in zip(data, data[1:]):
        if record["flags"] & FLAG_DISCONTINUITY or record["byte_pos"] != previous["byte_pos"] + previous["len"]:
            gaps += 1
    summary["gaps"] = gaps
    if gaps:
        problems.append(f"endpoint {endpoint}: {gaps} gaps in byte_pos")

    total = sum(r["len"] for r in data)
    summary["bytes"] = total
    if fmt:
        seconds = total / fmt["avg_bytes"] if fmt["avg_bytes"] else 0.0
        summary["seconds"] = round(seconds, 3)
        if seconds < min_seconds:
            problems.append(f"endpoint {endpoint}: only {seconds:.2f} s of data")
        # Bytes after the first record over the QPC between its last byte and
        # the last record's last byte: the virtual DMA's byte rate.
        span = (data[-1]["qpc"] - data[0]["qpc"]) / qpc_frequency
        if span > 0.5 and fmt["avg_bytes"]:
            measured = (total - data[0]["len"]) / span
            ppm = (measured / fmt["avg_bytes"] - 1) * 1e6
            summary["rate_error_ppm"] = round(ppm, 1)
            if abs(ppm) > 2000:
                problems.append(f"endpoint {endpoint}: DMA moved {ppm:+.0f} ppm off the format's byte rate")
        samples = decode(b"".join(r["payload"] for r in data), fmt)
        if samples is None:
            problems.append(f"endpoint {endpoint}: cannot decode {fmt}")
        else:
            front = samples[:, 0]
            # Skip the stream's leading silence (the shared engine primes it).
            loud = np.flatnonzero(np.abs(front) > 1e-3)
            front = front[loud[0]:loud[-1] + 1] if len(loud) else front[:0]
            own, other = tone_db(front, fmt["rate"], tone_hz), tone_db(front, fmt["rate"], other_hz)
            summary["tone_db"], summary["other_tone_db"] = round(own, 1), round(other, 1)
            if own < -30:
                problems.append(f"endpoint {endpoint}: its {tone_hz:.0f} Hz tone is at {own:.1f} dBFS")
            if other > own - 40:
                problems.append(f"endpoint {endpoint}: the other endpoint's tone leaked in ({other:.1f} dBFS)")
    return summary, problems


class ControlDevice:
    """\\\\.\\BioAtmos through kernel32 (Windows only)."""

    def __init__(self):
        import ctypes
        from ctypes import wintypes

        self.ctypes = ctypes
        kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
        kernel32.CreateFileW.restype = wintypes.HANDLE
        kernel32.CreateFileW.argtypes = [wintypes.LPCWSTR, wintypes.DWORD, wintypes.DWORD, wintypes.LPVOID,
                                         wintypes.DWORD, wintypes.DWORD, wintypes.HANDLE]
        kernel32.DeviceIoControl.restype = wintypes.BOOL
        kernel32.DeviceIoControl.argtypes = [wintypes.HANDLE, wintypes.DWORD, wintypes.LPVOID, wintypes.DWORD,
                                             wintypes.LPVOID, wintypes.DWORD, ctypes.POINTER(wintypes.DWORD),
                                             wintypes.LPVOID]
        kernel32.CloseHandle.argtypes = [wintypes.HANDLE]
        self.kernel32 = kernel32
        generic_read, generic_write, open_existing = 0x80000000, 0x40000000, 3
        handle = kernel32.CreateFileW(r"\\.\BioAtmos", generic_read | generic_write, 0, None, open_existing, 0, None)
        if handle in (None, wintypes.HANDLE(-1).value):
            raise OSError(ctypes.get_last_error(), "CreateFileW(\\\\.\\BioAtmos)")
        self.handle = handle

    def ioctl(self, code, request, capacity):
        ctypes = self.ctypes
        out = ctypes.create_string_buffer(capacity)
        returned = ctypes.c_ulong(0)
        inp = ctypes.create_string_buffer(request, len(request)) if request else None
        ok = self.kernel32.DeviceIoControl(self.handle, code, inp, len(request), out, capacity,
                                           ctypes.byref(returned), None)
        if not ok:
            raise OSError(ctypes.get_last_error(), f"DeviceIoControl(0x{code:08X})")
        return out.raw[:returned.value]

    def query_info(self):
        names = ["abi_version", "struct_size", "driver_version", "ring_bytes", "compiled_formats", "features",
                 "qpc_frequency", "endpoint_count", "reserved", "formats0", "formats1"]
        return dict(zip(names, ABI_INFO.unpack(self.ioctl(IOCTL_BIO_QUERY_INFO, b"", ABI_INFO.size))))

    def stats(self):
        raw = self.ioctl(IOCTL_BIO_GET_STATS, b"", STATS_HEAD.size + 2 * ENDPOINT_STATS.size)
        size, opens, lateness, _ = STATS_HEAD.unpack_from(raw)
        endpoints = [dict(zip(["total_bytes", "overruns", "lost_bytes", "epoch", "state"],
                              ENDPOINT_STATS.unpack_from(raw, STATS_HEAD.size + i * ENDPOINT_STATS.size)))
                     for i in range(2)]
        return {"struct_size": size, "open_count": opens, "max_timer_lateness_us": lateness, "endpoints": endpoints}

    def read(self, min_bytes, timeout_ms):
        return self.ioctl(IOCTL_BIO_READ, struct.pack("<II", min_bytes, timeout_ms), READ_MAX_BYTES)

    def close(self):
        self.kernel32.CloseHandle(self.handle)


def find_speaker(sc, endpoint):
    """The render endpoint whose name carries our endpoint name (ADR 0024)."""
    for speaker in sc.all_speakers():
        name = speaker.name
        if "BIO Atmos Virtual HDMI" not in name:
            continue
        if ("HDMI 2" in name) == (endpoint == 1):
            return speaker
    return None


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--seconds", type=float, default=3.0)
    parser.add_argument("--summary", required=True)
    args = parser.parse_args()

    from loopback_check import binary_fromstring
    np.fromstring = binary_fromstring
    import soundcard as sc

    speakers = {}
    deadline = time.monotonic() + 60
    while time.monotonic() < deadline:
        speakers = {e: find_speaker(sc, e) for e in TONES}
        if all(speakers.values()):
            break
        time.sleep(1)
    print("speakers:", {e: s.name if s else None for e, s in speakers.items()}, flush=True)
    names = [s.name for s in sc.all_speakers()]
    if not all(speakers.values()):
        raise SystemExit(f"BIO Atmos endpoints did not appear; render endpoints: {names}")

    device = ControlDevice()
    info = device.query_info()
    print("QUERY_INFO:", info, flush=True)
    stream = bytearray()
    stop = threading.Event()
    errors = []

    def reader():
        try:
            while not stop.is_set():
                stream.extend(device.read(4096, 50))
        except OSError as error:
            errors.append(str(error))

    def player(endpoint):
        rate = 48_000
        t = np.arange(int(args.seconds * rate)) / rate
        tone = (0.5 * np.sin(2 * np.pi * TONES[endpoint] * t)).astype(np.float32)
        with speakers[endpoint].player(samplerate=rate, channels=2, blocksize=rate // 100) as out:
            out.play(np.stack([tone, tone], axis=1))

    thread = threading.Thread(target=reader)
    thread.start()
    players = [threading.Thread(target=player, args=(e,)) for e in TONES]
    for p in players:
        p.start()
    for p in players:
        p.join()
    time.sleep(1.0)
    stop.set()
    thread.join()
    stats = device.stats()
    device.close()

    records = parse_records(bytes(stream))
    summary = {"query_info": info, "stats": stats, "read_errors": errors, "read_bytes": len(stream),
               "endpoints": []}
    problems = [f"READ failed: {e}" for e in errors]
    if info["abi_version"] != 1 or info["struct_size"] != ABI_INFO.size:
        problems.append(f"QUERY_INFO: abi {info['abi_version']}, size {info['struct_size']}")
    if info["endpoint_count"] != 2:
        problems.append(f"QUERY_INFO: {info['endpoint_count']} endpoints, expected 2")
    for endpoint, hz in TONES.items():
        other = TONES[1 - endpoint]
        result, issues = check_endpoint(records, endpoint, info["qpc_frequency"], hz, other, args.seconds * 0.8)
        summary["endpoints"].append(result)
        problems += issues
    with open(args.summary, "w", encoding="utf-8") as out:
        json.dump(summary, out, indent=2, default=str)
    print(json.dumps(summary, indent=2, default=str))
    step = os.environ.get("GITHUB_STEP_SUMMARY")
    if step:
        with open(step, "a", encoding="utf-8") as out:
            out.write("\n### Both endpoints through the driver ring\n\n")
            out.write("| endpoint | format | seconds | gaps | overruns | DMA rate error | own tone | other tone |\n")
            out.write("| --- | --- | --- | --- | --- | --- | --- | --- |\n")
            for r in summary["endpoints"]:
                fmt = r.get("format") or {}
                out.write(f"| {r['endpoint']} | {fmt.get('channels')} ch {fmt.get('rate')} Hz "
                          f"{fmt.get('sample')}{fmt.get('bits')} | {r.get('seconds')} | {r.get('gaps')} | "
                          f"{r.get('overruns')} | {r.get('rate_error_ppm')} ppm | {r.get('tone_db')} dBFS | "
                          f"{r.get('other_tone_db')} dBFS |\n")
            out.write(f"\nmax timer lateness {stats['max_timer_lateness_us']} us; "
                      f"{'FAIL: ' + '; '.join(problems) if problems else 'ok'}\n")
    for problem in problems:
        print(problem, file=sys.stderr)
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())

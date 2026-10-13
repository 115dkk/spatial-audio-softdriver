"""Fuzz the test-signed Spatial SoftReceiver control device under Driver Verifier.

This is deliberately a coarse ABI fuzzer rather than a coverage-guided kernel
fuzzer. It attacks buffer lengths, payload fields, IOCTL numbers, cancellation,
close races, and exclusive-open lifetime, then verifies that the device remains
usable. The pure case generation and expectation table are tested on Linux.
"""

import argparse
import ctypes
from ctypes import wintypes
import hashlib
import json
import os
import random
import struct
import sys
import threading
import time

from endpoints import CONTROL_DEVICE_PATH

FILE_DEVICE_UNKNOWN = 0x22
METHOD_BUFFERED, METHOD_IN_DIRECT, METHOD_OUT_DIRECT, METHOD_NEITHER = range(4)
FILE_ANY_ACCESS, FILE_READ_ACCESS, FILE_WRITE_ACCESS = 0, 1, 2


def ctl_code(device_type, function, method, access):
    return (device_type << 16) | (access << 14) | (function << 2) | method


IOCTL_BIO_QUERY_INFO = ctl_code(FILE_DEVICE_UNKNOWN, 0x900, METHOD_BUFFERED, FILE_READ_ACCESS)
IOCTL_BIO_READ = ctl_code(FILE_DEVICE_UNKNOWN, 0x901, METHOD_OUT_DIRECT, FILE_READ_ACCESS)
IOCTL_BIO_SET_RATE_TRIM = ctl_code(FILE_DEVICE_UNKNOWN, 0x902, METHOD_BUFFERED, FILE_WRITE_ACCESS)
IOCTL_BIO_GET_STATS = ctl_code(FILE_DEVICE_UNKNOWN, 0x903, METHOD_BUFFERED, FILE_READ_ACCESS)

ABI_VERSION = 1
ABI_INFO_SIZE = 160
READ_REQUEST_SIZE = 8
RATE_TRIM_SIZE = 8
BIO_STATS_SIZE = 144
RECORD_HEADER_SIZE = 40
READ_MAX_BYTES = 256 * 1024
TRIM_LIMIT_Q16 = 2000 * 65536
ERROR_IO_PENDING = 997
ERROR_OPERATION_ABORTED = 995
ERROR_SHARING_VIOLATION = 32
WAIT_TIMEOUT = 0x102
# A READ pends until min_bytes are queued or timeout_ms passes (both may be
# 0xFFFFFFFF in the corpus) and no audio flows while the fuzzer runs, so a
# request still pending after PENDING_WAIT_MS is cancelled. The driver must
# then complete it within CANCEL_WAIT_MS.
PENDING_WAIT_MS = 100
CANCEL_WAIT_MS = 5000

KNOWN = {
    "QUERY_INFO": IOCTL_BIO_QUERY_INFO,
    "READ": IOCTL_BIO_READ,
    "SET_RATE_TRIM": IOCTL_BIO_SET_RATE_TRIM,
    "GET_STATS": IOCTL_BIO_GET_STATS,
}
STRUCT_SIZES = {
    "QUERY_INFO": (0, ABI_INFO_SIZE),
    "READ": (READ_REQUEST_SIZE, RECORD_HEADER_SIZE),
    "SET_RATE_TRIM": (RATE_TRIM_SIZE, 0),
    "GET_STATS": (0, BIO_STATS_SIZE),
}

# Mirrors driver/bioatmos/control.cpp. QUERY_INFO and GET_STATS ignore input;
# SET_RATE_TRIM accepts extensions but reserved must be zero; READ accepts
# extensions and any non-empty output MDL. Runtime ring state can still turn an
# otherwise accepted READ into BUFFER_TOO_SMALL, so only successes are judged.
def expects_success(name, input_size, output_size, reserved=0):
    if name == "QUERY_INFO":
        return output_size >= ABI_INFO_SIZE
    if name == "GET_STATS":
        return output_size >= BIO_STATS_SIZE
    if name == "SET_RATE_TRIM":
        return input_size >= RATE_TRIM_SIZE and reserved == 0
    if name == "READ":
        return input_size >= READ_REQUEST_SIZE and output_size > 0
    return False


INPUT_SIZES = (0, 1, 7, 8, 9, 4096, 64 * 1024, 1024 * 1024)
OUTPUT_SIZES = (0, 1, RECORD_HEADER_SIZE - 1, RECORD_HEADER_SIZE,
                READ_MAX_BYTES, READ_MAX_BYTES + 1)
READ_EXTREMES = (0, 1, 0x7FFFFFFF, 0xFFFFFFFF)
TRIM_EXTREMES = (-TRIM_LIMIT_Q16, TRIM_LIMIT_Q16, 0,
                 -TRIM_LIMIT_Q16 - 1, TRIM_LIMIT_Q16 + 1,
                 -(1 << 31), (1 << 31) - 1)


def deterministic_cases(seed):
    """Returns a deterministic finite corpus; random bytes are part of it."""
    rng = random.Random(seed)
    cases = []
    for name, code in KNOWN.items():
        natural_in, natural_out = STRUCT_SIZES[name]
        sizes = sorted(set(INPUT_SIZES + (max(0, natural_in - 1), natural_in, natural_in + 1)))
        outputs = sorted(set(OUTPUT_SIZES + (max(0, natural_out - 1), natural_out, natural_out + 1)))
        for size in sizes:
            payload = bytes(rng.getrandbits(8) for _ in range(size))
            for output in outputs:
                reserved = 0
                if name == "READ" and size >= READ_REQUEST_SIZE:
                    payload = struct.pack("<II", rng.choice(READ_EXTREMES), rng.choice(READ_EXTREMES))
                    payload += bytes(rng.getrandbits(8) for _ in range(size - READ_REQUEST_SIZE))
                elif name == "SET_RATE_TRIM" and size >= RATE_TRIM_SIZE:
                    reserved = rng.choice((0, 0, 1, 0xFFFFFFFF))
                    payload = struct.pack("<iI", rng.choice(TRIM_EXTREMES), reserved)
                    payload += bytes(rng.getrandbits(8) for _ in range(size - RATE_TRIM_SIZE))
                cases.append({
                    "kind": "known", "name": name, "code": code,
                    "input": payload, "output_size": output,
                    "reserved": reserved,
                    "expect_success": expects_success(name, size, output, reserved),
                })
    return cases


def unknown_code(rng):
    choice = rng.randrange(3)
    if choice == 0:
        function = rng.choice(tuple(range(0x800, 0x900)) + tuple(range(0x904, 0x1000)))
        return ctl_code(FILE_DEVICE_UNKNOWN, function, rng.randrange(4), rng.randrange(4))
    if choice == 1:
        while True:
            base = rng.choice(tuple(KNOWN.values()))
            code = (base & ~0xC003) | rng.randrange(4) | (rng.randrange(4) << 14)
            if code not in KNOWN.values():
                return code
    while True:
        code = rng.getrandbits(32)
        if code not in KNOWN.values():
            return code


def case_fingerprint(cases):
    digest = hashlib.sha256()
    for case in cases:
        digest.update(struct.pack("<III?", case["code"], len(case["input"]),
                                  case["output_size"], case["expect_success"]))
        digest.update(case["input"])
    return digest.hexdigest()


def new_summary(seed, seconds):
    return {
        "schema_version": 1,
        "seed": seed,
        "seconds": seconds,
        "operations": 0,
        "status_counts": {},
        "case_counts": {},
        "unexpected_successes": [],
        "cancelled_pending": 0,
        "checks": {},
    }


def record_result(summary, case, success, status, returned):
    key = "SUCCESS" if success else "WIN32_{0}".format(status)
    summary["status_counts"][key] = summary["status_counts"].get(key, 0) + 1
    kind = case["kind"]
    summary["case_counts"][kind] = summary["case_counts"].get(kind, 0) + 1
    summary["operations"] += 1
    if success and not case["expect_success"]:
        summary["unexpected_successes"].append({
            "kind": kind,
            "name": case.get("name"),
            "code": "0x{0:08X}".format(case["code"]),
            "input_size": len(case["input"]),
            "output_size": case["output_size"],
            "returned": returned,
        })


class OVERLAPPED(ctypes.Structure):
    _fields_ = [
        ("Internal", ctypes.c_size_t),
        ("InternalHigh", ctypes.c_size_t),
        ("Offset", wintypes.DWORD),
        ("OffsetHigh", wintypes.DWORD),
        ("hEvent", wintypes.HANDLE),
    ]


class WindowsDevice:
    def __init__(self, overlapped=False):
        if os.name != "nt":
            raise RuntimeError("ioctl_fuzz.py device execution requires Windows")
        self.kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
        self._bind()
        flags = 0x40000000 if overlapped else 0
        self.handle = self.kernel32.CreateFileW(
            CONTROL_DEVICE_PATH, 0x80000000 | 0x40000000, 0, None, 3, flags, None)
        if self.handle in (None, wintypes.HANDLE(-1).value):
            raise OSError(ctypes.get_last_error(), "CreateFileW(\\\\.\\SpatialSoftReceiver)")
        self.overlapped = overlapped
        self.closed = False
        self.stuck = []

    def _bind(self):
        k = self.kernel32
        k.CreateFileW.restype = wintypes.HANDLE
        k.CreateFileW.argtypes = [wintypes.LPCWSTR, wintypes.DWORD, wintypes.DWORD,
                                  wintypes.LPVOID, wintypes.DWORD, wintypes.DWORD,
                                  wintypes.HANDLE]
        k.DeviceIoControl.restype = wintypes.BOOL
        k.DeviceIoControl.argtypes = [wintypes.HANDLE, wintypes.DWORD, wintypes.LPVOID,
                                      wintypes.DWORD, wintypes.LPVOID, wintypes.DWORD,
                                      ctypes.POINTER(wintypes.DWORD), wintypes.LPVOID]
        k.CancelIoEx.restype = wintypes.BOOL
        k.CancelIoEx.argtypes = [wintypes.HANDLE, wintypes.LPVOID]
        k.GetOverlappedResult.restype = wintypes.BOOL
        k.GetOverlappedResult.argtypes = [wintypes.HANDLE, ctypes.POINTER(OVERLAPPED),
                                          ctypes.POINTER(wintypes.DWORD), wintypes.BOOL]
        k.CreateEventW.restype = wintypes.HANDLE
        k.CloseHandle.restype = wintypes.BOOL
        k.WaitForSingleObject.restype = wintypes.DWORD
        k.WaitForSingleObject.argtypes = [wintypes.HANDLE, wintypes.DWORD]

    def ioctl(self, code, request, capacity):
        out = ctypes.create_string_buffer(capacity) if capacity else None
        inp = ctypes.create_string_buffer(request, len(request)) if request else None
        returned = wintypes.DWORD()
        ok = self.kernel32.DeviceIoControl(
            self.handle, code, inp, len(request), out, capacity,
            ctypes.byref(returned), None)
        if not ok:
            return False, ctypes.get_last_error(), b"", 0
        return True, 0, out.raw[:returned.value] if out else b"", returned.value

    def begin(self, code, request, capacity):
        out = ctypes.create_string_buffer(capacity) if capacity else None
        inp = ctypes.create_string_buffer(request, len(request)) if request else None
        event = self.kernel32.CreateEventW(None, True, False, None)
        if not event:
            raise ctypes.WinError(ctypes.get_last_error())
        ov = OVERLAPPED(hEvent=event)
        returned = wintypes.DWORD()
        ok = self.kernel32.DeviceIoControl(
            self.handle, code, inp, len(request), out, capacity,
            ctypes.byref(returned), ctypes.byref(ov))
        error = 0 if ok else ctypes.get_last_error()
        return {"ov": ov, "event": event, "out": out, "inp": inp,
                "immediate": bool(ok), "error": error, "returned": returned.value}

    def finish(self, operation, wait=True):
        returned = wintypes.DWORD(operation["returned"])
        if operation["immediate"]:
            ok, error = True, 0
        elif operation["error"] != ERROR_IO_PENDING:
            ok, error = False, operation["error"]
        else:
            ok = bool(self.kernel32.GetOverlappedResult(
                self.handle, ctypes.byref(operation["ov"]), ctypes.byref(returned), wait))
            error = 0 if ok else ctypes.get_last_error()
        self.kernel32.CloseHandle(operation["event"])
        out = operation["out"]
        return ok, error, out.raw[:returned.value] if ok and out else b"", returned.value

    def bounded_ioctl(self, code, request, capacity):
        """Issues one request on an overlapped handle. One still pending after
        PENDING_WAIT_MS is cancelled; returns (success, status, data, returned,
        cancelled). Returns None, leaving the operation referenced so its
        buffers outlive the IRP, if the driver did not complete the cancel."""
        operation = self.begin(code, request, capacity)
        cancelled = False
        if not operation["immediate"] and operation["error"] == ERROR_IO_PENDING:
            if self.kernel32.WaitForSingleObject(operation["event"], PENDING_WAIT_MS) == WAIT_TIMEOUT:
                self.cancel(operation)
                cancelled = True
                if self.kernel32.WaitForSingleObject(operation["event"], CANCEL_WAIT_MS) == WAIT_TIMEOUT:
                    self.stuck.append(operation)
                    return None
        ok, error, data, returned = self.finish(operation)
        return ok, error, data, returned, cancelled

    def cancel(self, operation=None):
        pointer = ctypes.byref(operation["ov"]) if operation else None
        return bool(self.kernel32.CancelIoEx(self.handle, pointer))

    def close_racing(self):
        if not self.closed:
            self.kernel32.CloseHandle(self.handle)
            self.closed = True

    def close(self):
        if not self.closed:
            self.kernel32.CloseHandle(self.handle)
            self.closed = True


def exercise_exclusive_open(summary):
    first = WindowsDevice()
    try:
        try:
            second = WindowsDevice()
        except OSError as error:
            summary["checks"]["second_open_sharing_violation"] = error.errno == ERROR_SHARING_VIOLATION
        else:
            second.close()
            summary["checks"]["second_open_sharing_violation"] = False
    finally:
        first.close()
    reopened = WindowsDevice()
    reopened.close()
    summary["checks"]["reopen_after_close"] = True


def exercise_races(summary, rng):
    device = WindowsDevice(overlapped=True)
    try:
        for action in ("cancel", "stats_trim", "close"):
            request = struct.pack("<II", READ_MAX_BYTES, 0xFFFFFFFF)
            operation = device.begin(IOCTL_BIO_READ, request, READ_MAX_BYTES)
            if action == "cancel":
                thread = threading.Thread(target=device.cancel, args=(operation,))
                thread.start()
                thread.join()
                device.finish(operation)
            elif action == "stats_trim":
                stats = device.begin(IOCTL_BIO_GET_STATS, b"", BIO_STATS_SIZE)
                trim = device.begin(IOCTL_BIO_SET_RATE_TRIM,
                                    struct.pack("<iI", rng.choice(TRIM_EXTREMES), 0), 0)
                device.cancel(operation)
                device.finish(stats)
                device.finish(trim)
                device.finish(operation)
            else:
                thread = threading.Thread(target=device.close_racing)
                thread.start()
                thread.join()
                # Close cancels the IRP and invalidates waiting through this handle.
                device.kernel32.CloseHandle(operation["event"])
                break
        summary["checks"]["overlapped_races_completed"] = True
    finally:
        device.close()


def final_health(summary):
    device = WindowsDevice()
    try:
        ok, error, info, returned = device.ioctl(IOCTL_BIO_QUERY_INFO, b"", ABI_INFO_SIZE)
        info_good = ok and returned == ABI_INFO_SIZE
        if info_good:
            abi_version, struct_size = struct.unpack_from("<II", info)
            info_good = abi_version == ABI_VERSION and struct_size == ABI_INFO_SIZE
        summary["checks"]["query_info"] = info_good
        ok, error, stats, returned = device.ioctl(IOCTL_BIO_GET_STATS, b"", BIO_STATS_SIZE)
        stats_good = ok and returned == BIO_STATS_SIZE and struct.unpack_from("<I", stats)[0] == BIO_STATS_SIZE
        summary["checks"]["get_stats"] = stats_good
        started = time.monotonic()
        ok, _, _, _ = device.ioctl(IOCTL_BIO_READ, struct.pack("<II", 1, 50), READ_MAX_BYTES)
        summary["checks"]["read_bounded"] = ok and time.monotonic() - started < 2.0
    finally:
        device.close()


def run(seed, seconds):
    rng = random.Random(seed)
    summary = new_summary(seed, seconds)
    exercise_exclusive_open(summary)
    device = WindowsDevice(overlapped=True)
    corpus = deterministic_cases(seed)
    deadline = time.monotonic() + seconds
    index = 0
    summary["checks"]["pending_requests_cancel"] = True
    try:
        while time.monotonic() < deadline:
            if index % 5:
                case = corpus[index % len(corpus)]
            else:
                case = {
                    "kind": "unknown", "name": None, "code": unknown_code(rng),
                    "input": bytes(rng.getrandbits(8) for _ in range(rng.choice(INPUT_SIZES))),
                    "output_size": rng.choice(OUTPUT_SIZES), "reserved": 0,
                    "expect_success": False,
                }
            result = device.bounded_ioctl(case["code"], case["input"], case["output_size"])
            if result is None:
                summary["checks"]["pending_requests_cancel"] = False
                summary["stuck_request"] = {
                    "name": case.get("name"), "code": "0x{0:08X}".format(case["code"]),
                    "input_size": len(case["input"]), "output_size": case["output_size"],
                }
                return summary
            success, status, _, returned, cancelled = result
            summary["cancelled_pending"] += int(cancelled)
            record_result(summary, case, success, status, returned)
            index += 1
    finally:
        if not device.stuck:
            device.close()
    exercise_races(summary, rng)
    final_health(summary)
    return summary


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--seed", type=lambda value: int(value, 0),
                        default=None)
    parser.add_argument("--seconds", type=float, default=60.0)
    parser.add_argument("--summary", required=True)
    args = parser.parse_args()
    if args.seconds <= 0:
        parser.error("--seconds must be positive")
    seed = args.seed if args.seed is not None else time.time_ns() & ((1 << 64) - 1)
    print("IOCTL fuzz seed: {0}".format(seed), flush=True)
    summary = run(seed, args.seconds)
    with open(args.summary, "w", encoding="utf-8") as output:
        json.dump(summary, output, indent=2, sort_keys=True)
        output.write("\n")
    print(json.dumps(summary, indent=2, sort_keys=True))
    failed_checks = [name for name, passed in summary["checks"].items() if not passed]
    return 1 if summary["unexpected_successes"] or failed_checks else 0


if __name__ == "__main__":
    sys.exit(main())

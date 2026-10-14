"""Shared Windows control-device access and ABI response parsing."""

import ctypes
from ctypes import wintypes

from ssr_abi import (
    ABI_INFO, ABI_VERSION, CONTROL_DEVICE_PATH, DRIVER_STATS, ENDPOINT_FORMATS,
    ENDPOINT_STATS, IOCTL_SSR_GET_STATS, IOCTL_SSR_QUERY_INFO, IOCTL_SSR_READ,
    MAX_ENDPOINTS, READ_MAX_BYTES, READ_REQUEST,
)


def _formats(raw, offset):
    codecs, rates, shapes, maximum, direct = ENDPOINT_FORMATS.unpack_from(raw, offset)
    return {"codecs": codecs, "pcm": {"rates": rates, "shapes": shapes,
            "max_masked_channels": maximum, "direct_channels": direct}}


def parse_abi_info(raw):
    if len(raw) != ABI_INFO.size:
        raise ValueError(f"QUERY_INFO must be {ABI_INFO.size} bytes")
    values = ABI_INFO.unpack(raw)
    result = dict(zip(ABI_INFO_FIELDS, values))
    return {
        "abi_version": result["abi_version"], "struct_size": result["struct_size"],
        "driver_version": result["driver_version"], "ring_bytes": result["ring_bytes"],
        "compiled": _formats(raw, 16), "features": result["features"],
        "qpc_frequency": result["qpc_frequency"], "endpoint_mask": result["endpoint_mask"],
        "reserved": result["reserved"],
        "endpoints": [_formats(raw, 64 + index * ENDPOINT_FORMATS.size)
                      for index in range(MAX_ENDPOINTS)],
    }


def parse_stats(raw):
    if len(raw) != DRIVER_STATS.size:
        raise ValueError(f"GET_STATS must be {DRIVER_STATS.size} bytes")
    size, opens, lateness, _ = DRIVER_STATS.unpack(raw)[:4]
    endpoints = [dict(zip(ENDPOINT_STATS_FIELDS,
                          ENDPOINT_STATS.unpack_from(raw, 16 + index * ENDPOINT_STATS.size)))
                 for index in range(MAX_ENDPOINTS)]
    return {"struct_size": size, "open_count": opens,
            "max_timer_lateness_us": lateness, "endpoints": endpoints}


# Imported separately so the generated module remains the only field-name source.
from ssr_abi import ABI_INFO_FIELDS, ENDPOINT_STATS_FIELDS  # noqa: E402


class ControlDevice:
    r"""Open and own one ``\\.\SpatialSoftReceiver`` handle."""

    def __init__(self):
        kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
        kernel32.CreateFileW.restype = wintypes.HANDLE
        kernel32.CreateFileW.argtypes = [wintypes.LPCWSTR, wintypes.DWORD, wintypes.DWORD,
                                         wintypes.LPVOID, wintypes.DWORD, wintypes.DWORD,
                                         wintypes.HANDLE]
        kernel32.DeviceIoControl.restype = wintypes.BOOL
        kernel32.DeviceIoControl.argtypes = [wintypes.HANDLE, wintypes.DWORD, wintypes.LPVOID,
                                             wintypes.DWORD, wintypes.LPVOID, wintypes.DWORD,
                                             ctypes.POINTER(wintypes.DWORD), wintypes.LPVOID]
        kernel32.CloseHandle.argtypes = [wintypes.HANDLE]
        self.kernel32 = kernel32
        self.handle = kernel32.CreateFileW(CONTROL_DEVICE_PATH, 0xC0000000, 0, None, 3, 0, None)
        if self.handle in (None, wintypes.HANDLE(-1).value):
            raise OSError(ctypes.get_last_error(), f"CreateFileW({CONTROL_DEVICE_PATH})")
        self.closed = False

    def ioctl(self, code, request=b"", capacity=0):
        out = ctypes.create_string_buffer(capacity)
        returned = wintypes.DWORD(0)
        inp = ctypes.create_string_buffer(request, len(request)) if request else None
        if not self.kernel32.DeviceIoControl(self.handle, code, inp, len(request), out, capacity,
                                             ctypes.byref(returned), None):
            raise OSError(ctypes.get_last_error(), f"DeviceIoControl(0x{code:08X})")
        return out.raw[:returned.value]

    def query_info(self):
        info = parse_abi_info(self.ioctl(IOCTL_SSR_QUERY_INFO, capacity=ABI_INFO.size))
        if info["abi_version"] != ABI_VERSION or info["struct_size"] != ABI_INFO.size:
            raise ValueError(f"QUERY_INFO ABI {info['abi_version']}, size {info['struct_size']}")
        return info

    def stats(self):
        return parse_stats(self.ioctl(IOCTL_SSR_GET_STATS, capacity=DRIVER_STATS.size))

    def read(self, min_bytes, timeout_ms):
        return self.ioctl(IOCTL_SSR_READ, READ_REQUEST.pack(min_bytes, timeout_ms), READ_MAX_BYTES)

    def close(self):
        if not self.closed:
            self.kernel32.CloseHandle(self.handle)
            self.closed = True

    def __enter__(self):
        return self

    def __exit__(self, *_):
        self.close()

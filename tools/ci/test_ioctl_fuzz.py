import json
from pathlib import Path
import tempfile
import unittest
from unittest import mock

import ioctl_fuzz
from ioctl_fuzz import (
    ABI_INFO_SIZE,
    BIO_STATS_SIZE,
    ERROR_IO_PENDING,
    ERROR_OPERATION_ABORTED,
    WAIT_TIMEOUT,
    IOCTL_BIO_GET_STATS,
    IOCTL_BIO_QUERY_INFO,
    IOCTL_BIO_READ,
    IOCTL_BIO_SET_RATE_TRIM,
    RATE_TRIM_SIZE,
    READ_REQUEST_SIZE,
    WindowsDevice,
    case_fingerprint,
    deterministic_cases,
    expects_success,
    new_summary,
    record_result,
)


ROOT = Path(__file__).resolve().parents[2]
CONTROL = ROOT / "driver" / "bioatmos" / "control.cpp"


class PendingKernel:
    """kernel32 for one overlapped request that the driver pends: each wait
    returns the next of `waits`, and the result is `completed_ok`."""

    def __init__(self, waits, completed_ok=False):
        self.waits = list(waits)
        self.completed_ok = completed_ok
        self.cancels = 0
        self.closed = 0

    def CreateEventW(self, *_):
        return 1

    def DeviceIoControl(self, *_):
        return False

    def WaitForSingleObject(self, _event, _milliseconds):
        return self.waits.pop(0)

    def CancelIoEx(self, *_):
        self.cancels += 1
        return True

    def GetOverlappedResult(self, *_):
        return self.completed_ok

    def CloseHandle(self, *_):
        self.closed += 1
        return True


def pending_device(kernel):
    device = WindowsDevice.__new__(WindowsDevice)
    device.kernel32 = kernel
    device.handle = 1
    device.overlapped = True
    device.closed = False
    device.stuck = []
    return device


class IoctlFuzzTests(unittest.TestCase):
    def test_expectation_table_matches_control_checks(self):
        source = CONTROL.read_text(encoding="utf-8")
        snippets = [
            "if (input < sizeof(BIO_READ_REQUEST)) status = STATUS_INVALID_PARAMETER;",
            "else if (!output || !irp->MdlAddress) status = STATUS_BUFFER_TOO_SMALL;",
            "if (output < sizeof(BIO_ABI_INFO)) { status = STATUS_BUFFER_TOO_SMALL; break; }",
            "if (output < sizeof(BIO_STATS)) status = STATUS_BUFFER_TOO_SMALL;",
            "if (input < sizeof(BIO_RATE_TRIM) || static_cast<BIO_RATE_TRIM*>(buffer)->reserved) status = STATUS_INVALID_PARAMETER;",
            "default: status = STATUS_INVALID_DEVICE_REQUEST; break;",
        ]
        for snippet in snippets:
            self.assertIn(snippet, source)
        self.assertTrue(expects_success("QUERY_INFO", 0, ABI_INFO_SIZE))
        self.assertFalse(expects_success("QUERY_INFO", 0, ABI_INFO_SIZE - 1))
        self.assertTrue(expects_success("GET_STATS", 100, BIO_STATS_SIZE))
        self.assertFalse(expects_success("GET_STATS", 0, BIO_STATS_SIZE - 1))
        self.assertTrue(expects_success("READ", READ_REQUEST_SIZE, 1))
        self.assertFalse(expects_success("READ", READ_REQUEST_SIZE - 1, 4096))
        self.assertFalse(expects_success("READ", READ_REQUEST_SIZE, 0))
        self.assertTrue(expects_success("SET_RATE_TRIM", RATE_TRIM_SIZE, 0, 0))
        self.assertFalse(expects_success("SET_RATE_TRIM", RATE_TRIM_SIZE, 0, 1))
        self.assertFalse(expects_success("unknown", 1024, 1024))
        self.assertEqual(len({IOCTL_BIO_QUERY_INFO, IOCTL_BIO_READ,
                              IOCTL_BIO_SET_RATE_TRIM, IOCTL_BIO_GET_STATS}), 4)

    def bounded(self, kernel, errors):
        device = pending_device(kernel)
        with mock.patch.object(ioctl_fuzz.ctypes, "get_last_error",
                               side_effect=errors, create=True):
            return device, device.bounded_ioctl(IOCTL_BIO_READ, b"\xff" * 8, 4096)

    def test_a_request_pending_past_the_wait_is_cancelled(self):
        kernel = PendingKernel([WAIT_TIMEOUT, 0])
        device, result = self.bounded(kernel, [ERROR_IO_PENDING, ERROR_OPERATION_ABORTED])
        self.assertEqual(result, (False, ERROR_OPERATION_ABORTED, b"", 0, True))
        self.assertEqual(kernel.cancels, 1)
        self.assertEqual(kernel.closed, 1)
        self.assertEqual(device.stuck, [])

    def test_a_request_completed_within_the_wait_is_not_cancelled(self):
        kernel = PendingKernel([0], completed_ok=True)
        _, result = self.bounded(kernel, [ERROR_IO_PENDING])
        self.assertEqual(result[0], True)
        self.assertEqual(result[4], False)
        self.assertEqual(kernel.cancels, 0)

    def test_a_cancel_the_driver_never_completes_is_reported(self):
        kernel = PendingKernel([WAIT_TIMEOUT, WAIT_TIMEOUT])
        device, result = self.bounded(kernel, [ERROR_IO_PENDING])
        self.assertIsNone(result)
        self.assertEqual(kernel.cancels, 1)
        self.assertEqual(len(device.stuck), 1)
        self.assertEqual(kernel.closed, 0)

    def test_generator_is_seed_deterministic(self):
        first = deterministic_cases(0xD2D)
        second = deterministic_cases(0xD2D)
        different = deterministic_cases(0xD2E)
        self.assertEqual(case_fingerprint(first), case_fingerprint(second))
        self.assertNotEqual(case_fingerprint(first), case_fingerprint(different))
        self.assertEqual(first, second)

    def test_summary_is_json_and_marks_unexpected_success(self):
        summary = new_summary(7, 3.5)
        bad = {
            "kind": "known", "name": "QUERY_INFO", "code": IOCTL_BIO_QUERY_INFO,
            "input": b"", "output_size": 1, "expect_success": False,
        }
        record_result(summary, bad, True, 0, 1)
        record_result(summary, dict(bad, kind="unknown"), False, 1, 0)
        summary["checks"] = {"query_info": True}
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "summary.json"
            path.write_text(json.dumps(summary), encoding="utf-8")
            loaded = json.loads(path.read_text(encoding="utf-8"))
        self.assertEqual(loaded["schema_version"], 1)
        self.assertEqual(loaded["seed"], 7)
        self.assertEqual(loaded["operations"], 2)
        self.assertEqual(loaded["status_counts"], {"SUCCESS": 1, "WIN32_1": 1})
        self.assertEqual(len(loaded["unexpected_successes"]), 1)
        self.assertEqual(loaded["unexpected_successes"][0]["code"], "0x00226400")


if __name__ == "__main__":
    unittest.main()

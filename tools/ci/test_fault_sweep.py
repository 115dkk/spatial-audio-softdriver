"""tools/ci/fault-sweep.ps1: the sweep's bookkeeping against a modelled driver.

The PowerShell runs with the device restart and the registry replaced by a
model of the driver's fault switch and unload report, so the logic is tested
on any OS with pwsh (CI sets SSR_TEST_PWSH).
"""
import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]

# A driver that passes $env:SSR_CALLS fault points per load, leaks one pool
# block when call $env:SSR_LEAK_AT fails, and ignores its switch when
# $env:SSR_IGNORE is set. Each restart unloads the running instance (writing
# its report) and loads one configured with the current FaultInjectAt.
MODEL = r"""
$global:SsrAt = 0
$global:SsrRunning = $null
$global:SsrReport = $null
$global:SsrRestarts = 0
function New-Instance {
    $calls = [int]$env:SSR_CALLS
    $injected = 0
    if ($global:SsrAt -ge 1 -and $global:SsrAt -le $calls -and -not $env:SSR_IGNORE) { $injected = $global:SsrAt }
    $leak = if ($injected -ne 0 -and $injected -eq [int]$env:SSR_LEAK_AT) { 1 } else { 0 }
    @{ Calls = $calls; Injected = $injected; Pool = $leak }
}
function Set-SsrFaultInjectAt([int] $At) { $global:SsrAt = $At }
function Get-SsrUnloadReport { $global:SsrReport }
function Invoke-SsrRestartCycles {
    param([string] $InstanceId, [int] $Count, [int] $SettleSeconds)
    $global:SsrRestarts++
    if ($global:SsrRunning) {
        $unloads = if ($global:SsrReport) { $global:SsrReport.Unloads + 1 } else { 1 }
        $global:SsrReport = [pscustomobject]@{
            Unloads = $unloads; FaultCalls = $global:SsrRunning.Calls; FaultInjectedAt = $global:SsrRunning.Injected
            OutstandingPool = $global:SsrRunning.Pool; OutstandingTimers = 0; OutstandingDma = 0
        }
    }
    $global:SsrRunning = New-Instance
    [pscustomobject]@{ Cycle = 1; PnputilExit = 0; Status = 'OK'; Problem = 0 }
}
$global:SsrRunning = New-Instance
"""


class FaultSweepTests(unittest.TestCase):
    def run_ps(self, body, *, calls=5, leak_at=0, ignore=False):
        pwsh = os.environ.get("SSR_TEST_PWSH") or shutil.which("pwsh")
        if not pwsh:
            self.skipTest("set SSR_TEST_PWSH to run tools/ci/fault-sweep.ps1")
        script = (
            f". '{ROOT / 'tools/ci/fault-sweep.ps1'}'\n"
            f"{MODEL}\n"
            f"try {{\n{body}\n}} catch {{ Write-Output \"ERROR $($_.Exception.Message)\"; exit 1 }}\n"
        )
        env = dict(os.environ, SSR_CALLS=str(calls), SSR_LEAK_AT=str(leak_at))
        env.pop("SSR_IGNORE", None)
        if ignore:
            env["SSR_IGNORE"] = "1"
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "case.ps1"
            path.write_text(script, encoding="utf-8")
            return subprocess.run([pwsh, "-NoProfile", "-File", str(path)], capture_output=True,
                                  text=True, env=env, timeout=120)

    SWEEP = ("$steps = @(Invoke-SsrFaultSweep -InstanceId 'ROOT\\MEDIA\\0000' -SettleSeconds 0)\n"
             "Write-Output \"STEPS $(($steps | ForEach-Object FailedCall) -join ',') RESTARTS $global:SsrRestarts AT $global:SsrAt\"")

    def test_a_clean_driver_is_swept_once_per_fault_point_and_left_with_the_switch_off(self):
        result = self.run_ps(self.SWEEP, calls=5)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        line = next(line for line in result.stdout.splitlines() if line.startswith("STEPS "))
        # Two clean restarts, then one per call and one to unload the last.
        self.assertEqual(line, "STEPS 0,1,2,3,4,5 RESTARTS 8 AT 0")

    def test_a_leak_after_one_failed_call_names_that_call(self):
        result = self.run_ps(self.SWEEP, calls=5, leak_at=3)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("call 3: OutstandingPool 1 at unload", result.stdout)
        self.assertNotIn("call 2:", result.stdout)

    def test_a_driver_that_ignores_its_switch_fails_the_sweep(self):
        result = self.run_ps(self.SWEEP, calls=3, ignore=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("passed call 1 without failing it", result.stdout)

    def test_a_driver_without_fault_points_fails_before_sweeping(self):
        result = self.run_ps(self.SWEEP, calls=0)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("no fault switch", result.stdout)

    def test_more_fault_points_than_the_bound_fail_rather_than_go_untested(self):
        result = self.run_ps("Invoke-SsrFaultSweep -InstanceId x -Max 4 -SettleSeconds 0", calls=5)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("more than the sweep's 4", result.stdout)

    def test_report_checks(self):
        body = (
            "$r = [pscustomobject]@{ Unloads = 4; FaultCalls = 9; FaultInjectedAt = 0; OutstandingPool = 0; OutstandingTimers = -1; OutstandingDma = 0 }\n"
            "Write-Output \"A $(@(Test-SsrUnloadReport $r 3 0) -join '|')\"\n"
            "Write-Output \"B $(@(Test-SsrUnloadReport $r 4 0) -join '|')\"\n"
            "$r.OutstandingTimers = 0\n"
            "Write-Output \"C $(@(Test-SsrUnloadReport $r 3 12) -join '|')\"\n"
            "Write-Output \"D $(@(Test-SsrUnloadReport $r 3 5) -join '|')\"\n"
            "Write-Output \"E $(@(Test-SsrUnloadReport $null 3 0) -join '|')\"\n"
        )
        result = self.run_ps(body)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        lines = dict(line.split(" ", 1) for line in result.stdout.splitlines() if line[:2] in ("A ", "B ", "C ", "D ", "E "))
        self.assertEqual(lines["A"], "OutstandingTimers -1 at unload")
        self.assertIn("did not unload", lines["B"])
        # Call 12 lies beyond the 9 this instance made: nothing to fail.
        self.assertEqual(lines["C"], "")
        self.assertEqual(lines["D"], "passed call 5 without failing it (FaultInjectedAt 0)")
        self.assertEqual(lines["E"], "the driver wrote no unload report")


if __name__ == "__main__":
    unittest.main()

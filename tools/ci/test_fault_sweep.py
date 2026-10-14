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
# block when call $env:SSR_LEAK_AT fails, ignores its switch when
# $env:SSR_IGNORE is set, and survives (starts anyway) when the failed call
# is $env:SSR_SURVIVE_AT. As on Windows, an instance whose start fails unloads
# at once and writes its report; one that started unloads at the disable.
MODEL = r"""
$global:SsrAt = 0
$global:SsrRunning = $null
$global:SsrReport = $null
$global:SsrActions = [System.Collections.Generic.List[string]]::new()
function Write-Report($instance) {
    $unloads = if ($global:SsrReport) { $global:SsrReport.Unloads + 1 } else { 1 }
    $global:SsrReport = [pscustomobject]@{
        Unloads = $unloads; FaultCalls = $instance.Calls; FaultInjectedAt = $instance.Injected
        OutstandingPool = $instance.Pool; OutstandingTimers = 0; OutstandingDma = 0
    }
}
function Set-SsrFaultInjectAt([int] $At) { $global:SsrAt = $At }
function Get-SsrUnloadReport { $global:SsrReport }
function Disable-SsrDevice([string] $InstanceId, [int] $SettleSeconds) {
    $global:SsrActions.Add('disable')
    if ($global:SsrRunning) { Write-Report $global:SsrRunning; $global:SsrRunning = $null }
}
function Enable-SsrDevice([string] $InstanceId, [int] $SettleSeconds) {
    $global:SsrActions.Add("enable $global:SsrAt")
    $calls = [int]$env:SSR_CALLS
    $injected = 0
    if ($global:SsrAt -ge 1 -and $global:SsrAt -le $calls -and -not $env:SSR_IGNORE) { $injected = $global:SsrAt }
    $leak = if ($injected -ne 0 -and $injected -eq [int]$env:SSR_LEAK_AT) { 1 } else { 0 }
    $instance = @{ Calls = $calls; Injected = $injected; Pool = $leak }
    if ($injected -ne 0 -and $injected -ne [int]$env:SSR_SURVIVE_AT) {
        Write-Report $instance
        return 10
    }
    $global:SsrRunning = $instance
    0
}
# The exercise passes $env:SSR_RUNTIME_CALLS more fault points in the running
# instance; one of them may be the switched one, and the instance keeps
# running (a stream that failed to open does not unload the driver).
function Invoke-Exercise([int] $At) {
    $global:SsrActions.Add("exercise $At")
    if ($At -eq 0 -and $env:SSR_CLEAN_EXERCISE_FAILS) { throw 'the clean exercise failed' }
    if (-not $global:SsrRunning) { return }
    $from = $global:SsrRunning.Calls + 1
    $global:SsrRunning.Calls += [int]$env:SSR_RUNTIME_CALLS
    if ($global:SsrAt -ge $from -and $global:SsrAt -le $global:SsrRunning.Calls -and -not $env:SSR_IGNORE) {
        $global:SsrRunning.Injected = $global:SsrAt
        if ($global:SsrAt -eq [int]$env:SSR_LEAK_AT) { $global:SsrRunning.Pool = 1 }
    }
}
# The install left a clean instance running.
$global:SsrRunning = @{ Calls = [int]$env:SSR_CALLS; Injected = 0; Pool = 0 }
"""


class FaultSweepTests(unittest.TestCase):
    def run_ps(self, body, *, calls=5, leak_at=0, ignore=False, survive_at=0, runtime=0, clean_exercise_fails=False):
        pwsh = os.environ.get("SSR_TEST_PWSH") or shutil.which("pwsh")
        if not pwsh:
            self.skipTest("set SSR_TEST_PWSH to run tools/ci/fault-sweep.ps1")
        script = (
            f". '{ROOT / 'tools/ci/fault-sweep.ps1'}'\n"
            f"{MODEL}\n"
            f"try {{\n{body}\n}} catch {{ Write-Output \"ERROR $($_.Exception.Message)\"; exit 1 }}\n"
        )
        env = dict(os.environ, SSR_CALLS=str(calls), SSR_LEAK_AT=str(leak_at), SSR_SURVIVE_AT=str(survive_at),
                   SSR_RUNTIME_CALLS=str(runtime))
        env.pop("SSR_IGNORE", None)
        env.pop("SSR_CLEAN_EXERCISE_FAILS", None)
        if clean_exercise_fails:
            env["SSR_CLEAN_EXERCISE_FAILS"] = "1"
        if ignore:
            env["SSR_IGNORE"] = "1"
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "case.ps1"
            path.write_text(script, encoding="utf-8")
            return subprocess.run([pwsh, "-NoProfile", "-File", str(path)], capture_output=True,
                                  text=True, env=env, timeout=120)

    SWEEP = ("$steps = @(Invoke-SsrFaultSweep -InstanceId 'ROOT\\MEDIA\\0000' -SettleSeconds 0)\n"
             "Write-Output \"STEPS $(($steps | ForEach-Object FailedCall) -join ',') AT $global:SsrAt\"\n"
             "Write-Output \"ACTIONS $($global:SsrActions -join ',')\"")

    def lines(self, result):
        return {line.split(" ", 1)[0]: line.split(" ", 1)[1] for line in result.stdout.splitlines()
                if line.startswith(("STEPS ", "ACTIONS "))}

    def test_a_clean_driver_is_swept_once_per_fault_point_and_left_running_clean(self):
        result = self.run_ps(self.SWEEP, calls=3)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        lines = self.lines(result)
        self.assertEqual(lines["STEPS"], "1,2,3 AT 0")
        # The install's instance goes, one clean life is measured, each call
        # is failed in its own instance, and a clean start ends it.
        self.assertEqual(lines["ACTIONS"],
                         "disable,enable 0,disable,enable 1,disable,enable 2,disable,enable 3,disable,enable 0")

    def test_a_start_that_survives_its_failure_is_judged_at_the_disable(self):
        result = self.run_ps(self.SWEEP, calls=3, survive_at=2)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        result = self.run_ps(self.SWEEP, calls=3, survive_at=2, leak_at=2)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("call 2: OutstandingPool 1 at unload", result.stdout)

    RUNTIME = ("$steps = @(Invoke-SsrFaultSweep -InstanceId x -SettleSeconds 0 -Exercise { param($At) Invoke-Exercise $At })\n"
               "Write-Output \"STEPS $(($steps | ForEach-Object FailedCall) -join ',') AT $global:SsrAt\"\n"
               "Write-Output \"ACTIONS $($global:SsrActions -join ',')\"")

    def test_the_exercise_sweeps_only_the_points_it_adds_to_a_load(self):
        result = self.run_ps(self.RUNTIME, calls=2, runtime=2)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        lines = self.lines(result)
        self.assertEqual(lines["STEPS"], "3,4 AT 0")
        self.assertTrue(lines["ACTIONS"].startswith("disable,enable 0,disable,enable 0,exercise 0,disable,"),
                        lines["ACTIONS"])
        self.assertTrue(lines["ACTIONS"].endswith("enable 3,exercise 3,disable,enable 4,exercise 4,disable,enable 0"),
                        lines["ACTIONS"])

    def test_a_clean_exercise_that_fails_stops_the_sweep(self):
        result = self.run_ps(self.RUNTIME, calls=2, runtime=2, clean_exercise_fails=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("the clean exercise failed", result.stdout)
        self.assertNotIn("enable 3", result.stdout)

    def test_a_runtime_leak_names_its_call(self):
        result = self.run_ps(self.RUNTIME, calls=2, runtime=3, leak_at=4)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("call 4: OutstandingPool 1 at unload", result.stdout)

    def test_an_exercise_that_reaches_no_fault_point_fails(self):
        result = self.run_ps(self.RUNTIME, calls=2, runtime=0)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("passed no fault point beyond the load's 2", result.stdout)

    def test_a_leak_after_one_failed_call_names_that_call(self):
        result = self.run_ps(self.SWEEP, calls=5, leak_at=3)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("call 3: OutstandingPool 1 at unload", result.stdout)
        self.assertNotIn("call 2:", result.stdout)
        self.assertNotIn("call 4:", result.stdout)

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

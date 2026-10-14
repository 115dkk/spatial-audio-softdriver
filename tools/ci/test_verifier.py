"""tools/ci/verifier.ps1: option choice per Windows version, refusals, and /query parsing.

The PowerShell runs with verifier.exe and cmd.exe replaced by functions, so the
decision logic is tested on any OS with pwsh (CI sets SSR_TEST_PWSH).
"""
import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]

# The shape Server 2022 prints (from a CI run), trimmed.
QUERY = """\
Time Stamp: 10/14/2026 05:32:35.071

Verifier Flags: 0x000009bf

  Standard Flags:

    [X] 0x00000001 Special pool.
    [X] 0x00000008 Pool tracking.

  Verifier Statistics Summary

    Pool Allocations Attempted:                      812
    Pool Allocations Failed Deliberately:            42

  Driver Verification List

    MODULE: other.sys (load: 1 / unload: 0)

      Pool Allocation Statistics: ( NonPaged / Paged )

        Current Pool Allocations:  (       99 /        5 )

    MODULE: spatialsoftreceiver.sys (load: 3 / unload: 2)

      Pool Allocation Statistics: ( NonPaged / Paged )

        Current Pool Allocations:  (        7 /        0 )
        Current Pool Bytes:        (     4096 /        0 )
"""

# The older shape.
LEGACY_QUERY = """\
08/14/2026, 04:00:00
Level: 000009BB
RaiseIrqls: 0
AllocationsFailedDeliberately: 42

Verified drivers:

Name: other.sys, loads: 1, unloads: 0
CurrentPagedPoolAllocations: 5
CurrentNonPagedPoolAllocations: 99

Name: spatialsoftreceiver.sys, loads: 3, unloads: 2
CurrentPagedPoolAllocations: 0
CurrentNonPagedPoolAllocations: 7
"""

# What `/dif ... /now` printed on Server 2025 before the driver had loaded.
DIF_NOT_RUNNING = """\
"RuleClasses": 0x0016091b 0x00000000
"Drivers":
"FailedDrivers": spatialsoftreceiver.sys 0xC0000C7B
"RemovedDrivers":
"FailedRemovedDrivers":
"""

MOCKS = r"""
$global:SsrCalls = [System.Collections.Generic.List[string]]::new()
function verifier.exe {
    $global:SsrCalls.Add("verifier " + ($args -join ' '))
    if ($args[0] -eq '/query') { $global:LASTEXITCODE = 0; return $env:SSR_QUERY }
    $global:LASTEXITCODE = [int]$env:SSR_VERIFIER_EXIT
    if ($args[0] -eq '/dif') { return $env:SSR_DIF_OUTPUT }
    'ok'
}
function cmd.exe {
    $global:SsrCalls.Add("cmd " + ($args -join ' '))
    $global:LASTEXITCODE = 0
    'ok'
}
"""

class VerifierScriptTests(unittest.TestCase):
    def run_ps(self, body, *, dif, exit_code=0, query=QUERY, dif_output='"Drivers": spatialsoftreceiver.sys'):
        pwsh = os.environ.get("SSR_TEST_PWSH") or shutil.which("pwsh")
        if not pwsh:
            self.skipTest("set SSR_TEST_PWSH to run tools/ci/verifier.ps1")
        script = (
            f". '{ROOT / 'tools/ci/verifier.ps1'}'\n"
            f"{MOCKS}\n"
            f"function Test-SsrDifVerifier {{ ${'true' if dif else 'false'} }}\n"
            f"{body}\n"
        )
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "case.ps1"
            path.write_text(script, encoding="utf-8")
            env = dict(os.environ, SSR_QUERY=query, SSR_VERIFIER_EXIT=str(exit_code), SSR_DIF_OUTPUT=dif_output)
            return subprocess.run([pwsh, "-NoProfile", "-File", str(path)], capture_output=True,
                                  text=True, env=env, timeout=120)

    def calls(self, result):
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        return [line[len("CALL "):] for line in result.stdout.splitlines() if line.startswith("CALL ")]

    STATE = ("$s = Get-SsrVerifierState\n"
             "Write-Output \"STATE $($s.Flags) $($s.FaultsInjected) $($s.Listed) $($s.Loads) "
             "$($s.Unloads) $($s.CurrentNonPagedPoolAllocations) $($s.CurrentPagedPoolAllocations)\"")

    def state_line(self, result):
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        return next(line for line in result.stdout.splitlines() if line.startswith("STATE "))

    def test_query_is_parsed_for_our_driver_only(self):
        result = self.run_ps(self.STATE, dif=False)
        self.assertEqual(self.state_line(result), f"STATE {0x9bf} 42 True 3 2 7 0")

    def test_the_older_query_shape_is_parsed_too(self):
        result = self.run_ps(self.STATE, dif=False, query=LEGACY_QUERY)
        self.assertEqual(self.state_line(result), f"STATE {0x9bb} 42 True 3 2 7 0")

    def test_an_unlisted_driver_reads_as_not_listed(self):
        result = self.run_ps(self.STATE, dif=False, query="No drivers are currently verified.")
        self.assertEqual(self.state_line(result), "STATE   False    ")

    def test_server_2022_sets_faults_first_then_all_flags_before_the_load(self):
        result = self.run_ps(
            "Enable-SsrVerifier -Mode Faults -When BeforeLoad -Probability 1000 | Out-Null\n"
            "$global:SsrCalls | ForEach-Object { Write-Output \"CALL $_\" }", dif=False)
        calls = self.calls(result)
        self.assertTrue(calls[0].startswith("cmd /d /c verifier.exe /volatile /faults 1000"), calls)
        self.assertIn('"" "" 0', calls[0])
        self.assertEqual(calls[1], "verifier /volatile /flags 0x9BF /adddriver spatialsoftreceiver.sys")
        self.assertEqual(calls[-1], "verifier /query")

    def test_server_2022_refuses_flags_that_did_not_stick(self):
        query = QUERY.replace("0x000009bf", "0x00000004")
        result = self.run_ps("Enable-SsrVerifier -Mode Faults -When BeforeLoad", dif=False, query=query)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("without 0x8", result.stdout + result.stderr)
        query = QUERY.replace("0x000009bf", "0x000009bb")
        result = self.run_ps("Enable-SsrVerifier -Mode Faults -When BeforeLoad", dif=False, query=query)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("without 0x4", result.stdout + result.stderr)

    def test_server_2025_starts_rule_classes_on_the_running_driver(self):
        result = self.run_ps(
            "Enable-SsrVerifier -Mode Faults -When Loaded | Out-Null\n"
            "$global:SsrCalls | ForEach-Object { Write-Output \"CALL $_\" }", dif=True)
        calls = self.calls(result)
        self.assertEqual(calls[0], "verifier /dif 1 2 4 5 9 12 18 19 /now /driver spatialsoftreceiver.sys")
        self.assertEqual(calls[1], "verifier /faultssystematic enableruntime")
        self.assertEqual(calls[-1], "verifier /query")

    def test_server_2025_fails_when_the_driver_is_not_running(self):
        result = self.run_ps("Enable-SsrVerifier -Mode Standard -When Loaded", dif=True, dif_output=DIF_NOT_RUNNING)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("0xC0000C7B: the driver is not running", result.stdout + result.stderr)

    def test_each_stage_does_nothing_on_the_other_windows(self):
        for dif, when in ((True, "BeforeLoad"), (False, "Loaded")):
            result = self.run_ps(
                f"Enable-SsrVerifier -Mode Faults -When {when} | Out-Null\n"
                "$global:SsrCalls | ForEach-Object { Write-Output \"CALL $_\" }", dif=dif)
            self.assertEqual(self.calls(result), [])

    def test_standard_mode_injects_nothing(self):
        for dif, when, first in (
                (False, "BeforeLoad", "verifier /volatile /flags 0x9BB /adddriver spatialsoftreceiver.sys"),
                (True, "Loaded", "verifier /dif 1 2 4 5 9 12 18 /now /driver spatialsoftreceiver.sys")):
            result = self.run_ps(
                f"Enable-SsrVerifier -Mode Standard -When {when} | Out-Null\n"
                "$global:SsrCalls | ForEach-Object { Write-Output \"CALL $_\" }", dif=dif)
            self.assertEqual(self.calls(result), [first, "verifier /query"])

    def test_a_driver_missing_from_the_query_fails_the_step(self):
        result = self.run_ps("Enable-SsrVerifier -Mode Standard -When BeforeLoad", dif=False,
                             query="No drivers are currently verified.")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("does not list spatialsoftreceiver.sys", result.stdout + result.stderr)

    def test_a_setting_that_needs_a_reboot_fails_the_step(self):
        for code, words in ((2, "needs a reboot"), (1, "failed (exit 1)")):
            result = self.run_ps("Enable-SsrVerifier -Mode Standard -When BeforeLoad", dif=False, exit_code=code)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn(words, result.stdout + result.stderr)


class WorkflowContractTests(unittest.TestCase):
    def test_driver_workflow_uses_the_helper_and_injects_faults_on_both_images(self):
        workflow = (ROOT / ".github/workflows/driver.yml").read_text(encoding="utf-8")
        self.assertNotIn("/flags 0x209BB", workflow)
        package = workflow.split("\n  package:\n", 1)[1].split("\n  faults:\n", 1)[0]
        faults = workflow.split("\n  faults:\n", 1)[1].split("\n  codeql:\n", 1)[0]
        for job, mode in ((package, "Standard"), (faults, "Faults")):
            install = job.index("install-driver-test.ps1")
            # Volatile flags before the first load, /dif once the driver runs.
            self.assertLess(job.index(f"Enable-SsrVerifier -Mode {mode} -When BeforeLoad"), install)
            self.assertGreater(job.index(f"Enable-SsrVerifier -Mode {mode} -When Loaded"), install)
        self.assertIn("os: [windows-2022, windows-2025]", faults)
        self.assertIn("Invoke-SsrRestartCycles", faults)
        self.assertIn("uninstall.ps1", faults)
        self.assertIn("Invoke-SsrRestartCycles -InstanceId $device.InstanceId -Count 5", package)


if __name__ == "__main__":
    unittest.main()

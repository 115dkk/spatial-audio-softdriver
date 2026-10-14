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

QUERY = """\
08/14/2026, 04:00:00
Level: 000009BB
RaiseIrqls: 0
AcquireSpinLocks: 1234
AllocationsFailedDeliberately: 42

Verified drivers:

Name: other.sys, loads: 1, unloads: 0
CurrentPagedPoolAllocations: 5
CurrentNonPagedPoolAllocations: 99

Name: spatialsoftreceiver.sys, loads: 3, unloads: 2
CurrentPagedPoolAllocations: 0
CurrentNonPagedPoolAllocations: 7
PagedPoolUsageInBytes: 0
NonPagedPoolUsageInBytes: 4096
"""

MOCKS = r"""
$global:SsrCalls = [System.Collections.Generic.List[string]]::new()
function verifier.exe {
    $global:SsrCalls.Add("verifier " + ($args -join ' '))
    if ($args[0] -eq '/query') { $global:LASTEXITCODE = 0; return $env:SSR_QUERY }
    $global:LASTEXITCODE = [int]$env:SSR_VERIFIER_EXIT
    'ok'
}
function cmd.exe {
    $global:SsrCalls.Add("cmd " + ($args -join ' '))
    $global:LASTEXITCODE = 0
    'ok'
}
"""


class VerifierScriptTests(unittest.TestCase):
    def run_ps(self, body, *, dif, exit_code=0):
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
            env = dict(os.environ, SSR_QUERY=QUERY, SSR_VERIFIER_EXIT=str(exit_code))
            return subprocess.run([pwsh, "-NoProfile", "-File", str(path)], capture_output=True,
                                  text=True, env=env, timeout=120)

    def calls(self, result):
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        return [line[len("CALL "):] for line in result.stdout.splitlines() if line.startswith("CALL ")]

    def test_query_is_parsed_for_our_driver_only(self):
        result = self.run_ps(
            "$s = Get-SsrVerifierState\n"
            "Write-Output \"STATE $($s.Level) $($s.FaultsInjected) $($s.Listed) $($s.Loads) "
            "$($s.Unloads) $($s.CurrentNonPagedPoolAllocations) $($s.CurrentPagedPoolAllocations)\"",
            dif=False)
        self.assertEqual(result.returncode, 0, result.stderr)
        line = next(line for line in result.stdout.splitlines() if line.startswith("STATE "))
        self.assertEqual(line, "STATE 000009BB 42 True 3 2 7 0")

    def test_server_2022_uses_volatile_flags_and_randomized_faults(self):
        result = self.run_ps(
            "Enable-SsrVerifier -Mode Faults -Probability 1000\n"
            "$global:SsrCalls | ForEach-Object { Write-Output \"CALL $_\" }", dif=False)
        calls = self.calls(result)
        self.assertEqual(calls[0], "verifier /volatile /flags 0x9BB /adddriver spatialsoftreceiver.sys")
        self.assertTrue(calls[1].startswith("cmd /d /c verifier.exe /volatile /faults 1000"), calls)
        self.assertIn('"" "" 0', calls[1])
        self.assertEqual(calls[-1], "verifier /query")

    def test_server_2025_uses_rule_classes_and_systematic_faults(self):
        result = self.run_ps(
            "Enable-SsrVerifier -Mode Faults\n"
            "$global:SsrCalls | ForEach-Object { Write-Output \"CALL $_\" }", dif=True)
        calls = self.calls(result)
        self.assertEqual(calls[0], "verifier /dif 1 2 4 5 9 12 18 19 /now /driver spatialsoftreceiver.sys")
        self.assertEqual(calls[1], "verifier /faultssystematic enableruntime")

    def test_standard_mode_injects_nothing(self):
        for dif, first in ((False, "verifier /volatile /flags 0x9BB /adddriver spatialsoftreceiver.sys"),
                           (True, "verifier /dif 1 2 4 5 9 12 18 /now /driver spatialsoftreceiver.sys")):
            result = self.run_ps(
                "Enable-SsrVerifier -Mode Standard\n"
                "$global:SsrCalls | ForEach-Object { Write-Output \"CALL $_\" }", dif=dif)
            calls = self.calls(result)
            self.assertEqual(calls, [first, "verifier /query"])

    def test_a_setting_that_needs_a_reboot_fails_the_step(self):
        for code, words in ((2, "needs a reboot"), (1, "failed (exit 1)")):
            result = self.run_ps("Enable-SsrVerifier -Mode Standard", dif=False, exit_code=code)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn(words, result.stdout + result.stderr)


class WorkflowContractTests(unittest.TestCase):
    def test_driver_workflow_uses_the_helper_and_injects_faults_on_both_images(self):
        workflow = (ROOT / ".github/workflows/driver.yml").read_text(encoding="utf-8")
        self.assertNotIn("/flags 0x209BB", workflow)
        self.assertIn("Enable-SsrVerifier -Mode Standard", workflow)
        faults = workflow.split("\n  faults:\n", 1)[1].split("\n  codeql:\n", 1)[0]
        self.assertIn("os: [windows-2022, windows-2025]", faults)
        self.assertIn("Enable-SsrVerifier -Mode Faults", faults)
        self.assertIn("Invoke-SsrRestartCycles", faults)
        self.assertIn("uninstall.ps1", faults)
        package = workflow.split("\n  package:\n", 1)[1].split("\n  faults:\n", 1)[0]
        self.assertIn("Invoke-SsrRestartCycles -InstanceId $device.InstanceId -Count 5", package)


if __name__ == "__main__":
    unittest.main()

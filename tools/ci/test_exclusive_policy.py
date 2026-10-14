"""Static package contracts; Windows exclusive_policy_check.ps1 tests enforcement."""
import json
import os
import re
import shutil
import struct
import subprocess
import tempfile
import unittest
from pathlib import Path

from driver_flow_check import DEFAULT_ENDPOINT_MASK, parse_wave_format

ROOT = Path(__file__).resolve().parents[2]


def read(path):
    return (ROOT / path).read_text(encoding="utf-16" if path.endswith(".inx") else "utf-8-sig")


def section(text, name):
    return text.split(f"[{name}]", 1)[1].split("\n[", 1)[0]


class ExclusivePolicyContractTests(unittest.TestCase):
    def test_three_minipairs_and_portcls_capacity(self):
        pairs = read("driver/spatialsoftreceiver/minipairs.h")
        for ep, constant in enumerate(("PRIMARY", "SECOND", "PASSTHROUGH")):
            self.assertIn(f'{{L"WaveHdmi{ep}", L"TopologyHdmi{ep}", SSR_ENDPOINT_{constant}}}', pairs)
        adapter = read("driver/spatialsoftreceiver/adapter.cpp")
        self.assertIn("2 * ARRAYSIZE(SsrMiniPairs)", adapter)
        self.assertIn("Settings = {SSR_DEFAULT_ENDPOINT_MASK,", adapter)
        self.assertIn("Settings.EndpointMask &= available", adapter)
        load = adapter.split("static void LoadSettings()", 1)[1].split('extern "C"', 1)[0]
        self.assertNotIn("return;", load)
        self.assertIn("Settings().EndpointMask & (1UL << i)", adapter)
        self.assertEqual(DEFAULT_ENDPOINT_MASK, 7)

    def test_inf_interfaces_masks_names_and_serialized_oem_formats(self):
        inf = read("driver/spatialsoftreceiver/spatialsoftreceiver.inx")
        interfaces = section(inf, "SpatialSoftReceiver_Device.NT.Interfaces")
        parameters = section(inf, "SpatialSoftReceiver_Service_Parameters")
        self.assertIn("EndpointMask,0x00010001,7", parameters)
        for ep in range(3):
            self.assertEqual(interfaces.count(f"%KSNAME_WaveHdmi{ep}%"), 3)
            self.assertEqual(interfaces.count(f"%KSNAME_TopologyHdmi{ep}%"), 2)
            self.assertIn(f'"Spatial SoftReceiver{"" if ep == 0 else " Music" if ep == 1 else " PASS"}"', inf)
            topo = section(inf, f"SpatialSoftReceiver.I.TopologyHdmi{ep}.AddReg")
            line = next(line for line in topo.splitlines() if line.startswith("HKR,EP\\0,%PKEY_AudioEngine_OEMFormat%"))
            blob = bytes(int(part, 16) for part in line.split(",")[4:])
            self.assertEqual(struct.unpack_from("<II", blob), (65, 40))
            self.assertEqual(len(blob), 48)
            fmt = parse_wave_format(blob[8:])
            self.assertEqual((fmt["channels"], fmt["rate"], fmt["sample"]), (8 if ep == 0 else 2, 48000, "float"))
            mask = next(line.split(",")[4:] for line in parameters.splitlines() if f",CodecMask{ep}," in line)
            self.assertEqual(bytes.fromhex("".join(mask)), bytes.fromhex("6c00000000000000"))
            for name, value in (("PcmRateMask", "0x3f"), ("PcmShapeMask", "0x1f"),
                                ("PcmMaxMaskedChannels", "18"), ("PcmDirectChannels", "0xffffffff")):
                self.assertIn(f"HKR,Parameters,{name}{ep},0x00010001,{value}", parameters)
        # Do not present the undocumented policy as a supported INF property.
        self.assertNotIn("b3f8fa53", inf.lower())

    def test_policy_uses_property_store_commit_and_real_exclusive_initialize(self):
        helper = read("tools/ci/exclusive-policy.ps1")
        self.assertIn('new Guid("b3f8fa53-0004-438e-9003-51a46e139bfc")', helper)
        self.assertIn("device.OpenPropertyStore(2, out store)", helper)
        self.assertIn("Check(store.Commit())", helper)
        self.assertIn("client.IsFormatSupported(1, format, IntPtr.Zero)", helper)
        self.assertIn("client.Initialize(1, 0, period, period, format, IntPtr.Zero)", helper)
        self.assertIn("Marshal.FreeCoTaskMem(format)", helper)
        self.assertIn("$control.Write($null, $null)", helper)
        self.assertIn("Get-SpatialEndpoints -InstanceId $InstanceId", helper)
        self.assertNotIn("IndexOf($names", helper)
        check = read("tools/ci/exclusive_policy_check.ps1")
        self.assertIn("0x8889000E", check)
        self.assertIn("$item.probe.support_hr -eq '0x8889000E'", check)
        self.assertIn("$item.probe.initialize_hr -eq '0x8889000E'", check)
        self.assertIn("$item.probe.initialize_hr -eq '0x00000000'", check)
        # A policy refusal ends the probe: it is about the endpoint, not a format.
        self.assertIn("if (unchecked((uint)supported) == 0x8889000E) return result;", helper)
        self.assertIn("exit $(if ($report.passed) { 0 } else { 1 })", check)

    def test_kit_applies_checks_collects_and_clears_policy(self):
        install = read("tools/testkit/install.ps1")
        self.assertLess(install.index("$identitiesMatch ="), install.index("Set-SsrExclusivePolicy -"))
        self.assertIn("-band 0x7", install)
        self.assertIn("exclusive_policy_check.ps1", install)
        uninstall = read("tools/testkit/uninstall.ps1")
        self.assertLess(uninstall.index("Clear-SsrExclusivePolicy -"), uninstall.index("pnputil.exe /remove-device"))
        self.assertIn("Get-SsrPolicySnapshot", read("tools/testkit/collect.ps1"))
        kit = read("tools/ci/driver-test-kit.ps1")
        for name in ("exclusive-policy.ps1", "exclusive_policy_check.ps1", "loopback_check.py"):
            self.assertIn(f"'{name}'", kit)
        states = read("tools/ci/endpoint-states.ps1")
        self.assertIn("$Expect = 3", states)
        self.assertIn("Get-SpatialEndpoints -InstanceId $InstanceId", states)

    def test_checker_accepts_only_matching_policy_and_initialize_results(self):
        pwsh = os.environ.get("SSR_TEST_PWSH") or shutil.which("pwsh")
        if not pwsh:
            self.skipTest("set SSR_TEST_PWSH to exercise the PowerShell checker without Windows COM")
        # Execute the actual report/exit logic with an injected transport. This
        # tests decision paths, not the Windows implementation of private keys.
        mock = '''
class SsrExclusivePolicy {
    [int] $endpoint
    SsrExclusivePolicy([string] $id) { $this.endpoint = [int]$id }
    [object] Read() {
        $v = [int]($this.endpoint -eq 2)
        if ($env:SSR_POLICY_CASE -eq 'wrong_policy') { $v = 1 - $v }
        if ($env:SSR_POLICY_CASE -eq 'missing_property') { return @{allow_exclusive=$null; exclusive_priority=$null} }
        return @{allow_exclusive=$v; exclusive_priority=$v}
    }
    [object] Probe() {
        if ($env:SSR_POLICY_CASE -eq 'exception') { throw 'activation failed' }
        if ($this.endpoint -ne 2 -and $env:SSR_POLICY_CASE -eq 'refused_at_format') {
            # What the runners showed: IsFormatSupported already refuses.
            return @{ supported=$false; support_hr='0x8889000E'; initialize_hr=$null }
        }
        if ($env:SSR_POLICY_CASE -eq 'unsupported') {
            return @{ supported=$false; support_hr='0x88890008'; initialize_hr=$null }
        }
        $hr = if ($this.endpoint -eq 2) { '0x00000000' } else { '0x8889000E' }
        if ($env:SSR_POLICY_CASE -eq 'wrong_hr') { $hr = '0x8889000A' }
        return @{ supported=$true; support_hr='0x00000000'; initialize_hr=$hr }
    }
    [void] Dispose() { }
}
function Get-SsrPolicyTargets {
    param([string] $InstanceId)
    $ids = if ($env:SSR_POLICY_CASE -eq 'missing') { @(0,1) } elseif ($env:SSR_POLICY_CASE -eq 'duplicate') { @(0,1,2,2) } else { @(0,1,2) }
    foreach ($ep in $ids) { [pscustomobject]@{endpoint=$ep; name="endpoint $ep"; id=[string]$ep; state=1} }
}
'''
        with tempfile.TemporaryDirectory() as folder:
            folder = Path(folder)
            checker = folder / "exclusive_policy_check.ps1"
            checker.write_text(read("tools/ci/exclusive_policy_check.ps1"))
            (folder / "exclusive-policy.ps1").write_text(mock)
            summary = folder / "summary.json"
            passing = ("ok", "refused_at_format")
            for case in ("ok", "refused_at_format", "wrong_policy", "missing_property", "wrong_hr", "unsupported",
                         "exception", "missing", "duplicate"):
                with self.subTest(case=case):
                    run = subprocess.run([pwsh, "-NoProfile", "-File", str(checker), "-Summary", str(summary)],
                                         env={**os.environ, "SSR_POLICY_CASE": case}, capture_output=True, text=True, timeout=30)
                    self.assertEqual(run.returncode, 0 if case in passing else 1, run.stdout + run.stderr)
                    result = json.loads(summary.read_text(encoding="utf-8-sig"))
                    self.assertEqual(result["passed"], case in passing)
                    self.assertEqual(bool(result["problems"]), case not in passing)

    def test_korean_powershell_keeps_utf8_bom(self):
        for folder in ("tools/ci", "tools/testkit"):
            for path in (ROOT / folder).glob("*.ps1"):
                data = path.read_bytes()
                if re.search("[가-힣]", data.decode("utf-8-sig")):
                    self.assertTrue(data.startswith(b"\xef\xbb\xbf"), path.name)


if __name__ == "__main__":
    unittest.main()

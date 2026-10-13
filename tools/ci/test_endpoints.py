"""Identity, localization, and signed-package contracts, without Windows audio."""
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
from types import SimpleNamespace
import unittest

from endpoints import CONTROL_DEVICE_PATH, find_speaker, parse_endpoints

ROOT = Path(__file__).resolve().parents[2]


def read(path):
    return (ROOT / path).read_text(encoding="utf-16" if path.endswith(".inx") else "utf-8-sig")


def sample():
    # Deliberately reordered, localized, duplicated display labels, custom name.
    return [
        {"endpoint": 2, "id": "device-c", "name": "음악", "state": 1},
        {"endpoint": 0, "id": "device-a", "name": "내 게임 출력", "state": 1},
        {"endpoint": 1, "id": "device-b", "name": "음악", "state": 1},
    ]


class EndpointTests(unittest.TestCase):
    def test_json_and_id_selection_ignore_names_and_order(self):
        rows = parse_endpoints("\ufeff" + json.dumps(sample(), ensure_ascii=False))
        speakers = {row["id"]: SimpleNamespace(id=row["id"], name="renamed again") for row in rows}
        sc = SimpleNamespace(get_speaker=lambda id: speakers[id])
        for row in rows:
            self.assertIs(find_speaker(sc, row["endpoint"], rows), speakers[row["id"]])
        self.assertEqual(parse_endpoints("[]"), [])
        self.assertIsNone(find_speaker(sc, 0, []))
        rows[1]["state"] = 2
        self.assertIsNone(find_speaker(sc, 0, rows))

    def test_bad_json_and_duplicate_identity_fail(self):
        for value in ({}, None, "name", [0], [{"endpoint": 0}], sample() + [sample()[0]]):
            with self.subTest(value=value), self.assertRaises(ValueError):
                parse_endpoints(json.dumps(value))
        for field, value in (("endpoint", 3), ("endpoint", True), ("state", 0), ("state", True),
                             ("id", ""), ("id", None), ("name", None)):
            rows = sample()
            rows[0][field] = value
            with self.subTest(field=field, value=value), self.assertRaises(ValueError):
                parse_endpoints(json.dumps(rows))
        rows = sample()
        rows.append({"endpoint": 0, "id": "other-a", "name": "unrelated", "state": 1})
        with self.assertRaises(ValueError):
            parse_endpoints(json.dumps(rows))
        rows[-1]["state"] = 4  # stale inactive instance is not a second active endpoint
        self.assertEqual(len(parse_endpoints(json.dumps(rows))), 4)

    def test_soundcard_fuzzy_fallback_is_rejected(self):
        sc = SimpleNamespace(get_speaker=lambda id: SimpleNamespace(id="wrong", name=id))
        with self.assertRaises(ValueError):
            find_speaker(sc, 0, sample())

    def test_inf_english_defaults_equal_core_table_and_korean_labels(self):
        path = ROOT / "driver/bioatmos/spatialsoftreceiver.inx"
        self.assertTrue(path.read_bytes().startswith(b"\xff\xfe"))
        inf = read("driver/bioatmos/spatialsoftreceiver.inx")
        english = inf.split("[Strings]\n")[1].split("[Strings.0412]")[0]
        korean = inf.split("[Strings.0412]\n")[1]
        # Windows selects one complete section; no per-key English fallback.
        tokens = lambda text: dict(re.findall(r'^([^;\s][^=]*?)\s*=\s*"([^"]*)"', text, re.M))
        en_tokens, ko_tokens = tokens(english), tokens(korean)
        self.assertEqual(set(en_tokens), set(ko_tokens))
        for key in en_tokens:
            if key not in {"DiskName", "SpatialSoftReceiver.DeviceDesc", "SpatialSoftReceiver.ServiceDesc",
                           "Endpoint0Name", "Endpoint1Name", "Endpoint2Name"}:
                self.assertEqual(en_tokens[key], ko_tokens[key], key)
        defaults = re.findall(r'u"([^"]+)"', read("driver/core/bio_names.h"))
        for ep in range(3):
            self.assertEqual(re.search(rf'Endpoint{ep}Name\s*=\s*"([^"]+)"', english)[1], defaults[ep])
            self.assertIn(f"HKR,Parameters,EndpointName{ep},0x00000000,%Endpoint{ep}Name%", inf)
            self.assertEqual(inf.count(f"HKR,,FriendlyName,,%Endpoint{ep}Name%"), 2)
        expected = ("공간 소프트리시버", "공간 소프트리시버 음악", "공간 소프트리시버 PASS")
        for ep, name in enumerate(expected):
            self.assertRegex(korean, rf'Endpoint{ep}Name\s*=\s*"{name}"')
        for key in ("DiskName", "SpatialSoftReceiver.DeviceDesc", "SpatialSoftReceiver.ServiceDesc"):
            self.assertRegex(korean, rf'{re.escape(key)}\s*=\s*"공간 소프트리시버')
        self.assertIn('ProviderName          = "115dkk"', english)
        self.assertIn('MfgName               = "115dkk"', english)
        self.assertIn("AddService = SpatialSoftReceiver,", inf)
        self.assertIn("Root\\SpatialSoftReceiver", inf)
        for filename in ("spatialsoftreceiver.cat", "spatialsoftreceiver.sys"):
            self.assertIn(filename, inf)
        for lang in (en_tokens, ko_tokens):
            # Numeric DIRIDs such as %13% are Windows-provided, not Strings keys.
            for token in re.findall(r'%([^%\n]+)%', inf.split('[Strings]')[0]):
                if not token.isdecimal():
                    self.assertIn(token, lang)
        project = read("driver/bioatmos/bioatmos.vcxproj")
        self.assertIn("<TargetName>spatialsoftreceiver</TargetName>", project)
        self.assertIn('<Inf Include="spatialsoftreceiver.inx"', project)
        self.assertIn("$(OutDir)spatialsoftreceiver.inf", project)
        builder = read("tools/build-driver.ps1")
        for extension in ("sys", "inf", "pdb"):
            self.assertIn(f"'spatialsoftreceiver.{extension}'", builder)

    def test_control_paths_equal_generated_contract(self):
        header = read("crates/bio-abi/include/bioatmos_abi.h")
        literal = re.search(r'^#define BIO_CONTROL_DEVICE_PATH (".*")$', header, re.M)[1]
        contract = json.loads(literal)
        self.assertEqual(CONTROL_DEVICE_PATH, contract)
        ps = read("tools/ci/package-identities.ps1")
        self.assertEqual(re.search(r"\$SpatialControlDevicePath = '([^']+)'", ps)[1], contract)
        ring = read("crates/bio-sys/src/bio_ring.rs")
        self.assertIn("Device::open(CONTROL_DEVICE_PATH)", ring)
        for filename in ("driver_flow_check.py", "ioctl_fuzz.py"):
            source = read("tools/ci/" + filename)
            self.assertIn("import CONTROL_DEVICE_PATH", source)
            self.assertNotIn('r"\\\\.\\SpatialSoftReceiver"', source)
        kernel = read("driver/bioatmos/control.cpp")
        for prefix in ("Device", "DosDevices"):
            self.assertIn(json.dumps("\\" + prefix + "\\" + contract.split("\\")[-1]), kernel)

    def test_kit_and_removal_use_shared_identity_contract(self):
        kit = read("tools/ci/driver-test-kit.ps1")
        for name in ("endpoints.py", "endpoints.ps1", "package-identities.ps1"):
            self.assertIn(f"'{name}'", kit)
        self.assertIn('bio-atmos-driver-test-$Version-$Arch', kit)
        identities = read("tools/ci/package-identities.ps1")
        for legacy in ("ROOT\\BioAtmos", "bioatmos.inf", "Service = 'bioatmos'"):
            self.assertIn(legacy, identities)
        uninstall = read("tools/testkit/uninstall.ps1")
        self.assertIn("$SpatialLegacyPackages", uninstall)
        self.assertIn("sc.exe delete $package.Service", uninstall)
        for path in ("tools/ci/exclusive-policy.ps1", "tools/ci/endpoint-volume.ps1",
                     "tools/ci/endpoint-states.ps1", "tools/testkit/install.ps1", "tools/testkit/collect.ps1"):
            source = read(path)
            self.assertIn("Get-SpatialEndpoints", source)
            self.assertNotIn("Get-PurposeName", source)
            self.assertNotIn("$EndpointNames", source)

    def test_powershell_ownership_filter_does_not_trust_names(self):
        pwsh = os.environ.get("BIO_TEST_PWSH") or shutil.which("pwsh")
        if not pwsh:
            self.skipTest("set BIO_TEST_PWSH to test the PowerShell ownership filter")
        helper = str(ROOT / "tools/ci/endpoints.ps1").replace("'", "''")
        script = r'''
$ErrorActionPreference = 'Stop'
class SpatialEndpoints {
    static [string[]] Enumerate([string[]] $owners) { return $owners }
}
function Get-PnpDevice {
    param([string] $ErrorAction)
    [pscustomobject]@{ HardwareID=@('ROOT\SpatialSoftReceiver'); InstanceId='ROOT\MEDIA\0000'; FriendlyName='custom' }
    [pscustomobject]@{ HardwareID=@('ROOT\BioAtmos'); InstanceId='ROOT\MEDIA\0001'; FriendlyName='custom' }
    [pscustomobject]@{ HardwareID=@('OTHER'); InstanceId='ROOT\MEDIA\0002'; FriendlyName='Spatial SoftReceiver' }
}
. '__HELPER__'
$found = @(Get-SpatialEndpoints)
if ($found.Count -ne 1 -or $found[0] -ne 'ROOT\MEDIA\0000') { throw 'current ownership' }
if (@(Get-SpatialEndpoints -IncludeLegacy).Count -ne 2) { throw 'legacy ownership' }
if (@(Get-SpatialEndpoints -InstanceId 'ROOT\MEDIA\0002').Count) { throw 'InstanceId bypassed ownership' }
$found = @(Get-SpatialEndpoints -InstanceId 'root\media\0001' -IncludeLegacy)
if ($found.Count -ne 1 -or $found[0] -ne 'ROOT\MEDIA\0001') { throw 'legacy instance filter' }
'''.replace("__HELPER__", helper)
        run = subprocess.run([pwsh, "-NoProfile", "-Command", script],
                             capture_output=True, text=True, timeout=60)
        self.assertEqual(run.returncode, 0, run.stdout + run.stderr)

    def test_actual_csharp_filter_mapping(self):
        pwsh = os.environ.get("BIO_TEST_PWSH") or shutil.which("pwsh")
        if not pwsh:
            self.skipTest("set BIO_TEST_PWSH to compile and test actual C# mapping on Linux")
        # Runs only pure MapFilter, not Windows COM or PnP enumeration.
        helper = str(ROOT / "tools/ci/endpoints.ps1").replace("'", "''")
        script = r'''
$ErrorActionPreference = 'Stop'
. '__HELPER__'
$owners = @('ROOT\MEDIA\0000')
$guid = '{6994ad04-93ef-11d0-a3cc-00a0c9223196}'
foreach ($ep in 0..2) {
    foreach ($ref in "TopologyHdmi$ep", "WaveHdmi$ep") {
        foreach ($prefix in '\\?\', '\??\') {
            $path = "${prefix}root#media#0000#$guid\$ref"
            if ([SpatialEndpoints]::MapFilter($path, $owners) -ne $ep) { throw "mapping failed: $path" }
        }
    }
}
foreach ($path in @(
    "\\?\ROOT#MEDIA#0001#$guid\TopologyHdmi0",
    "\\?\ROOT#MEDIA#0000#$guid\TopologyHdmi3",
    "\\?\ROOT#MEDIA#0000#$guid\TopologyHdmi00",
    "\\?\ROOT#MEDIA#0000#$guid\TopologyHdmi0\extra",
    "\\?\ROOT#MEDIA#0000#$guid\NotTopologyHdmi0",
    'Spatial SoftReceiver', '공간 소프트리시버', '', $null)) {
    if ([SpatialEndpoints]::MapFilter($path, $owners) -ne -1) { throw "false identity: $path" }
}
# Dot-sourcing must not erase the caller's InstanceId parameter.
$InstanceId = 'ROOT\MEDIA\0000'
. '__HELPER__'
if ($InstanceId -ne 'ROOT\MEDIA\0000') { throw 'dot-source clobbered InstanceId' }
'''.replace("__HELPER__", helper)
        run = subprocess.run([pwsh, "-NoProfile", "-Command", script],
                             capture_output=True, text=True, timeout=60)
        self.assertEqual(run.returncode, 0, run.stdout + run.stderr)


if __name__ == "__main__":
    unittest.main()

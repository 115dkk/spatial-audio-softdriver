import json
from pathlib import Path
import tempfile
import unittest

from codeql_filter import filter_sarif, main


def result(rule, uri, line=1):
    return {
        "ruleId": rule,
        "message": {"text": "x"},
        "locations": [{"physicalLocation": {
            "artifactLocation": {"uri": uri, "uriBaseId": "%SRCROOT%"},
            "region": {"startLine": line},
        }}],
    }


def sarif(*results):
    return {"version": "2.1.0", "runs": [{"tool": {"driver": {"name": "CodeQL"}},
                                          "results": list(results)}]}


class CodeqlFilterTests(unittest.TestCase):
    def test_drops_wdk_headers_and_reviewed_pairs_and_keeps_the_rest(self):
        value = sarif(
            result("cpp/drivers/wdk-deprecated-api",
                   "driver/packages/Microsoft.Windows.WDK.x64.10.0.26100.6584/c/Include/km/wdm.h", 25613),
            result("cpp/drivers/init-not-cleared", "driver/spatialsoftreceiver/adapter.cpp", 20),
            # A reviewed rule elsewhere, and another rule in the reviewed file, stay.
            result("cpp/drivers/init-not-cleared", "driver/spatialsoftreceiver/control.cpp", 9),
            result("cpp/drivers/wdk-deprecated-api", "driver/spatialsoftreceiver/adapter.cpp", 30),
            # A path that only starts like the WDK folder stays.
            result("cpp/x", "driver/packagesx/a.h"),
        )
        kept, dropped = filter_sarif(value)
        self.assertEqual(kept, 3)
        self.assertEqual([entry[0:3] for entry in dropped], [
            ("cpp/drivers/wdk-deprecated-api",
             "driver/packages/Microsoft.Windows.WDK.x64.10.0.26100.6584/c/Include/km/wdm.h", 25613),
            ("cpp/drivers/init-not-cleared", "driver/spatialsoftreceiver/adapter.cpp", 20),
        ])
        self.assertEqual(
            [r["locations"][0]["physicalLocation"]["artifactLocation"]["uri"]
             for r in value["runs"][0]["results"]],
            ["driver/spatialsoftreceiver/control.cpp", "driver/spatialsoftreceiver/adapter.cpp",
             "driver/packagesx/a.h"],
        )

    def test_keeps_results_without_a_relative_location(self):
        value = sarif({"ruleId": "cpp/x", "message": {"text": "x"}},
                      result("cpp/x", "file:///D:/a/driver/packages/x.h"))
        kept, dropped = filter_sarif(value)
        self.assertEqual((kept, dropped), (2, []))

    def test_main_writes_every_file_under_its_name(self):
        with tempfile.TemporaryDirectory() as folder:
            source = Path(folder, "in")
            source.mkdir()
            Path(source, "cpp.sarif").write_text(json.dumps(sarif(
                result("cpp/x", "driver/packages/a.h"), result("cpp/x", "driver/core/b.cpp"))))
            target = Path(folder, "out")
            self.assertEqual(main([str(source), str(target)]), 0)
            written = json.loads(Path(target, "cpp.sarif").read_text())
            self.assertEqual(len(written["runs"][0]["results"]), 1)

    def test_main_rejects_an_empty_folder(self):
        with tempfile.TemporaryDirectory() as folder:
            with self.assertRaises(SystemExit):
                main([folder, str(Path(folder, "out"))])


if __name__ == "__main__":
    unittest.main()

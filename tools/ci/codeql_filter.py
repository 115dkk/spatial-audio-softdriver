"""Drops CodeQL results that are not about this repository's code.

The Windows driver suites analyse every header a driver includes, so results
land in the WDK that NuGet restores under ``driver/packages/`` (inline
functions in ``wdm.h`` and ``ntstrsafe.h``). That is Microsoft's code, and
CodeQL's ``paths-ignore`` cannot leave it out of a compiled language's
database. Results a reviewer has checked against our code and found the query
cannot follow are listed in ``REVIEWED`` with the reason.

Reads every ``*.sarif`` in the input folder and writes the kept results to the
output folder under the same names, for ``codeql-action/upload-sarif``. Prints
what it dropped.
"""

import argparse
import json
from pathlib import Path

# Paths, relative to the checkout, whose results are never ours to fix.
THIRD_PARTY_PREFIXES = ("driver/packages/",)

# (rule, file) pairs checked by hand, with the reason they are not defects.
REVIEWED = {
    (
        "cpp/drivers/init-not-cleared",
        "driver/spatialsoftreceiver/adapter.cpp",
    ): (
        "PcAddAdapterDevice creates the FDO and clears DO_DEVICE_INITIALIZING; "
        "AddDevice then finds that FDO in the PDO's stack and clears it again, "
        "which the query cannot trace"
    ),
}


def location(result):
    """The checkout-relative path of a result's first location, or ``None``."""
    try:
        uri = result["locations"][0]["physicalLocation"]["artifactLocation"]["uri"]
    except (KeyError, IndexError, TypeError):
        return None
    uri = uri.replace("\\", "/")
    if uri.startswith("file:///"):
        return None
    return uri.removeprefix("./")


def why_dropped(result):
    """The reason a result is not ours, or ``None`` to keep it."""
    path = location(result)
    if path is None:
        return None
    if path.startswith(THIRD_PARTY_PREFIXES):
        return "third-party header"
    return REVIEWED.get((result.get("ruleId"), path))


def filter_sarif(sarif):
    """Removes dropped results in place; returns ``(kept, dropped)`` where
    ``dropped`` lists ``(rule, path, line, reason)``."""
    kept = 0
    dropped = []
    for run in sarif.get("runs", []):
        results = []
        for result in run.get("results", []):
            reason = why_dropped(result)
            if reason is None:
                results.append(result)
                continue
            region = result["locations"][0]["physicalLocation"].get("region", {})
            dropped.append((result.get("ruleId"), location(result), region.get("startLine"), reason))
        kept += len(results)
        run["results"] = results
    return kept, dropped


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("input", type=Path, help="folder that holds the analysis's .sarif files")
    parser.add_argument("output", type=Path, help="folder to write the filtered .sarif files to")
    args = parser.parse_args(argv)
    files = sorted(args.input.glob("*.sarif"))
    if not files:
        parser.error(f"no .sarif files in {args.input}")
    args.output.mkdir(parents=True, exist_ok=True)
    for path in files:
        sarif = json.loads(path.read_text(encoding="utf-8"))
        kept, dropped = filter_sarif(sarif)
        (args.output / path.name).write_text(json.dumps(sarif), encoding="utf-8")
        print(f"{path.name}: kept {kept}, dropped {len(dropped)}")
        for rule, file, line, reason in dropped:
            print(f"  dropped {rule} at {file}:{line} ({reason})")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

"""Stable driver endpoint numbers from endpoints.ps1; names are display data."""

import json
from pathlib import Path
import subprocess

CONTROL_DEVICE_PATH = r"\\.\SpatialSoftReceiver"
ENDPOINT_NUMBERS = (0, 1, 2)


def parse_endpoints(text):
    """Validate the JSON boundary; duplicates are never resolved by name/order."""
    rows = json.loads(text.lstrip("\ufeff"))
    if not isinstance(rows, list):
        raise ValueError("endpoint JSON must be an array")
    ids = set()
    active = set()
    for row in rows:
        if not isinstance(row, dict):
            raise ValueError("endpoint must be an object")
        endpoint, state = row.get("endpoint"), row.get("state")
        if type(endpoint) is not int or endpoint not in ENDPOINT_NUMBERS:
            raise ValueError("invalid endpoint number")
        if type(state) is not int or state not in (1, 2, 4, 8):
            raise ValueError("invalid endpoint state")
        if not isinstance(row.get("id"), str) or not row["id"]:
            raise ValueError("missing endpoint id")
        if not isinstance(row.get("name"), str):
            raise ValueError("invalid endpoint name")
        key = row["id"].casefold()
        if key in ids or (state == 1 and endpoint in active):
            raise ValueError("duplicate endpoint identity")
        ids.add(key)
        if state == 1:
            active.add(endpoint)
    return rows


def enumerate_endpoints():
    helper = Path(__file__).with_suffix(".ps1")
    result = subprocess.run(
        ["powershell.exe", "-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass",
         "-File", str(helper)],
        check=True, capture_output=True, encoding="utf-8", timeout=30,
    )
    return parse_endpoints(result.stdout)


def find_speaker(soundcard, endpoint, rows=None):
    """Open by the exact IMMDevice id, never soundcard's fuzzy name fallback."""
    if endpoint not in ENDPOINT_NUMBERS:
        raise ValueError("unknown endpoint number")
    if rows is None:
        rows = enumerate_endpoints()
    matches = [row for row in rows if row["endpoint"] == endpoint and row["state"] == 1]
    if len(matches) > 1:
        raise ValueError("multiple active endpoints for one driver endpoint")
    if not matches:
        return None
    speaker = soundcard.get_speaker(id=matches[0]["id"])
    if speaker.id.casefold() != matches[0]["id"].casefold():
        raise ValueError("soundcard returned a different endpoint id")
    return speaker

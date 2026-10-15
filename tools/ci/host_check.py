"""Checks ``ssr-live host show`` against the PowerShell endpoint helper.

``--host`` is what the Rust host adapter (``ssr_sys::WindowsHost``) reports
through ``ssr-live host show``: the platform, the driver state, every render
endpoint (ours with its endpoint number) and the default device of each role.
``--endpoints`` is ``tools/ci/endpoints.ps1``'s array, which maps our endpoints
through the KS filter in C#. The two mappings were written apart, so they must
agree on every endpoint id and number.

Every failed check is one problem string. All are collected; the summary JSON
holds them with the parsed facts, and the exit code is 1 when there is any.
"""

import argparse
import json
from pathlib import Path
import sys

from endpoints import ENDPOINT_NUMBERS, parse_endpoints
from ssr_abi import DEFAULT_ENDPOINT_MASK

MIN_OS_BUILD = 19041  # Windows 10 2004, the oldest build the app supports.
ARCHES = ("x64", "arm64", "other")
SECURE_BOOT = ("enabled", "disabled", "unsupported")
DRIVER_STATES = ("not-installed", "installed", "reboot-required")
POLICIES = ("applied", "missing", "unknown")
OUTPUT_STATES = ("active", "disabled", "not-present", "unplugged")
ROLES = ("console", "multimedia", "communications")
# endpoints.ps1 reports IMMDevice::GetState (DEVICE_STATE_ACTIVE and so on).
POWERSHELL_STATES = {1: "active", 2: "disabled", 4: "not-present", 8: "unplugged"}
ALL_ENDPOINTS_MASK = sum(1 << endpoint for endpoint in ENDPOINT_NUMBERS)


def read_text(path):
    """A file as text, whether PowerShell wrote it as UTF-16 or UTF-8."""
    data = Path(path).read_bytes()
    if data.startswith((b"\xff\xfe", b"\xfe\xff")):
        return data.decode("utf-16")
    return data.decode("utf-8-sig")


def mask_endpoints(mask):
    return [endpoint for endpoint in ENDPOINT_NUMBERS if mask & (1 << endpoint)]


def same_id(left, right):
    return isinstance(left, str) and isinstance(right, str) and left.casefold() == right.casefold()


def check_platform(platform):
    if not isinstance(platform, dict):
        return ["host platform is not an object"]
    problems = []
    build = platform.get("os_build")
    if type(build) is not int or build < MIN_OS_BUILD:
        problems.append(f"os_build {build!r} is not a build number of at least {MIN_OS_BUILD}")
    if platform.get("arch") not in ARCHES:
        problems.append(f"arch {platform.get('arch')!r} is not one of {', '.join(ARCHES)}")
    if type(platform.get("test_signing")) is not bool:
        problems.append(f"test_signing {platform.get('test_signing')!r} is not a boolean")
    if platform.get("secure_boot") not in SECURE_BOOT:
        problems.append(
            f"secure_boot {platform.get('secure_boot')!r} is not one of {', '.join(SECURE_BOOT)}")
    return problems


def check_driver(driver, expect_driver, expect_policy):
    if not isinstance(driver, dict):
        return ["host driver is not an object"]
    state = driver.get("state")
    if state not in DRIVER_STATES:
        return [f"driver state {state!r} is not one of {', '.join(DRIVER_STATES)}"]
    if state != expect_driver:
        return [f"driver is {state}, expected {expect_driver}"]
    if state != "installed":
        return []
    problems = []
    code = driver.get("problem_code")
    if type(code) is not int:
        problems.append(f"driver problem_code {code!r} is not a number")
    elif code != 0:
        problems.append(f"driver problem code is {code}, expected 0")
    if driver.get("service_running") is not True:
        problems.append(f"driver service_running is {driver.get('service_running')!r}, expected true")
    policy = driver.get("exclusive_policy")
    if policy not in POLICIES:
        problems.append(f"exclusive_policy {policy!r} is not one of {', '.join(POLICIES)}")
    elif expect_policy != "any" and policy != expect_policy:
        problems.append(f"exclusive policy is {policy}, expected {expect_policy}")
    return problems


def valid_outputs(outputs):
    """The well-formed outputs, and a problem for each malformed one."""
    if not isinstance(outputs, list):
        return [], ["host outputs is not a list"]
    valid, problems = [], []
    for index, output in enumerate(outputs):
        if not isinstance(output, dict):
            problems.append(f"output {index} is not an object")
            continue
        endpoint = output.get("endpoint")
        if not isinstance(output.get("id"), str) or not output["id"]:
            problems.append(f"output {index} has no id")
        elif not isinstance(output.get("name"), str):
            problems.append(f"output {output['id']} has no name")
        elif output.get("state") not in OUTPUT_STATES:
            problems.append(f"output {output['id']} state {output.get('state')!r} is not one of "
                            f"{', '.join(OUTPUT_STATES)}")
        elif endpoint is not None and (type(endpoint) is not int or endpoint not in ENDPOINT_NUMBERS):
            problems.append(f"output {output['id']} carries endpoint {endpoint!r}, not one of "
                            f"{', '.join(map(str, ENDPOINT_NUMBERS))}")
        else:
            valid.append(output)
    return valid, problems


def check_outputs(outputs, endpoints, mask, expect_driver):
    """Our endpoints against the mask and against endpoints.ps1, both ways."""
    problems = []
    ours = [output for output in outputs if output["endpoint"] is not None]
    if expect_driver == "not-installed":
        for output in ours:
            problems.append(f"output {output['id']} carries endpoint {output['endpoint']} "
                            "although the driver is expected not installed")
    else:
        for endpoint in mask_endpoints(mask):
            active = [output for output in ours
                      if output["endpoint"] == endpoint and output["state"] == "active"]
            listed = [row for row in endpoints if row["endpoint"] == endpoint and row["state"] == 1]
            if len(active) != 1:
                problems.append(f"endpoint {endpoint}: {len(active)} active outputs carry it, "
                                "expected exactly one")
            if not listed:
                problems.append(f"endpoint {endpoint}: endpoints.json has no active endpoint {endpoint}")
            if len(active) == 1 and listed and not same_id(active[0]["id"], listed[0]["id"]):
                problems.append(f"endpoint {endpoint}: host reports {active[0]['id']}, "
                                f"endpoints.json {listed[0]['id']}")
        for output in ours:
            if not mask & (1 << output["endpoint"]) and output["state"] == "active":
                problems.append(f"endpoint {output['endpoint']} is outside the mask {mask:#x} "
                                f"but output {output['id']} is active")
    # The Rust KS mapping and the C# one agree on every endpoint either lists.
    listed_by_id = {row["id"].casefold(): row for row in endpoints}
    for output in ours:
        row = listed_by_id.get(output["id"].casefold())
        if row is None:
            problems.append(f"output {output['id']} carries endpoint {output['endpoint']}, "
                            "but endpoints.json does not list it")
        elif row["endpoint"] != output["endpoint"]:
            problems.append(f"output {output['id']} carries endpoint {output['endpoint']}, "
                            f"endpoints.json says {row['endpoint']}")
        elif POWERSHELL_STATES[row["state"]] != output["state"]:
            problems.append(f"output {output['id']} (endpoint {output['endpoint']}) is "
                            f"{output['state']}, endpoints.json says {POWERSHELL_STATES[row['state']]}")
    outputs_by_id = {output["id"].casefold(): output for output in outputs}
    for row in endpoints:
        output = outputs_by_id.get(row["id"].casefold())
        if output is None:
            problems.append(f"endpoints.json endpoint {row['endpoint']} ({row['id']}) "
                            "is not among the host outputs")
        elif output["endpoint"] is None:
            problems.append(f"endpoints.json maps {row['id']} to endpoint {row['endpoint']}, "
                            "the host gives it no endpoint number")
    return problems


def check_roles(roles, outputs):
    if not isinstance(roles, dict):
        return ["host default_roles is not an object"]
    problems = []
    known = {output["id"].casefold() for output in outputs}
    for role in ROLES:
        if role not in roles:
            problems.append(f"default_roles has no {role}")
            continue
        device = roles[role]
        if device is None:
            continue
        if not isinstance(device, str) or not device:
            problems.append(f"default {role} device {device!r} is not an endpoint id")
        elif device.casefold() not in known:
            problems.append(f"default {role} device {device} is not among the host outputs")
    return problems


def check(host, endpoints, mask, expect_driver, expect_policy):
    """Every problem with ``host`` (``host show``'s document) given
    ``endpoints`` (``parse_endpoints`` rows), the driver's endpoint ``mask`` and
    the expected driver state and exclusive policy."""
    if not isinstance(host, dict):
        return ["host document is not an object"]
    problems = []
    if expect_driver == "installed" and (not mask or mask & ~ALL_ENDPOINTS_MASK):
        problems.append(f"mask {mask:#x} does not name endpoints among "
                        f"{', '.join(map(str, ENDPOINT_NUMBERS))}")
    problems += check_platform(host.get("platform"))
    problems += check_driver(host.get("driver"), expect_driver, expect_policy)
    outputs, malformed = valid_outputs(host.get("outputs"))
    problems += malformed
    problems += check_outputs(outputs, endpoints, mask, expect_driver)
    problems += check_roles(host.get("default_roles"), outputs)
    return problems


def summary(host, endpoints, args, problems):
    outputs = host.get("outputs") if isinstance(host, dict) else None
    outputs = outputs if isinstance(outputs, list) else []
    return {
        "expect": {"mask": args.expect_mask, "driver": args.expect_driver,
                   "policy": args.expect_policy},
        "platform": host.get("platform") if isinstance(host, dict) else None,
        "driver": host.get("driver") if isinstance(host, dict) else None,
        "ours": [output for output in outputs
                 if isinstance(output, dict) and output.get("endpoint") is not None],
        "outputs": outputs,
        "default_roles": host.get("default_roles") if isinstance(host, dict) else None,
        "endpoints": endpoints,
        "problems": problems,
    }


def mask_argument(text):
    try:
        return int(text, 0)
    except ValueError:
        raise argparse.ArgumentTypeError(f"invalid endpoint mask {text!r}") from None


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--host", required=True, type=Path, help="ssr-live host show's output")
    parser.add_argument("--endpoints", required=True, type=Path, help="endpoints.ps1's output")
    parser.add_argument("--expect-mask", type=mask_argument, default=DEFAULT_ENDPOINT_MASK,
                        help="the driver's endpoint mask (default %(default)s)")
    parser.add_argument("--expect-driver", required=True, choices=("installed", "not-installed"))
    parser.add_argument("--expect-policy", default="any", choices=("applied", "missing", "any"),
                        help="the exclusive-mode policy an installed driver shows (default any)")
    parser.add_argument("--summary", required=True, type=Path)
    args = parser.parse_args(argv)

    problems, host, rows = [], {}, []
    try:
        host = json.loads(read_text(args.host))
    except (OSError, ValueError) as error:
        problems.append(f"{args.host}: {error}")
    try:
        rows = parse_endpoints(read_text(args.endpoints))
    except (OSError, ValueError) as error:
        problems.append(f"{args.endpoints}: {error}")
    if not problems:
        problems = check(host, rows, args.expect_mask, args.expect_driver, args.expect_policy)

    # ASCII JSON: a redirected Windows console encodes stdout in the ANSI code
    # page, which has no room for localized endpoint names.
    result = json.dumps(summary(host, rows, args, problems), indent=2)
    args.summary.write_text(result, encoding="utf-8")
    print(result)
    for problem in problems:
        print(problem, file=sys.stderr)
    return 1 if problems else 0


if __name__ == "__main__":
    raise SystemExit(main())

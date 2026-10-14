#!/usr/bin/env python3
"""Compiles the kernel driver's sources on Linux, without Windows.

clang in MSVC mode checks every translation unit of driver/spatialsoftreceiver against
the WDK and Windows SDK headers (taken from Microsoft's NuGet packages, pinned
by SHA-256) for x64 and ARM64, and optionally runs the clang static analyzer.
Nothing is linked, so this is a type and syntax check, not a build: the
Windows CI job builds and signs the real driver with MSVC and the WDK. It
exists so a driver change can be checked in the Linux container before CI.

    python3 tools/kcheck.py                 # both architectures
    python3 tools/kcheck.py --analyze       # plus clang --analyze (x64)
    python3 tools/kcheck.py driver/spatialsoftreceiver/control.cpp

The packages are cached in $SSR_KCHECK_CACHE (default ~/.cache/ssr-kcheck).
Windows file names are case-insensitive and the headers rely on that, so the
cache gains symlinks for every spelling an #include uses.
"""

import argparse
import hashlib
import os
import re
import shutil
import subprocess
import sys
import urllib.request
import zipfile
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
SDK_VERSION = "10.0.26100.0"
PACKAGES = [
    (
        "microsoft.windows.wdk.x64",
        "10.0.26100.6584",
        "c393d03dfb640b5c92f546b32f6770ef68cd3aaf691956e7d66d8e2c28a1b55e",
    ),
    (
        "microsoft.windows.sdk.cpp",
        "10.0.26100.6584",
        "5d31b38205bdd9ac761b4cb39fbbc6b7209b01c11194324afc674d7d119483a0",
    ),
]
INCLUDE_DIRS = ["km/crt", "km", "shared"]
DEFINES = {
    "x64": ["_AMD64_", "AMD64", "_WIN64"],
    "arm64": ["_ARM64_", "ARM64", "_WIN64", "_ARM64_WORKAROUND_"],
}
TARGETS = {"x64": "x86_64-pc-windows-msvc", "arm64": "aarch64-pc-windows-msvc"}
COMMON_DEFINES = [
    "_KERNEL_MODE",
    "NTDDI_VERSION=0x0A000008",  # the vcxproj and the INF: Windows 10 2004
    "_WIN32_WINNT=0x0A00",
    "WINVER=0x0A00",
    "POOL_NX_OPTIN=1",
    "POOL_ZERO_DOWN_LEVEL_SUPPORT=1",
    "NDEBUG",
]
INCLUDE_RE = re.compile(rb'#\s*include\s*[<"]([^">]+)[">]')


def cache_dir():
    return Path(os.environ.get("SSR_KCHECK_CACHE", Path.home() / ".cache" / "ssr-kcheck"))


def fetch(name, version, sha256, into):
    target = into / f"{name}.{version}.nupkg"
    if not target.exists():
        url = f"https://api.nuget.org/v3-flatcontainer/{name}/{version}/{name}.{version}.nupkg"
        print(f"kcheck: downloading {url}", file=sys.stderr)
        partial = target.with_suffix(".part")
        with urllib.request.urlopen(url) as response, open(partial, "wb") as out:
            shutil.copyfileobj(response, out)
        partial.rename(target)
    digest = hashlib.sha256(target.read_bytes()).hexdigest()
    if digest != sha256:
        target.unlink()
        raise SystemExit(f"kcheck: {target.name} has SHA-256 {digest}, expected {sha256}")
    return target


def casefold(roots, scan):
    """Adds a symlink for every #include spelling that differs in case."""
    index = {}
    for root in roots:
        for path in root.rglob("*"):
            if path.is_file():
                index.setdefault(str(path.relative_to(root)).lower(), []).append((root, path))
    names = set()
    for tree in [*roots, *scan]:
        for path in tree.rglob("*"):
            if path.is_file() and path.suffix.lower() in (".h", ".hpp", ".cpp", ".c", ".inl", ""):
                for match in INCLUDE_RE.finditer(path.read_bytes()):
                    names.add(match.group(1).decode("latin-1").replace("\\", "/"))
    for name in names:
        for root, actual in index.get(name.lower(), []):
            wanted = root / name
            if not wanted.exists():
                wanted.parent.mkdir(parents=True, exist_ok=True)
                wanted.symlink_to(actual)


def headers(scan):
    root = cache_dir()
    stamp = root / f"include-{SDK_VERSION}.ok"
    include = root / "c" / "Include" / SDK_VERSION
    if not stamp.exists():
        root.mkdir(parents=True, exist_ok=True)
        for name, version, sha256 in PACKAGES:
            package = fetch(name, version, sha256, root)
            with zipfile.ZipFile(package) as archive:
                prefix = f"c/Include/{SDK_VERSION}/"
                for member in archive.namelist():
                    if member.startswith(prefix) and member[len(prefix):].split("/")[0] in ("km", "shared"):
                        archive.extract(member, root)
        stamp.touch()
    roots = [include / d for d in INCLUDE_DIRS]
    casefold(roots, scan)
    return roots


def clang_resource_dir(clang):
    return subprocess.run([clang, "-print-resource-dir"], check=True, capture_output=True, text=True).stdout.strip()


def check(clang, source, arch, roots, extra_includes, analyze, defines=()):
    args = [
        clang,
        f"--target={TARGETS[arch]}",
        "-fms-extensions",
        "-fms-compatibility",
        "-fms-compatibility-version=19.40",
        "-std=c++23",
        "-fno-exceptions",
        "-fno-rtti",
        "-nostdinc",
        "-isystem", str(Path(clang_resource_dir(clang)) / "include"),
        "-Werror",
        "-Wall",
        "-Wno-unknown-pragmas",
        "-Wno-microsoft-anon-tag",
        "-Wno-microsoft-enum-forward-reference",
        "-Wno-ignored-pragma-intrinsic",
        "-Wno-pragma-pack",
        "-Wno-unused-value",
    ]
    for root in roots:
        args += ["-isystem", str(root)]
    for path in extra_includes:
        args += ["-I", str(path)]
    for define in DEFINES[arch] + COMMON_DEFINES + list(defines):
        args.append(f"-D{define}")
    if analyze:
        args += ["--analyze", "-Xanalyzer", "-analyzer-output=text", "-o", os.devnull]
    else:
        args.append("-fsyntax-only")
    args.append(str(source))
    result = subprocess.run(args, capture_output=True, text=True)
    output = (result.stdout + result.stderr).strip()
    if analyze and "warning:" in output:
        return False, output
    return result.returncode == 0, output


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("sources", nargs="*", type=Path)
    parser.add_argument("--arch", choices=sorted(TARGETS), action="append")
    parser.add_argument("--analyze", action="store_true", help="also run clang --analyze on x64")
    parser.add_argument("--clang", default=os.environ.get("CLANG", "clang++"))
    parser.add_argument("--define", action="append", default=[], metavar="NAME[=VALUE]",
                        help="extra preprocessor definition, as a diagnostic build passes through SsrDiagDefines")
    args = parser.parse_args()

    driver = REPO / "driver" / "spatialsoftreceiver"
    core = REPO / "driver" / "core"
    sources = args.sources or sorted(driver.glob("*.cpp")) + sorted(core.glob("*.cpp"))
    if not sources:
        raise SystemExit("kcheck: no driver sources found")
    # driver/kcrt first, as in the vcxproj: the kernel build has no user-mode CRT.
    extra = [REPO / "driver" / "kcrt", driver, core, REPO / "crates" / "ssr-abi" / "include"]
    roots = headers([REPO / "driver"])

    failed = 0
    for arch in args.arch or ["x64", "arm64"]:
        for source in sources:
            ok, output = check(args.clang, source, arch, roots, extra, False, args.define)
            print(f"kcheck {arch} {source.relative_to(REPO) if source.is_absolute() else source}: {'ok' if ok else 'FAILED'}")
            if output:
                print(output)
            failed += not ok
    if args.analyze:
        for source in sources:
            ok, output = check(args.clang, source, "x64", roots, extra, True, args.define)
            print(f"kcheck analyze {source}: {'ok' if ok else 'FAILED'}")
            if output:
                print(output)
            failed += not ok
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())

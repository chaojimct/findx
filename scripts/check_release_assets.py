"""Fail-closed check of files about to be published as a GitHub Release.

Protects the official Windows native setup from being overwritten or mixed with
Tauri/WebView or Unix-only leftovers. No network, no writes, no tagging.
"""
from __future__ import annotations
import argparse, re, sys
from pathlib import Path

WINDOWS_SETUP = re.compile(r"^FindX-(\d+\.\d+\.\d+)-setup\.exe$")
LINUX_DEB = re.compile(r"^FindX-(\d+\.\d+\.\d+)-linux-x64\.deb$")
LINUX_TAR = re.compile(r"^FindX-(\d+\.\d+\.\d+)-linux-x64\.tar\.gz$")
MAC_DMG = re.compile(r"^FindX-(\d+\.\d+\.\d+)-macos-(x64|aarch64)\.dmg$")
SHA = re.compile(r"^FindX-(\d+\.\d+\.\d+)-(linux-x64|macos-(x64|aarch64))\.sha256$")
ALLOWED = (WINDOWS_SETUP, LINUX_DEB, LINUX_TAR, MAC_DMG, SHA)

def classify(name: str):
    for rx in ALLOWED:
        m = rx.match(name)
        if m:
            return rx, m.group(1)
    return None, None

def validate(dist: Path, expected: str, require_unix: bool = True) -> list[str]:
    errors = []
    if not dist.is_dir():
        return [f"dist directory missing: {dist}"]
    names = sorted(p.name for p in dist.iterdir() if p.is_file())
    versions = set()
    has_win = has_deb = has_tar = has_mac = False
    for name in names:
        lower = name.lower()
        if "isolated" in lower or "fixture" in lower or "drill" in lower:
            errors.append(f"refusing isolated/fixture/drill asset: {name}")
            continue
        if "tauri" in lower or "webview" in lower:
            errors.append(f"refusing Tauri/WebView asset: {name}")
            continue
        rx, ver = classify(name)
        if rx is None:
            # A second *setup.exe of any other shape is the historical footgun.
            if lower.endswith("setup.exe") or lower.endswith(".msi"):
                errors.append(f"refusing non-official Windows installer name: {name}")
            else:
                errors.append(f"refusing unexpected release file: {name}")
            continue
        versions.add(ver)
        if rx is WINDOWS_SETUP:
            has_win = True
        elif rx is LINUX_DEB:
            has_deb = True
        elif rx is LINUX_TAR:
            has_tar = True
        elif rx is MAC_DMG:
            has_mac = True
        path = dist / name
        if path.stat().st_size < 1024:
            errors.append(f"asset is too small to be a real official package: {name}")
    if expected:
        if any(v != expected for v in versions):
            errors.append(f"asset version(s) {sorted(versions)} != expected {expected}")
    if not has_win:
        errors.append("missing official Windows native setup FindX-<ver>-setup.exe")
    if require_unix:
        if not (has_deb and has_tar):
            errors.append("missing official Linux packages (.deb and .tar.gz)")
        if not has_mac:
            errors.append("missing official macOS .dmg (unsigned is allowed; a fake/empty file is not)")
    return errors

if __name__ == "__main__":
    p = argparse.ArgumentParser()
    p.add_argument("--dist", required=True)
    p.add_argument("--expected-version", required=True)
    p.add_argument("--allow-missing-unix", action="store_true")
    a = p.parse_args()
    errors = validate(Path(a.dist), a.expected_version, require_unix=not a.allow_missing_unix)
    print("\n".join(errors) if errors else "ok")
    sys.exit(bool(errors))

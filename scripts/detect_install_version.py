#!/usr/bin/env python3
"""Detect a FindX install's version relative to a 2.5.0 (or other) package.

Read-only with respect to official paths. Never writes, never starts SCM,
and never treats official or experimental-live locations as executable upgrade targets.
"""
from __future__ import print_function

import argparse
import json
import os
import re
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import isolated_upgrade_tx as tx

EXPECTED_EXES = ("FindX.exe", "findx2-service.exe", "findx2.exe", "fx.exe")
EXPECTED_PRODUCT_NAMES = {"findx"}
VERSION_RE = re.compile(r"^(\d+)\.(\d+)\.(\d+)(?:\..*)?$")


def parse_version(raw):
    if not raw:
        return None
    text = str(raw).strip()
    # PE sometimes returns "2.5.0\0" or "2.5.0.0"
    text = text.strip("\x00").strip()
    m = VERSION_RE.match(text)
    if not m:
        return None
    return tuple(int(p) for p in m.groups())


def version_str(ver):
    if not ver:
        return None
    return "%s.%s.%s" % ver


def _query_string(buf, block, key):
    import ctypes
    from ctypes import wintypes

    u = ctypes.c_void_p()
    n = wintypes.UINT()
    if ctypes.windll.version.VerQueryValueW(buf, block + key, ctypes.byref(u), ctypes.byref(n)) and u.value:
        return ctypes.wstring_at(u.value).strip("\x00").strip()
    return None


def read_pe_metadata(path):
    """Return version/identity strings from a Windows PE resource. None if absent."""
    path = Path(path)
    result = {
        "path": str(path),
        "exists": path.is_file(),
        "length": path.stat().st_size if path.is_file() else 0,
        "file_version": None,
        "product_version": None,
        "product_name": None,
        "company_name": None,
        "file_description": None,
        "original_filename": None,
        "error": None,
    }
    if not path.is_file():
        result["error"] = "missing"
        return result
    if result["length"] == 0:
        result["error"] = "empty"
        return result
    try:
        import ctypes
        from ctypes import wintypes

        size = ctypes.windll.version.GetFileVersionInfoSizeW(str(path), None)
        if not size:
            result["error"] = "no_version_resource"
            return result
        buf = ctypes.create_string_buffer(size)
        if not ctypes.windll.version.GetFileVersionInfoW(str(path), 0, size, buf):
            result["error"] = "version_resource_unreadable"
            return result
        u = ctypes.c_void_p()
        n = wintypes.UINT()
        blocks = []
        if ctypes.windll.version.VerQueryValueW(buf, r"\VarFileInfo\Translation", ctypes.byref(u), ctypes.byref(n)) and u.value and n.value >= 4:
            trans = ctypes.string_at(u.value, 4)
            lang = int.from_bytes(trans[:2], "little")
            code = int.from_bytes(trans[2:], "little")
            blocks.append(r"\StringFileInfo\%04X%04X\\" % (lang, code))
        blocks.append(r"\StringFileInfo\040904B0\\")
        blocks.append(r"\StringFileInfo\000004B0\\")
        seen = set()
        for block in blocks:
            if block in seen:
                continue
            seen.add(block)
            for field, key in (
                ("file_version", "FileVersion"),
                ("product_version", "ProductVersion"),
                ("product_name", "ProductName"),
                ("company_name", "CompanyName"),
                ("file_description", "FileDescription"),
                ("original_filename", "OriginalFilename"),
            ):
                if result[field]:
                    continue
                value = _query_string(buf, block, key)
                if value:
                    result[field] = value
        if not result["product_version"] and not result["file_version"]:
            # Fall back to the binary FI fixed field.
            if ctypes.windll.version.VerQueryValueW(buf, r"\\", ctypes.byref(u), ctypes.byref(n)) and u.value:
                class VS_FIXEDFILEINFO(ctypes.Structure):
                    _fields_ = [
                        ("dwSignature", wintypes.DWORD),
                        ("dwStrucVersion", wintypes.DWORD),
                        ("dwFileVersionMS", wintypes.DWORD),
                        ("dwFileVersionLS", wintypes.DWORD),
                        ("dwProductVersionMS", wintypes.DWORD),
                        ("dwProductVersionLS", wintypes.DWORD),
                    ]
                info = VS_FIXEDFILEINFO.from_address(u.value)
                result["product_version"] = "%s.%s.%s" % (
                    info.dwProductVersionMS >> 16,
                    info.dwProductVersionMS & 0xFFFF,
                    info.dwProductVersionLS >> 16,
                )
        if not result["product_version"] and not result["file_version"]:
            result["error"] = "version_strings_missing"
        else:
            result["error"] = None
        return result
    except Exception as exc:
        result["error"] = "version_read_failed: %s" % exc
        return result


def classify_location(path):
    p = tx._norm(path)
    for label, root in (
        ("experimental_dev", tx.EXPERIMENTAL_DEV),
        ("official", tx.OFFICIAL_INSTALL),
        ("official", tx.OFFICIAL_INSTALL_X86),
        ("official", tx.OFFICIAL_PROGRAMDATA),
        ("official", tx.OFFICIAL_APPDATA_GUI),
    ):
        if p == tx._norm(root) or tx._is_relative_to(p, root):
            return label, "protected location: %s" % p
    reason = tx.forbidden_reason(p)
    if reason and "volume root" in reason:
        return "volume_root", reason
    return "isolated_or_other", None


def package_from_dir(package_dir):
    package_dir = tx._norm(package_dir)
    files = {}
    for name in EXPECTED_EXES:
        path = package_dir / name
        meta = read_pe_metadata(path) if path.exists() else {"path": str(path), "exists": False, "error": "missing", "product_version": None, "file_version": None, "product_name": None}
        files[name] = meta
    main = files["FindX.exe"]
    version = parse_version(main.get("product_version") or main.get("file_version"))
    manifest = None
    manifest_path = package_dir / "stage-manifest.json"
    if manifest_path.exists():
        manifest = tx.read_json(manifest_path)
        if not version and manifest.get("version"):
            version = parse_version(manifest.get("version"))
    missing_pe = [name for name, meta in files.items() if meta.get("exists") and not parse_version(meta.get("product_version") or meta.get("file_version"))]
    return {
        "dir": str(package_dir),
        "version": version_str(version),
        "version_tuple": version,
        "files": files,
        "manifest_version": (manifest or {}).get("version"),
        "companions_missing_pe": missing_pe,
        "product_name": main.get("product_name"),
    }


def inspect_install(install_dir):
    install_dir = Path(install_dir) if install_dir else None
    report = {
        "dir": str(install_dir) if install_dir else None,
        "present": False,
        "location": "none",
        "location_reason": None,
        "files": {},
        "missing_files": [],
        "empty_or_unreadable": [],
        "companions_missing_pe": [],
        "marker_present": False,
        "product_name": None,
        "version": None,
        "version_tuple": None,
        "identity_ok": False,
        "partial": False,
        "corrupt": False,
    }
    if not install_dir or not Path(install_dir).exists():
        return report
    install_dir = tx._norm(install_dir)
    report["dir"] = str(install_dir)
    report["location"], report["location_reason"] = classify_location(install_dir)
    if not install_dir.is_dir():
        report["corrupt"] = True
        report["empty_or_unreadable"].append(str(install_dir))
        return report

    marker = install_dir / "FindX.installed"
    report["marker_present"] = marker.is_file() and marker.stat().st_size > 0

    any_expected = False
    for name in EXPECTED_EXES:
        path = install_dir / name
        meta = read_pe_metadata(path)
        report["files"][name] = meta
        if path.exists():
            any_expected = True
        else:
            report["missing_files"].append(name)
        if path.exists() and (meta.get("error") in ("empty", "version_resource_unreadable", "version_read_failed") or meta.get("length") == 0):
            if meta.get("error") == "empty" or meta.get("length") == 0:
                report["empty_or_unreadable"].append(name)
        if path.exists() and not parse_version(meta.get("product_version") or meta.get("file_version")):
            report["companions_missing_pe"].append(name)

    report["present"] = any_expected or report["marker_present"] or any(install_dir.glob("*.exe"))
    main = report["files"].get("FindX.exe") or {}
    report["product_name"] = main.get("product_name")
    report["version_tuple"] = parse_version(main.get("product_version") or main.get("file_version"))
    report["version"] = version_str(report["version_tuple"])

    # Official 2.4.10 ships companions without PE resources. That is a known
    # packaging gap, not by itself a corrupt install, if FindX.exe identifies the product.
    if "FindX.exe" in report["companions_missing_pe"]:
        # already listed; keep
        pass
    else:
        report["companions_missing_pe"] = [n for n in report["companions_missing_pe"] if n != "FindX.exe"]

    product = (report["product_name"] or "").strip().lower()
    if report["version_tuple"] and product in EXPECTED_PRODUCT_NAMES:
        report["identity_ok"] = True
    elif report["version_tuple"] and product and product not in EXPECTED_PRODUCT_NAMES:
        report["identity_ok"] = False
    elif not main.get("exists"):
        report["identity_ok"] = False
    else:
        report["identity_ok"] = False

    if report["empty_or_unreadable"]:
        report["corrupt"] = True
    if report["present"] and (report["missing_files"] or not report["marker_present"] or not main.get("exists")):
        report["partial"] = True
    # A complete 2.4.10-like tree has all exes + marker + FindX.exe identity,
    # even if companions lack PE.
    if report["identity_ok"] and not report["missing_files"] and report["marker_present"] and not report["corrupt"]:
        report["partial"] = False
    return report


def detect(install_dir, package_dir=None, package_version=None):
    package = None
    pkg_ver = parse_version(package_version) if package_version else None
    if package_dir:
        package = package_from_dir(package_dir)
        pkg_ver = package.get("version_tuple") or pkg_ver
        if package.get("product_name") and package["product_name"].strip().lower() not in EXPECTED_PRODUCT_NAMES:
            return _result(
                "identity_mismatch",
                "refuse",
                inspect_install(install_dir),
                package,
                pkg_ver,
                "package ProductName is not FindX",
            )
        if package and package.get("files", {}).get("FindX.exe", {}).get("exists") and not pkg_ver:
            return _result(
                "partial_or_corrupt",
                "refuse",
                inspect_install(install_dir),
                package,
                None,
                "package FindX.exe has no readable product version",
            )

    installed = inspect_install(install_dir)
    if not installed["present"]:
        return _result("fresh", "install_fresh" if pkg_ver else "none", installed, package, pkg_ver, "no existing FindX install at the target directory")

    product = (installed.get("product_name") or "").strip().lower()
    if installed["files"].get("FindX.exe", {}).get("exists") and product and product not in EXPECTED_PRODUCT_NAMES:
        return _result("identity_mismatch", "refuse", installed, package, pkg_ver, "existing FindX.exe ProductName is not FindX")

    if installed["corrupt"] or (installed["partial"] and not installed["identity_ok"]):
        # Missing primary exe, garbage file, or incomplete tree without identity.
        return _result("partial_or_corrupt", "refuse", installed, package, pkg_ver, "existing tree is missing, unreadable, or only a partial FindX install")

    if not installed["identity_ok"]:
        if installed["present"] and not installed["version_tuple"]:
            return _result("partial_or_corrupt", "refuse", installed, package, pkg_ver, "existing FindX.exe has no readable product version")
        return _result("identity_mismatch", "refuse", installed, package, pkg_ver, "existing directory does not look like a FindX install")

    if installed["partial"]:
        return _result("partial_or_corrupt", "refuse", installed, package, pkg_ver, "existing FindX install is incomplete")

    if not pkg_ver:
        return _result("upgrade", "refuse", installed, package, pkg_ver, "package version is unknown; no action selected")

    inst = installed["version_tuple"]
    if inst == pkg_ver:
        decision, action, reason = "same", "none", "installed version equals package version"
    elif inst < pkg_ver:
        decision, action, reason = "upgrade", "run_upgrade_transaction", "installed %s is older than package %s" % (version_str(inst), version_str(pkg_ver))
    else:
        decision, action, reason = "downgrade_blocked", "refuse", "package %s is older than installed %s" % (version_str(pkg_ver), version_str(inst))

    # Official / experimental / volume-root trees may be *read* but must not be mutated.
    if installed["location"] in ("official", "experimental_dev", "volume_root"):
        action = "refuse"
        reason = "%s; execution is blocked because the install lives in a protected location (%s)" % (reason, installed["location"])
    return _result(decision, action, installed, package, pkg_ver, reason)


def _result(decision, action, installed, package, pkg_ver, reason):
    execution_allowed = action in ("install_fresh", "run_upgrade_transaction")
    return {
        "decision": decision,
        "action": action,
        "execution_allowed": execution_allowed,
        "reason": reason,
        "installed": installed,
        "package": package,
        "package_version": version_str(pkg_ver) if pkg_ver else (package or {}).get("version"),
    }


def main(argv=None):
    parser = argparse.ArgumentParser(description="Detect a FindX install relative to a package (read-only).")
    parser.add_argument("--install-dir", required=True, help="Existing or intended install directory")
    parser.add_argument("--package-dir", help="Staged package or extracted setup directory")
    parser.add_argument("--package-version", help="Package version if no package dir is given")
    args = parser.parse_args(argv)
    result = detect(args.install_dir, package_dir=args.package_dir, package_version=args.package_version)
    json.dump(result, sys.stdout, indent=2, ensure_ascii=False)
    sys.stdout.write("\n")
    return 0 if result["decision"] in ("fresh", "upgrade", "same") else 2


if __name__ == "__main__":
    sys.exit(main())

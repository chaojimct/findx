import hashlib
import json
import os
import shutil
import time
from pathlib import Path

# Isolated upgrade transaction for FindX 2.4.10 -> 2.5.0.
# Never touches the official install, ProgramData index, real SCM, or HKCU Run.

OFFICIAL_INSTALL = Path(r"C:\Program Files\FindX")
OFFICIAL_INSTALL_X86 = Path(r"C:\Program Files (x86)\FindX")
OFFICIAL_PROGRAMDATA = Path(r"C:\ProgramData\FindX")
OFFICIAL_APPDATA_GUI = Path(os.environ.get("APPDATA", r"C:\Users\Public")) / "tools.findx.gui"
EXPERIMENTAL_DEV = Path(os.environ.get("LOCALAPPDATA", r"C:\Users\Public")) / "FindX-native-dev"

INSTALL_FILES = (
    "FindX.exe",
    "FindX.installed",
    "findx2-service.exe",
    "findx2.exe",
    "fx.exe",
)
INDEX_SIDECAR_NAMES = (
    "index.bin",
    "manifest.json",
    "published.json",
    "entries.dat",
    "frns.dat",
    "dirs.dat",
    "ranges.dat",
    "names.dat",
    "dirpaths.dat",
    "index.bin.tri",
    "index.bin.tri.pending",
    "index.bin.exclude.json",
    "index.bin.v6",
)

MARKER_BYTES = b"1\r\n"


class UpgradeError(Exception):
    def __init__(self, message, journal=None):
        super().__init__(message)
        self.journal = journal


def _norm(path):
    return Path(os.path.abspath(os.path.expandvars(str(path))))


def sha256_file(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1024 * 1024), b""):
            h.update(chunk)
    return h.hexdigest()


def _is_relative_to(path, root):
    try:
        _norm(path).relative_to(_norm(root))
        return True
    except ValueError:
        return False


def forbidden_reason(path):
    raw = str(path).strip().strip('"')
    compact = raw.replace("/", "\\").rstrip("\\")
    if len(compact) == 2 and compact[1] == ":" and compact[0].isalpha():
        return "refusing volume root: %s" % path
    p = _norm(path)
    blocked = (
        OFFICIAL_INSTALL,
        OFFICIAL_INSTALL_X86,
        OFFICIAL_PROGRAMDATA,
        OFFICIAL_APPDATA_GUI,
        EXPERIMENTAL_DEV,
    )
    for root in blocked:
        if p == _norm(root) or _is_relative_to(p, root):
            return "refusing official or experimental-live path: %s" % p
    if p.drive and list(p.parts) == [p.anchor]:
        return "refusing volume root: %s" % p
    return None


def assert_allowed(path, allowed_root):
    p = _norm(path)
    reason = forbidden_reason(p)
    if reason:
        raise UpgradeError(reason)
    if not _is_relative_to(p, allowed_root):
        raise UpgradeError("path is outside the allowed isolated root %s: %s" % (allowed_root, p))
    return p


def write_json(path, obj):
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_name(".findx-tx-%s-%s" % (os.getpid(), path.name))
    text = json.dumps(obj, indent=2, sort_keys=True, ensure_ascii=False) + "\n"
    with open(tmp, "wb") as f:
        f.write(text.encode("utf-8"))
        f.flush()
        os.fsync(f.fileno())
    os.replace(tmp, path)


def read_json(path):
    with open(path, "r", encoding="utf-8") as f:
        return json.load(f)


def copy_file_atomic(src, dest, *, create_new=False):
    dest = Path(dest)
    dest.parent.mkdir(parents=True, exist_ok=True)
    tmp = dest.with_name(".findx-new-%s-%s" % (os.getpid(), dest.name))
    if tmp.exists():
        tmp.unlink()
    with open(src, "rb") as inf, open(tmp, "wb") as out:
        shutil.copyfileobj(inf, out, length=1024 * 1024)
        out.flush()
        os.fsync(out.fileno())
    if create_new and dest.exists():
        tmp.unlink()
        raise UpgradeError("refusing to overwrite existing file: %s" % dest)
    os.replace(tmp, dest)


def tree_manifest(root):
    root = _norm(root)
    files = []
    if not root.exists():
        return files
    for dirpath, dirnames, filenames in os.walk(root):
        dirnames.sort()
        for name in sorted(filenames):
            path = Path(dirpath) / name
            rel = path.relative_to(root).as_posix()
            files.append({
                "rel": rel,
                "sha256": sha256_file(path),
                "length": path.stat().st_size,
            })
    return files


def collect_isolated_files(layout):
    """Map relative keys to absolute paths for the complete install set."""
    mapping = {}
    sections = {
        "install": layout["install_dir"],
        "data": layout["data_dir"],
        "appdata": layout["appdata_dir"],
        "scm": layout["scm_dir"],
        "run": layout["run_dir"],
    }
    for prefix, root in sections.items():
        root = Path(root)
        if not root.exists():
            continue
        for dirpath, dirnames, filenames in os.walk(root):
            dirnames.sort()
            for name in sorted(filenames):
                path = Path(dirpath) / name
                rel = "%s/%s" % (prefix, path.relative_to(root).as_posix())
                mapping[rel] = path
    return mapping


def write_backup(layout, backup_dir):
    backup_dir = Path(backup_dir)
    if backup_dir.exists():
        raise UpgradeError("backup dir already exists: %s" % backup_dir)
    backup_dir.mkdir(parents=True)
    files = []
    for rel, src in collect_isolated_files(layout).items():
        dest = backup_dir / rel
        copy_file_atomic(src, dest, create_new=True)
        files.append({
            "rel": rel,
            "sha256": sha256_file(dest),
            "length": dest.stat().st_size,
        })
    manifest = {
        "kind": "isolated-upgrade-backup",
        "created": time.strftime("%Y-%m-%dT%H:%M:%S%z"),
        "files": files,
    }
    write_json(backup_dir / "backup-manifest.json", manifest)
    return manifest


def restore_backup(layout, backup_dir):
    backup_dir = Path(backup_dir)
    manifest = read_json(backup_dir / "backup-manifest.json")
    current = collect_isolated_files(layout)
    expected = set()
    for item in manifest["files"]:
        rel = item["rel"]
        expected.add(rel)
        src = backup_dir / rel
        if sha256_file(src) != item["sha256"]:
            raise UpgradeError("backup itself is corrupt: %s" % rel)
        prefix, _, rest = rel.partition("/")
        dest_root = {
            "install": layout["install_dir"],
            "data": layout["data_dir"],
            "appdata": layout["appdata_dir"],
            "scm": layout["scm_dir"],
            "run": layout["run_dir"],
        }[prefix]
        dest = Path(dest_root) / rest
        copy_file_atomic(src, dest)
        if sha256_file(dest) != item["sha256"]:
            raise UpgradeError("restore mismatch: %s" % rel)
    # Remove files added by a partial upgrade so the old complete set is exact.
    for rel, path in current.items():
        if rel not in expected and path.exists():
            path.unlink()
    return verify_against_backup(layout, backup_dir)


def verify_against_backup(layout, backup_dir):
    manifest = read_json(Path(backup_dir) / "backup-manifest.json")
    current = collect_isolated_files(layout)
    expected = {item["rel"]: item for item in manifest["files"]}
    extra = sorted(set(current) - set(expected))
    missing = sorted(set(expected) - set(current))
    mismatches = []
    for rel, item in expected.items():
        if rel not in current:
            continue
        digest = sha256_file(current[rel])
        length = current[rel].stat().st_size
        if digest != item["sha256"] or length != item["length"]:
            mismatches.append(rel)
    ok = not extra and not missing and not mismatches
    return {
        "identical": ok,
        "extra": extra,
        "missing": missing,
        "mismatches": mismatches,
    }


KNOWN_SETTINGS_FIELDS = (
    "indexPath",
    "volume",
    "pipeName",
    "pinyinDefault",
    "serviceExePath",
    "searchLimit",
    "autoStartService",
    "firstIndexFullMetadata",
    "runMode",
    "drives",
    "excludedDirs",
    "enableMetadataBackfill",
    "enableEverythingIpc",
    "saveIntervalSecs",
    "autoStartApp",
    "autoCheckUpdate",
    "theme",
)


def migrate_official_settings(source_json, dest_settings, dest_prefs, *, isolated_index, isolated_pipe, isolated_service, official_source_must_not_be_live=True):
    """Copy known official GUI settings into a NEW native profile. Never writes official AppData."""
    src = _norm(source_json)
    dest_settings = _norm(dest_settings)
    dest_prefs = _norm(dest_prefs)
    for path in (src, dest_settings, dest_prefs):
        reason = forbidden_reason(path)
        # Reading a *fixture copy* of official-format JSON is fine even if a test
        # used official field names; writing official AppData is not.
        if path in (dest_settings, dest_prefs) and reason:
            raise UpgradeError(reason)
        if path == dest_settings or path == dest_prefs:
            if _is_relative_to(path, OFFICIAL_APPDATA_GUI) or path == _norm(OFFICIAL_APPDATA_GUI / "findx2-gui-settings.json"):
                raise UpgradeError("refusing to write official GUI settings")
    if official_source_must_not_be_live and (src == _norm(OFFICIAL_APPDATA_GUI / "findx2-gui-settings.json") or _is_relative_to(src, OFFICIAL_APPDATA_GUI)):
        # Live official settings may be *read* only when an explicit operator
        # copies them into the isolated fixture first. Direct live import is refused.
        raise UpgradeError("refusing to read live official AppData; copy into the isolated fixture first")

    data = read_json(src)
    if not isinstance(data, dict):
        raise UpgradeError("official settings must be a JSON object")

    unknown = {k: v for k, v in data.items() if k not in KNOWN_SETTINGS_FIELDS}
    theme = data.get("theme", "light")
    if theme not in ("light", "dark", "system"):
        theme = "light"

    index_path = isolated_index
    pipe_name = isolated_pipe
    service_exe = isolated_service
    # Never carry experimental dev or official production paths into the new profile.
    incoming_index = str(data.get("indexPath") or "")
    incoming_pipe = str(data.get("pipeName") or "")
    if incoming_index:
        if forbidden_reason(incoming_index) or "FindX-native-dev" in incoming_index:
            incoming_index = ""  # drop, use isolated
    settings = {
        "indexPath": index_path,
        "volume": data.get("volume") or "",
        "pipeName": pipe_name,
        "pinyinDefault": bool(data.get("pinyinDefault", True)),
        "serviceExePath": service_exe,
        "searchLimit": int(data.get("searchLimit") or 5000),
        "autoStartService": False,  # isolated drill never auto-starts SCM
        "firstIndexFullMetadata": bool(data.get("firstIndexFullMetadata", False)),
        "runMode": "standalone",
        "drives": list(data.get("drives") or []),
        "excludedDirs": list(data.get("excludedDirs") or []),
        "enableMetadataBackfill": bool(data.get("enableMetadataBackfill", True)),
        "enableEverythingIpc": bool(data.get("enableEverythingIpc", True)),
        "saveIntervalSecs": int(data.get("saveIntervalSecs") or 30),
        "autoStartApp": bool(data.get("autoStartApp", False)),
        "autoCheckUpdate": bool(data.get("autoCheckUpdate", False)),
    }
    prefs = {
        "usageReduceMotion": False,
        "browserFavorites": list(data.get("browserFavorites") or []),
        "browserRecent": list(data.get("browserRecent") or []),
        "browserShowHidden": bool(data.get("browserShowHidden", False)),
        "browserColumnSchema": int(data.get("browserColumnSchema") or 0),
        "theme": theme,
        "themeStyle": data.get("themeStyle") or "default",
        "diagEnabled": False,
        "sidebarOpen": True,
        "previewOpen": True,
        "previewWidth": None,
        "columnWidths": None,
        "searchFitColumns": True,
        "browserFitColumns": True,
        "browserColumnWidths": None,
    }
    if dest_settings.exists():
        copy_file_atomic(dest_settings, dest_settings.with_suffix(dest_settings.suffix + ".pre-migrate.bak"))
    write_json(dest_settings, settings)
    write_json(dest_prefs, prefs)
    unknown_path = dest_settings.with_name("imported-official-unknown.json")
    write_json(unknown_path, unknown)
    return {
        "settings": settings,
        "prefs": prefs,
        "unknown_fields": sorted(unknown),
        "dropped_incoming_index": bool(data.get("indexPath") and data.get("indexPath") != index_path),
        "incoming_pipe_ignored": incoming_pipe,
    }


def default_layout(root):
    root = _norm(root)
    return {
        "root": root,
        "install_dir": root / "install",
        "data_dir": root / "data",
        "appdata_dir": root / "appdata",
        "scm_dir": root / "scm",
        "run_dir": root / "run",
        "tx_dir": root / "tx",
    }


def prepare_old_tree(layout, old_payload, index_payload, official_settings_fixture, allowed_root):
    """Populate an isolated 2.4.10-style tree. Sources must be readable copies, not live writes."""
    for key in ("install_dir", "data_dir", "appdata_dir", "scm_dir", "run_dir", "tx_dir"):
        assert_allowed(layout[key], allowed_root)
        layout[key].mkdir(parents=True, exist_ok=True)
    old_payload = _norm(old_payload)
    for name in INSTALL_FILES:
        src = old_payload / name
        if not src.exists():
            if name == "FindX.installed":
                dest = layout["install_dir"] / name
                dest.write_bytes(MARKER_BYTES)
                continue
            raise UpgradeError("old payload missing %s" % name)
        copy_file_atomic(src, layout["install_dir"] / name, create_new=True)
    index_payload = _norm(index_payload)
    if index_payload.is_file():
        copy_file_atomic(index_payload, layout["data_dir"] / "index.bin", create_new=True)
    elif index_payload.is_dir():
        for path in sorted(index_payload.rglob("*")):
            if path.is_file():
                rel = path.relative_to(index_payload)
                copy_file_atomic(path, layout["data_dir"] / rel, create_new=True)
    else:
        raise UpgradeError("index payload not found: %s" % index_payload)
    if official_settings_fixture:
        copy_file_atomic(official_settings_fixture, layout["appdata_dir"] / "findx2-gui-settings.json", create_new=True)
    service = str(layout["install_dir"] / "findx2-service.exe")
    write_json(layout["scm_dir"] / "simulated-scm.json", {
        "name": "FindX2Search",
        "binPath": service,
        "status": "Stopped",
        "startType": "demand",
        "realScmTouched": False,
    })
    write_json(layout["run_dir"] / "simulated-run.json", {
        "key": r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run",
        "valueName": "FindX",
        "command": str(layout["install_dir"] / "FindX.exe"),
        "realRegistryTouched": False,
        "present": True,
    })


def _stage_candidate(stage_dir, candidate_dir, allowed_root):
    stage_dir = assert_allowed(stage_dir, allowed_root)
    if stage_dir.exists():
        raise UpgradeError("stage dir already exists: %s" % stage_dir)
    stage_dir.mkdir(parents=True)
    candidate_dir = _norm(candidate_dir)
    files = []
    names = list(INSTALL_FILES)
    # Also stage any extra hashed payload files from a stage-manifest if present.
    manifest_path = candidate_dir / "stage-manifest.json"
    declared = None
    if manifest_path.exists():
        declared = read_json(manifest_path)
        names = [item["name"] if isinstance(item, dict) and "name" in item else item for item in declared.get("files", [])]
        if "FindX.installed" not in names:
            names.append("FindX.installed")
    for name in names:
        src = candidate_dir / name
        dest = stage_dir / name
        if name == "FindX.installed" and not src.exists():
            dest.write_bytes(MARKER_BYTES)
            files.append({"name": name, "sha256": sha256_file(dest), "length": dest.stat().st_size})
            continue
        if not src.exists():
            raise UpgradeError("candidate missing %s" % name)
        copy_file_atomic(src, dest, create_new=True)
        digest = sha256_file(dest)
        if declared:
            expected = None
            for item in declared.get("files", []):
                if item.get("name") == name:
                    expected = item.get("sha256")
            if expected and expected.lower() != digest:
                raise UpgradeError("staged hash mismatch for %s" % name)
        files.append({"name": name, "sha256": digest, "length": dest.stat().st_size})
    write_json(stage_dir / "stage-manifest.json", {
        "version": "2.5.0",
        "gui": "native",
        "files": files,
        "installation_performed": False,
    })
    return files


def _replace_install_files(layout, stage_dir, fault):
    replaced = []
    for name in INSTALL_FILES:
        src = Path(stage_dir) / name
        if not src.exists():
            if name == "FindX.installed":
                src_bytes = MARKER_BYTES
                dest = layout["install_dir"] / name
                tmp = dest.with_name(".findx-new-%s-%s" % (os.getpid(), name))
                tmp.write_bytes(src_bytes)
                os.replace(tmp, dest)
                replaced.append(name)
                continue
            raise UpgradeError("staged payload missing %s" % name)
        dest = layout["install_dir"] / name
        copy_file_atomic(src, dest)
        replaced.append(name)
        if fault == "mid_replace" and len(replaced) == 1:
            raise UpgradeError("injected fault: mid_replace")
    return replaced


def _update_simulated_scm_run(layout, migrated):
    scm_path = layout["scm_dir"] / "simulated-scm.json"
    scm = read_json(scm_path)
    if scm.get("status") not in ("Stopped", "stopped", 1):
        raise UpgradeError("refusing to continue: simulated SCM is not Stopped")
    if scm.get("realScmTouched"):
        raise UpgradeError("simulated SCM flagged as having touched the real service")
    scm["binPath"] = str(layout["install_dir"] / "findx2-service.exe")
    scm["status"] = "Stopped"
    scm["realScmTouched"] = False
    scm["versionAfter"] = "2.5.0"
    write_json(scm_path, scm)

    run_path = layout["run_dir"] / "simulated-run.json"
    run = read_json(run_path)
    if run.get("realRegistryTouched"):
        raise UpgradeError("simulated Run flagged as having touched the real registry")
    auto = bool((migrated or {}).get("settings", {}).get("autoStartApp", False))
    run["command"] = str(layout["install_dir"] / "FindX.exe")
    run["present"] = auto or run.get("present", True)
    run["realRegistryTouched"] = False
    run["migratedAutoStartApp"] = auto
    write_json(run_path, run)
    return scm, run


def _product_version(path):
    path = Path(path)
    # Tiny stubs used in protocol tests have no PE version resource.
    try:
        import ctypes
        from ctypes import wintypes
        size = ctypes.windll.version.GetFileVersionInfoSizeW(str(path), None)
        if not size:
            return None
        buf = ctypes.create_string_buffer(size)
        if not ctypes.windll.version.GetFileVersionInfoW(str(path), 0, size, buf):
            return None
        u = ctypes.c_void_p()
        ulen = wintypes.UINT()
        if not ctypes.windll.version.VerQueryValueW(buf, r"\StringFileInfo\040904B0\ProductVersion", ctypes.byref(u), ctypes.byref(ulen)):
            # Try translation-agnostic first block via FixedFileInfo
            if ctypes.windll.version.VerQueryValueW(buf, r"\\", ctypes.byref(u), ctypes.byref(ulen)) and u.value:
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
                a = info.dwProductVersionMS >> 16
                b = info.dwProductVersionMS & 0xFFFF
                c = info.dwProductVersionLS >> 16
                return "%s.%s.%s" % (a, b, c)
            return None
        return ctypes.wstring_at(u.value)
    except Exception:
        return None


def health_check(layout, expected_version=None, cli=None, pinyin_query=None, pinyin_expect=None):
    report = {"ok": True, "checks": []}

    def check(name, cond, detail=""):
        report["checks"].append({"name": name, "ok": bool(cond), "detail": detail})
        if not cond:
            report["ok"] = False

    install = Path(layout["install_dir"])
    for name in INSTALL_FILES:
        path = install / name
        check("exists:" + name, path.is_file(), str(path))
    scm = read_json(layout["scm_dir"] / "simulated-scm.json")
    check("scm-stopped", scm.get("status") == "Stopped", json.dumps(scm, ensure_ascii=False))
    check("scm-not-real", scm.get("realScmTouched") is False, "")
    run = read_json(layout["run_dir"] / "simulated-run.json")
    check("run-not-real", run.get("realRegistryTouched") is False, "")
    for name in ("FindX.exe", "findx2-service.exe", "findx2.exe"):
        ver = _product_version(install / name)
        if expected_version and ver is not None:
            check("version:" + name, str(ver).startswith(expected_version), ver or "")
        elif expected_version:
            check("version:" + name + ":skipped-stub", True, "no PE version")
    index = Path(layout["data_dir"]) / "index.bin"
    if not index.exists():
        # v6 directory form
        index = Path(layout["data_dir"])
    if cli and Path(cli).exists() and index.exists():
        import subprocess
        try:
            proc = subprocess.run(
                [str(cli), "status", "-i", str(index)],
                capture_output=True,
                text=True,
                encoding="utf-8",
                errors="replace",
                timeout=30,
                check=False,
            )
            check("cli-status", proc.returncode == 0, (proc.stdout or "")[:1000] + (proc.stderr or "")[:400])
        except Exception as exc:
            check("cli-status", False, str(exc))
        if pinyin_query:
            try:
                proc = subprocess.run(
                    [str(cli), "search", "-i", str(index), pinyin_query, "--pinyin", "--json"],
                    capture_output=True,
                    text=True,
                    encoding="utf-8",
                    errors="replace",
                    timeout=30,
                    check=False,
                )
                out = (proc.stdout or "") + (proc.stderr or "")
                ok = proc.returncode == 0 and (pinyin_expect is None or pinyin_expect in out)
                check("cli-pinyin", ok, out[:1200])
            except Exception as exc:
                check("cli-pinyin", False, str(exc))
    settings = Path(layout["appdata_dir"]) / "native" / "settings.json"
    prefs = Path(layout["appdata_dir"]) / "native" / "prefs.json"
    if settings.exists():
        s = read_json(settings)
        check("native-settings-isolated-pipe", "findx2-native" in str(s.get("pipeName", "")) or str(s.get("pipeName", "")).startswith("findx2-isolated"), s.get("pipeName", ""))
        check("native-settings-not-official-index", forbidden_reason(s.get("indexPath", layout["data_dir"])) is None, s.get("indexPath", ""))
    if prefs.exists():
        p = read_json(prefs)
        check("native-theme-migrated", p.get("theme") in ("light", "dark", "system"), p.get("theme", ""))
    return report


def run_upgrade(layout, candidate_dir, allowed_root, *, fault=None, cli=None, expected_version="2.5.0", pinyin_query=None, pinyin_expect=None):
    allowed_root = _norm(allowed_root)
    for key in ("install_dir", "data_dir", "appdata_dir", "scm_dir", "run_dir", "tx_dir"):
        assert_allowed(layout[key], allowed_root)
    reason = forbidden_reason(candidate_dir)
    # Candidate may live under the checkout target, not official paths.
    if reason:
        raise UpgradeError(reason)
    if not _is_relative_to(candidate_dir, allowed_root) and not _is_relative_to(candidate_dir, Path(allowed_root).parents[0]):
        # Allow candidate staged elsewhere inside the checkout (sibling of the fixture root).
        checkout = Path(allowed_root)
        # Walk up to find 'target' or accept if under findx checkout.
        ok = False
        for parent in [checkout, *checkout.parents]:
            if parent.name.lower() == "findx" or (parent / "Cargo.toml").exists():
                if _is_relative_to(candidate_dir, parent):
                    ok = True
                    break
        if not ok:
            raise UpgradeError("candidate is outside the checkout: %s" % candidate_dir)

    tx_dir = Path(layout["tx_dir"])
    tx_dir.mkdir(parents=True, exist_ok=True)
    journal_path = tx_dir / "journal.json"
    journal = {
        "state": "started",
        "fault": fault,
        "started": time.strftime("%Y-%m-%dT%H:%M:%S%z"),
        "realScmTouched": False,
        "realRegistryTouched": False,
        "officialInstallTouched": False,
    }
    write_json(journal_path, journal)

    try:
        pre = health_check(layout, expected_version=None)
        if not any(c["name"] == "exists:FindX.exe" and c["ok"] for c in pre["checks"]):
            raise UpgradeError("old isolated install incomplete", journal)
        scm = read_json(layout["scm_dir"] / "simulated-scm.json")
        if scm.get("status") != "Stopped":
            raise UpgradeError("simulated SCM must be Stopped before upgrade", journal)

        backup_dir = tx_dir / ("backup-%s" % int(time.time() * 1000))
        backup = write_backup(layout, backup_dir)
        journal.update({"state": "backed_up", "backup_dir": str(backup_dir), "backup_files": len(backup["files"])})
        write_json(journal_path, journal)
        if fault == "after_backup":
            raise UpgradeError("injected fault: after_backup", journal)

        stage_dir = tx_dir / "stage"
        staged = _stage_candidate(stage_dir, candidate_dir, allowed_root)
        journal.update({"state": "staged", "stage_dir": str(stage_dir), "staged_files": len(staged)})
        write_json(journal_path, journal)
        if fault == "after_stage":
            raise UpgradeError("injected fault: after_stage", journal)

        replaced = _replace_install_files(layout, stage_dir, fault)
        journal.update({"state": "replaced", "replaced": replaced})
        write_json(journal_path, journal)
        if fault == "after_replace":
            raise UpgradeError("injected fault: after_replace", journal)

        src_settings = layout["appdata_dir"] / "findx2-gui-settings.json"
        migrated = None
        if src_settings.exists():
            migrated = migrate_official_settings(
                src_settings,
                layout["appdata_dir"] / "native" / "settings.json",
                layout["appdata_dir"] / "native" / "prefs.json",
                isolated_index=str(layout["data_dir"] / "index.bin"),
                isolated_pipe="findx2-isolated-upgrade",
                isolated_service=str(layout["install_dir"] / "findx2-service.exe"),
            )
            journal.update({"state": "prefs_migrated", "unknown_fields": migrated["unknown_fields"]})
            write_json(journal_path, journal)
        if fault == "after_prefs":
            raise UpgradeError("injected fault: after_prefs", journal)

        scm, run = _update_simulated_scm_run(layout, migrated)
        journal.update({"state": "scm_run_updated", "scm": scm, "run": run})
        write_json(journal_path, journal)
        if fault == "after_scm":
            raise UpgradeError("injected fault: after_scm", journal)
        if fault == "health_fail":
            raise UpgradeError("injected fault: health_fail", journal)

        health = health_check(
            layout,
            expected_version=expected_version,
            cli=cli,
            pinyin_query=pinyin_query,
            pinyin_expect=pinyin_expect,
        )
        journal.update({"state": "health_checked", "health": health})
        write_json(journal_path, journal)
        if not health["ok"]:
            raise UpgradeError("health check failed", journal)

        journal.update({"state": "committed", "finished": time.strftime("%Y-%m-%dT%H:%M:%S%z")})
        write_json(journal_path, journal)
        return {
            "ok": True,
            "state": "committed",
            "journal": journal,
            "backup_dir": str(backup_dir),
            "health": health,
            "migrated": migrated,
        }
    except Exception as exc:
        backup_dir = journal.get("backup_dir")
        rollback = None
        if backup_dir:
            rollback = restore_backup(layout, backup_dir)
        journal.update({
            "state": "rolled_back" if rollback and rollback.get("identical") else "rollback_failed",
            "error": str(exc),
            "rollback": rollback,
            "finished": time.strftime("%Y-%m-%dT%H:%M:%S%z"),
        })
        write_json(journal_path, journal)
        return {
            "ok": False,
            "state": journal["state"],
            "error": str(exc),
            "journal": journal,
            "backup_dir": backup_dir,
            "rollback": rollback,
        }


def copy_official_payload_readonly(dest, allowed_root):
    """Read official 2.4.10 binaries into an isolated payload directory. Does not write official paths."""
    dest = assert_allowed(dest, allowed_root)
    dest.mkdir(parents=True, exist_ok=True)
    src_root = OFFICIAL_INSTALL
    if not src_root.is_dir():
        raise UpgradeError("official install not readable at %s" % src_root)
    copied = []
    for name in INSTALL_FILES:
        src = src_root / name
        if not src.exists():
            if name == "FindX.installed":
                (dest / name).write_bytes(MARKER_BYTES)
                copied.append({"name": name, "sha256": sha256_file(dest / name), "source": "synthesized-marker"})
                continue
            raise UpgradeError("official install missing %s (read-only copy aborted)" % name)
        copy_file_atomic(src, dest / name, create_new=not (dest / name).exists())
        copied.append({"name": name, "sha256": sha256_file(dest / name), "length": (dest / name).stat().st_size, "source": str(src)})
    # Never copy engine-* trial subfolders or unins* from the official tree.
    return copied

#!/usr/bin/env python3
"""Full isolated 2.4.10 -> 2.5.0 upgrade/rollback drill. Never mutates the official install."""
from __future__ import print_function

import json
import os
import shutil
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parent
sys.path.insert(0, str(HERE))
import isolated_upgrade_tx as tx

ROOT = REPO / "target" / "t154-stage-c"
ALLOWED = ROOT
CANDIDATE = ROOT / "candidate-stage"
OLD_PAYLOAD = ROOT / "old-payload-2410"
PUBLIC_INDEX = ROOT / "public-index"
SETTINGS = ROOT / "official-format-settings.json"
EVIDENCE = ROOT / "drill-evidence.json"
PINYIN_EXPECT = "\u7eff\u6c34\u9752\u5c71"


def log(msg):
    line = "[%s] %s" % (time.strftime("%Y-%m-%d %H:%M:%S"), msg)
    print(line, flush=True)
    with open(ROOT / "drill.log", "a", encoding="utf-8") as f:
        f.write(line + "\n")


def official_untouched_snapshot():
    snap = {"paths": {}}
    official = Path(r"C:\Program Files\FindX")
    for name in tx.INSTALL_FILES:
        p = official / name
        if p.is_file():
            snap["paths"][str(p)] = {"sha256": tx.sha256_file(p), "length": p.stat().st_size, "mtime": p.stat().st_mtime}
    # Trial subfolder must remain unmodified too.
    trial = official / "engine-ad794de" / "findx2-service.exe"
    if trial.is_file():
        snap["paths"][str(trial)] = {"sha256": tx.sha256_file(trial), "length": trial.stat().st_size, "mtime": trial.stat().st_mtime}
    return snap


def write_settings_fixture():
    SETTINGS.write_text(
        json.dumps(
            {
                "indexPath": r"C:\ProgramData\FindX\index.bin",
                "volume": "C:",
                "pipeName": "findx2",
                "pinyinDefault": True,
                "serviceExePath": r"C:\Program Files\FindX\findx2-service.exe",
                "searchLimit": 1234,
                "autoStartService": True,
                "firstIndexFullMetadata": False,
                "runMode": "service",
                "drives": ["C:"],
                "excludedDirs": [],
                "enableMetadataBackfill": True,
                "enableEverythingIpc": True,
                "saveIntervalSecs": 30,
                "autoStartApp": True,
                "autoCheckUpdate": True,
                "theme": "dark",
                "browserRecent": [r"C:\notes\%s.txt" % PINYIN_EXPECT],
                "unknownOfficialField": "keep-me",
            },
            ensure_ascii=False,
            indent=2,
        ),
        encoding="utf-8",
    )


def run_cli(exe, args, timeout=30):
    proc = subprocess.run([str(exe), *args], capture_output=True, text=True, encoding="utf-8", errors="replace", timeout=timeout, check=False)
    return {
        "cmd": [str(exe), *args],
        "returncode": proc.returncode,
        "stdout": (proc.stdout or "")[:4000],
        "stderr": (proc.stderr or "")[:2000],
    }


def main():
    ROOT.mkdir(parents=True, exist_ok=True)
    before_official = official_untouched_snapshot()
    if not CANDIDATE.joinpath("FindX.exe").is_file():
        raise SystemExit("missing candidate stage: %s" % CANDIDATE)
    if not OLD_PAYLOAD.joinpath("FindX.exe").is_file():
        log("copying official 2.4.10 binaries read-only into isolated payload")
        copied = tx.copy_official_payload_readonly(OLD_PAYLOAD, ALLOWED)
        log("copied %s" % copied)
    if not PUBLIC_INDEX.joinpath("index.bin").is_file():
        raise SystemExit("missing public index fixture: %s" % PUBLIC_INDEX)
    write_settings_fixture()

    # Confirm official 2.4.10 version on the isolated *copy* (not by writing official files).
    old_ver = tx._product_version(OLD_PAYLOAD / "FindX.exe")
    new_ver = tx._product_version(CANDIDATE / "FindX.exe")
    log("old copied ProductVersion=%s new ProductVersion=%s" % (old_ver, new_ver))

    old_cli = OLD_PAYLOAD / "findx2.exe"
    new_cli = CANDIDATE / "findx2.exe"
    pre_old = run_cli(old_cli, ["status", "-i", str(PUBLIC_INDEX / "index.bin")])
    pre_new = run_cli(new_cli, ["status", "-i", str(PUBLIC_INDEX / "index.bin")])
    pre_pinyin = run_cli(new_cli, ["search", "-i", str(PUBLIC_INDEX / "index.bin"), "lvsqs", "--pinyin", "--json"])
    log("preflight old status rc=%s new status rc=%s pinyin rc=%s" % (
        pre_old["returncode"], pre_new["returncode"], pre_pinyin["returncode"]))

    results = {
        "old_version": old_ver,
        "new_version": new_ver,
        "preflight": {"old_status": pre_old, "new_status": pre_new, "pinyin": pre_pinyin},
        "cases": [],
    }
    faults = [
        "after_backup",
        "after_stage",
        "mid_replace",
        "after_replace",
        "after_prefs",
        "after_scm",
        "health_fail",
        None,  # success last, after rollback cases, using its own tree
    ]
    for fault in faults:
        name = "success" if fault is None else "fail-%s" % fault
        layout = tx.default_layout(ROOT / "drill" / name)
        if layout["root"].exists():
            shutil.rmtree(layout["root"])
        tx.prepare_old_tree(layout, OLD_PAYLOAD, PUBLIC_INDEX / "index.bin", SETTINGS, ALLOWED)
        # Old CLI must still read the isolated v5 copy before upgrade.
        before_status = run_cli(layout["install_dir"] / "findx2.exe", ["status", "-i", str(layout["data_dir"] / "index.bin")])
        result = tx.run_upgrade(
            layout,
            CANDIDATE,
            ALLOWED,
            fault=fault,
            cli=str(layout["install_dir"] / "findx2.exe") if fault is None else None,
            expected_version="2.5.0" if fault is None else None,
            pinyin_query="lvsqs" if fault is None else None,
            pinyin_expect=PINYIN_EXPECT if fault is None else None,
        )
        after_cli = None
        after_pinyin = None
        if result.get("ok"):
            after_cli = run_cli(layout["install_dir"] / "findx2.exe", ["--version"])
            after_status = run_cli(layout["install_dir"] / "findx2.exe", ["status", "-i", str(layout["data_dir"] / "index.bin")])
            after_pinyin = run_cli(
                layout["install_dir"] / "findx2.exe",
                ["search", "-i", str(layout["data_dir"] / "index.bin"), "lvsqs", "--pinyin", "--json"],
            )
            result["after_version"] = after_cli
            result["after_status"] = after_status
            result["after_pinyin"] = after_pinyin
            # Optional success-path compact (bounded fixture only) then confirm still readable.
            compact = run_cli(layout["install_dir"] / "findx2.exe", ["compact", "-i", str(layout["data_dir"] / "index.bin")])
            result["success_compact"] = compact
            result["status_after_compact"] = run_cli(
                layout["install_dir"] / "findx2.exe",
                ["status", "-i", str(layout["data_dir"] / "index.bin")],
            )
        else:
            # After rollback, old 2.4.10 CLI (restored bytes) must still read the old complete index.
            after_cli = run_cli(layout["install_dir"] / "findx2.exe", ["status", "-i", str(layout["data_dir"] / "index.bin")])
            result["rolled_back_old_status"] = after_cli
        case = {
            "name": name,
            "ok": bool(result.get("ok")),
            "state": result.get("state"),
            "error": result.get("error"),
            "rollback_identical": (result.get("rollback") or {}).get("identical") if not result.get("ok") else None,
            "before_old_status_rc": before_status["returncode"],
            "result": result,
        }
        results["cases"].append(case)
        log("case %s state=%s ok=%s rollback=%s" % (name, case["state"], case["ok"], case["rollback_identical"]))

    after_official = official_untouched_snapshot()
    official_changed = []
    for path, meta in before_official["paths"].items():
        later = after_official["paths"].get(path)
        if later != meta:
            official_changed.append(path)
    results["official_install_replaced"] = False
    results["official_bytes_unchanged"] = not official_changed
    results["official_changed_paths"] = official_changed
    results["trial_engine_switched"] = False
    results["publish_gates_closed"] = True

    ok_fail = all(
        (not c["ok"]) and c["state"] == "rolled_back" and c["rollback_identical"]
        for c in results["cases"] if c["name"].startswith("fail-")
    )
    ok_success = any(c["name"] == "success" and c["ok"] and c["state"] == "committed" for c in results["cases"])
    results["drill_passed"] = bool(ok_fail and ok_success and results["official_bytes_unchanged"])
    EVIDENCE.write_text(json.dumps(results, indent=2, ensure_ascii=False), encoding="utf-8")
    log("wrote %s drill_passed=%s" % (EVIDENCE, results["drill_passed"]))
    if not results["drill_passed"]:
        raise SystemExit("isolated drill did not fully pass")


if __name__ == "__main__":
    main()

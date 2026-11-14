#!/usr/bin/env python3
"""Isolated packaged-upgrade drill: version detect, isolated setup, transactional 2.4.10->2.5.0."""
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
import detect_install_version as detect
import isolated_upgrade_tx as tx

ROOT = REPO / "target" / "t156-packaging"
ALLOWED = ROOT
SETUP = ROOT / "setup" / "FindX-2.5.0-isolated-fixture-setup.exe"
STAGE = ROOT / "candidate-stage"
EXTRACTED = ROOT / "extracted-from-setup"
FRESH = ROOT / "fresh-install"
OLD_PAYLOAD = REPO / "target" / "t154-stage-c" / "old-payload-2410"
PUBLIC_INDEX = REPO / "target" / "t154-stage-c" / "public-index" / "index.bin"
SETTINGS = ROOT / "official-format-settings.json"
EVIDENCE = ROOT / "drill-evidence.json"
PINYIN_EXPECT = "\u7eff\u6c34\u9752\u5c71"
OFFICIAL = Path(r"C:\Program Files\FindX")


def log(msg):
    ROOT.mkdir(parents=True, exist_ok=True)
    line = "[%s] %s" % (time.strftime("%Y-%m-%d %H:%M:%S"), msg)
    print(line, flush=True)
    with open(ROOT / "drill.log", "a", encoding="utf-8") as f:
        f.write(line + "\n")


def official_snapshot():
    snap = {"paths": {}, "service": None, "pid_39932": False}
    try:
        import ctypes
        import ctypes.wintypes
        # Keep this informational; the drill also shells out to Get-Service.
    except Exception:
        pass
    for name in list(tx.INSTALL_FILES) + ["unins000.exe", "unins000.dat"]:
        p = OFFICIAL / name
        if p.is_file():
            snap["paths"][str(p)] = {"sha256": tx.sha256_file(p), "length": p.stat().st_size, "mtime_ns": p.stat().st_mtime_ns}
    trial = OFFICIAL / "engine-ad794de" / "findx2-service.exe"
    if trial.is_file():
        snap["paths"][str(trial)] = {"sha256": tx.sha256_file(trial), "length": trial.stat().st_size, "mtime_ns": trial.stat().st_mtime_ns}
    return snap


def process_state():
    state = {"findx2search": None, "pid_39932": False, "pid_22120": False}
    try:
        proc = subprocess.run(
            ["powershell", "-NoProfile", "-Command",
             "$s=Get-Service FindX2Search -ErrorAction SilentlyContinue; $s.Status; Write-Host '---'; [bool](Get-Process -Id 39932 -ErrorAction SilentlyContinue); [bool](Get-Process -Id 22120 -ErrorAction SilentlyContinue)"],
            capture_output=True, text=True, encoding="utf-8", errors="replace", timeout=30, check=False,
        )
        lines = [ln.strip() for ln in (proc.stdout or "").splitlines() if ln.strip()]
        state["raw"] = (proc.stdout or "").strip()
        # Fallback simpler queries
    except Exception as exc:
        state["error"] = str(exc)
    # Direct CIM-free checks via tasklist
    try:
        svc = subprocess.run(["sc", "query", "FindX2Search"], capture_output=True, text=True, encoding="utf-8", errors="replace", timeout=15)
        state["findx2search_query"] = svc.stdout
        state["findx2search_stopped"] = "STOPPED" in (svc.stdout or "").upper()
    except Exception as exc:
        state["findx2search_error"] = str(exc)
    def running(pid):
        r = subprocess.run(["powershell", "-NoProfile", "-Command", "if (Get-Process -Id %s -ErrorAction SilentlyContinue) {'yes'} else {'no'}" % pid],
                           capture_output=True, text=True, encoding="utf-8", errors="replace", timeout=15)
        return "yes" in (r.stdout or "")
    state["pid_39932"] = running(39932)
    state["pid_22120"] = running(22120)
    return state


def run_cli(exe, args, timeout=30):
    proc = subprocess.run([str(exe), *args], capture_output=True, text=True, encoding="utf-8", errors="replace", timeout=timeout, check=False)
    return {"cmd": [str(exe), *args], "returncode": proc.returncode, "stdout": (proc.stdout or "")[:4000], "stderr": (proc.stderr or "")[:2000]}


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
                "theme": "dark",
                "browserRecent": [r"C:\notes\%s.txt" % PINYIN_EXPECT],
                "unknownOfficialField": "keep-me",
            },
            ensure_ascii=False,
            indent=2,
        ),
        encoding="utf-8",
    )


def run_setup(dest, extra_args=None, log_name="setup.log"):
    dest = Path(dest)
    dest.mkdir(parents=True, exist_ok=True)
    log_path = ROOT / log_name
    args = [str(SETUP), "/SILENT", "/SUPPRESSMSGBOXES", "/NORESTART", "/DIR=" + str(dest), "/LOG=" + str(log_path)]
    if extra_args:
        args.extend(extra_args)
    log("running setup: %s" % args)
    proc = subprocess.run(args, capture_output=True, text=True, encoding="utf-8", errors="replace", timeout=180, check=False)
    return {
        "cmd": args,
        "returncode": proc.returncode,
        "stdout": (proc.stdout or "")[:2000],
        "stderr": (proc.stderr or "")[:2000],
        "log_path": str(log_path),
        "log_tail": Path(log_path).read_text(encoding="utf-8", errors="replace")[-4000:] if Path(log_path).exists() else "",
    }


def main():
    ROOT.mkdir(parents=True, exist_ok=True)
    if not SETUP.is_file():
        raise SystemExit("missing isolated setup: %s" % SETUP)
    if not STAGE.is_file() and not (STAGE / "FindX.exe").is_file():
        raise SystemExit("missing staged package: %s" % STAGE)
    if not PUBLIC_INDEX.is_file():
        raise SystemExit("missing public index fixture")
    if not (OLD_PAYLOAD / "FindX.exe").is_file():
        log("copying official 2.4.10 binaries read-only into isolated payload")
        OLD_PAYLOAD.parent.mkdir(parents=True, exist_ok=True)
        tx.copy_official_payload_readonly(OLD_PAYLOAD, REPO / "target")
    write_settings_fixture()

    before_official = official_snapshot()
    before_proc = process_state()
    evidence = {
        "started": time.strftime("%Y-%m-%dT%H:%M:%S%z"),
        "package_version": "2.5.0",
        "setup": {"path": str(SETUP), "sha256": tx.sha256_file(SETUP), "length": SETUP.stat().st_size},
        "before_process": before_proc,
        "cases": {},
    }

    evidence["detect_official_readonly"] = detect.detect(OFFICIAL, package_dir=STAGE)
    log("official detect decision=%s action=%s version=%s" % (
        evidence["detect_official_readonly"]["decision"],
        evidence["detect_official_readonly"]["action"],
        evidence["detect_official_readonly"]["installed"]["version"],
    ))

    # Fresh isolated setup install.
    if FRESH.exists():
        # Keep the directory but install into a clean child if leftover files exist.
        for leftover in FRESH.glob("*"):
            if leftover.name.lower().startswith("unins"):
                continue
        # Use a fresh empty dest to avoid Inno seeing leftover files.
        fresh_dest = ROOT / "fresh-install-run"
        if fresh_dest.exists():
            shutil.rmtree(fresh_dest)
        fresh_dest.mkdir(parents=True)
    else:
        fresh_dest = FRESH
        fresh_dest.mkdir(parents=True, exist_ok=True)
    fresh = run_setup(fresh_dest, log_name="fresh-setup.log")
    evidence["fresh_setup"] = fresh
    log("fresh setup rc=%s" % fresh["returncode"])
    if fresh["returncode"] != 0:
        raise SystemExit("isolated setup failed to install into the fixture: %s" % fresh)

    extracted_ok = True
    if EXTRACTED.exists():
        shutil.rmtree(EXTRACTED)
    EXTRACTED.mkdir(parents=True)
    for name in list(tx.INSTALL_FILES) + ["stage-manifest.json"]:
        src = fresh_dest / name
        if src.is_file():
            tx.copy_file_atomic(src, EXTRACTED / name, create_new=True)
        elif name != "stage-manifest.json":
            extracted_ok = False
    pe = {name: detect.read_pe_metadata(EXTRACTED / name) for name in tx.INSTALL_FILES if name.endswith(".exe")}
    evidence["extracted_from_setup"] = {"dir": str(EXTRACTED), "pe": pe, "ok": extracted_ok}
    for name, meta in pe.items():
        if meta.get("product_version") != "2.5.0" or meta.get("file_version") != "2.5.0":
            extracted_ok = False
            evidence["extracted_from_setup"]["ok"] = False
            log("PE mismatch after setup extract: %s %s" % (name, meta))
    if not extracted_ok:
        raise SystemExit("extracted setup payload is incomplete or PE-inconsistent")

    # Version-detect the extracted package vs a 2.4.10 fixture (decision only).
    preview = ROOT / "detect-preview-2410"
    if preview.exists():
        shutil.rmtree(preview)
    tx.prepare_old_tree(tx.default_layout(preview), OLD_PAYLOAD, PUBLIC_INDEX, SETTINGS, ALLOWED)
    # prepare_old_tree wants layout and writes into preview/install. Detect that install dir.
    preview_install = tx.default_layout(preview)["install_dir"]
    evidence["detect_upgrade_preview"] = detect.detect(preview_install, package_dir=EXTRACTED)
    log("preview detect decision=%s action=%s" % (
        evidence["detect_upgrade_preview"]["decision"], evidence["detect_upgrade_preview"]["action"]))
    if evidence["detect_upgrade_preview"]["action"] != "run_upgrade_transaction":
        raise SystemExit("expected upgrade action on 2.4.10 fixture vs packaged 2.5.0")

    # Official /DIR must be refused and must not change official bytes.
    official_refuse = run_setup(OFFICIAL, log_name="official-dir-refused.log")
    evidence["official_dir_refused"] = official_refuse
    log("official /DIR setup rc=%s (expected nonzero)" % official_refuse["returncode"])
    mid_official = official_snapshot()
    official_changed_after_refuse = [p for p, meta in before_official["paths"].items() if mid_official["paths"].get(p) != meta]
    evidence["official_changed_after_refuse"] = official_changed_after_refuse
    if official_refuse["returncode"] == 0 or official_changed_after_refuse:
        raise SystemExit("isolated setup must not install to or mutate official paths")

    # Transactional packaged upgrade + rollback using the extracted setup payload.
    write_settings_fixture()
    faults = ["mid_replace", "after_replace", "health_fail", None]
    cases = []
    for fault in faults:
        name = "success" if fault is None else "fail-%s" % fault
        layout = tx.default_layout(ROOT / "drill" / name)
        if layout["root"].exists():
            shutil.rmtree(layout["root"])
        tx.prepare_old_tree(layout, OLD_PAYLOAD, PUBLIC_INDEX, SETTINGS, ALLOWED)
        detection = detect.detect(layout["install_dir"], package_dir=EXTRACTED)
        if detection["action"] != "run_upgrade_transaction":
            raise SystemExit("detect refused packaged upgrade for %s: %s" % (name, detection))
        before_status = run_cli(layout["install_dir"] / "findx2.exe", ["status", "-i", str(layout["data_dir"] / "index.bin")])
        result = tx.run_upgrade(
            layout,
            EXTRACTED,
            ALLOWED,
            fault=fault,
            cli=str(layout["install_dir"] / "findx2.exe") if fault is None else None,
            expected_version="2.5.0" if fault is None else None,
            pinyin_query="lvsqs" if fault is None else None,
            pinyin_expect=PINYIN_EXPECT if fault is None else None,
        )
        if result.get("ok"):
            result["after_version"] = run_cli(layout["install_dir"] / "findx2.exe", ["--version"])
            result["after_status"] = run_cli(layout["install_dir"] / "findx2.exe", ["status", "-i", str(layout["data_dir"] / "index.bin")])
            result["after_pinyin"] = run_cli(
                layout["install_dir"] / "findx2.exe",
                ["search", "-i", str(layout["data_dir"] / "index.bin"), "lvsqs", "--pinyin", "--json"],
            )
            result["success_compact"] = run_cli(layout["install_dir"] / "findx2.exe", ["compact", "-i", str(layout["data_dir"] / "index.bin")])
            # Confirm PE on the committed tree.
            result["committed_pe"] = {n: detect.read_pe_metadata(layout["install_dir"] / n) for n in ("FindX.exe", "findx2.exe", "fx.exe", "findx2-service.exe")}
        else:
            result["rolled_back_old_status"] = run_cli(layout["install_dir"] / "findx2.exe", ["status", "-i", str(layout["data_dir"] / "index.bin")])
            result["rolled_back_pe"] = detect.read_pe_metadata(layout["install_dir"] / "FindX.exe")
        case = {
            "name": name,
            "ok": bool(result.get("ok")),
            "state": result.get("state"),
            "error": result.get("error"),
            "detect": {"decision": detection["decision"], "action": detection["action"]},
            "rollback_identical": (result.get("rollback") or {}).get("identical") if not result.get("ok") else None,
            "before_old_status_rc": before_status["returncode"],
            "result": result,
        }
        cases.append(case)
        log("case %s state=%s ok=%s rollback=%s" % (name, case["state"], case["ok"], case["rollback_identical"]))
    evidence["cases"] = cases

    after_official = official_snapshot()
    after_proc = process_state()
    official_changed = [p for p, meta in before_official["paths"].items() if after_official["paths"].get(p) != meta]
    evidence["official_install_replaced"] = False
    evidence["official_bytes_unchanged"] = not official_changed
    evidence["official_changed_paths"] = official_changed
    evidence["after_process"] = after_proc
    evidence["trial_engine_switched"] = False
    evidence["publish_gates_closed"] = True
    evidence["publish_ready"] = False
    evidence["auto_update_channel_implemented"] = False

    ok_fail = all((not c["ok"]) and c["state"] == "rolled_back" and c["rollback_identical"] for c in cases if c["name"].startswith("fail-"))
    success = next((c for c in cases if c["name"] == "success"), None)
    ok_success = bool(success and success["ok"] and success["state"] == "committed")
    pe_ok = False
    if ok_success:
        pe_ok = all(
            (success["result"].get("committed_pe") or {}).get(n, {}).get("product_version") == "2.5.0"
            and (success["result"].get("committed_pe") or {}).get(n, {}).get("file_version") == "2.5.0"
            for n in ("FindX.exe", "findx2.exe", "fx.exe", "findx2-service.exe")
        )
        pinyin_ok = success["result"].get("after_pinyin", {}).get("returncode") == 0 and PINYIN_EXPECT in success["result"].get("after_pinyin", {}).get("stdout", "")
    else:
        pinyin_ok = False
    evidence["drill_passed"] = bool(
        ok_fail and ok_success and pe_ok and pinyin_ok
        and evidence["official_bytes_unchanged"]
        and evidence["official_dir_refused"]["returncode"] != 0
        and not evidence["official_changed_after_refuse"]
        and evidence["detect_official_readonly"]["action"] == "refuse"
        and after_proc.get("findx2search_stopped")
        and after_proc.get("pid_39932")
    )
    evidence["finished"] = time.strftime("%Y-%m-%dT%H:%M:%S%z")
    EVIDENCE.write_text(json.dumps(evidence, indent=2, ensure_ascii=False), encoding="utf-8")
    log("wrote %s drill_passed=%s pe_ok=%s pinyin_ok=%s official_unchanged=%s" % (
        EVIDENCE, evidence["drill_passed"], pe_ok, pinyin_ok, evidence["official_bytes_unchanged"]))
    if not evidence["drill_passed"]:
        raise SystemExit("t156 packaged-upgrade drill did not fully pass")


if __name__ == "__main__":
    main()

"""TINY Linux/Unix fixture only: real scan, IPC search, watch changes, SIGTERM save/restart.
Never scans /, touches an installed service, or uses the real user's runtime/data dirs.
"""
import argparse
import json
import os
from pathlib import Path
import socket
import shutil
import subprocess
import time
p=argparse.ArgumentParser();p.add_argument("--service", required=True);p.add_argument("--work", required=True);p.add_argument("--gui-probe")
a=p.parse_args(); work=Path(a.work).resolve();work.mkdir(parents=True,exist_ok=False)
volume=work/"volume";volume.mkdir();home=work/"home";home.mkdir();runtime=work/"runtime";runtime.mkdir(mode=0o700)
(volume/"alpha-t162-fixture.txt").write_text("one")
index=work/"index.bin";endpoint=runtime/"findx2.sock"
env=dict(os.environ,HOME=str(home),XDG_RUNTIME_DIR=str(runtime),TMPDIR=str(work))
command=[str(Path(a.service).resolve()),"--index",str(index),"--volume",str(volume),"--pipe","findx2","--no-backfill","--save-interval-secs","3600"]
log=(work/"service.log").open("w"); proc=None
report={"scope":"tiny fixture; NOT full-disk acceptance", "volume":str(volume),"index":str(index)}
def request(body):
    with socket.socket(socket.AF_UNIX) as s:
        s.settimeout(3);s.connect(str(endpoint));s.sendall(json.dumps(body).encode()+b"\n")
        return json.loads(s.makefile("rb").readline())
def wait_for(fn, description, seconds=20):
    deadline=time.monotonic()+seconds; last=None
    while time.monotonic()<deadline:
        if proc.poll() is not None: raise AssertionError(f"service exited {proc.returncode} during {description}")
        try:
            last=fn()
            if last:return last
        except (OSError,ValueError):pass
        time.sleep(.1)
    raise AssertionError(f"timeout: {description}; last={last}")
def search(query):return request({"type":"search","query":query,"limit":100,"pinyin":False})
def start():
    global proc
    proc=subprocess.Popen(command,env=env,stdout=log,stderr=subprocess.STDOUT)
    wait_for(lambda: request({"type":"status"}).get("type")=="status_result", "IPC ready")
def stop():
    proc.terminate(); code=proc.wait(timeout=15);assert code==0, f"not graceful: {code}"
try:
    start()
    initial=wait_for(lambda: search("alpha-t162-fixture").get("hits"),"initial indexed search")
    report["initial_search_hits"]=len(initial)
    if a.gui_probe:
        prefix=work/"prefix";prefix.mkdir()
        shutil.copy2(a.gui_probe,prefix/"FindX")
        (prefix/"FindX.installed").write_bytes(b"1\r\n")
        probe_env=dict(env,FINDX_TINY_FIXTURE_PROBE="1")
        probe=subprocess.run([str(prefix/"FindX")],env=probe_env,text=True,capture_output=True,timeout=20,check=True)
        report["gui_session_probe"]=json.loads(probe.stdout)

    # This proves watch registration (no timing-only readiness assumption).
    (volume/"watch-ready-t162.txt").write_text("ready")
    wait_for(lambda: search("watch-ready-t162").get("hits"),"real incremental creation")
    (volume/"beta-t162-fixture.txt").write_text("two")
    wait_for(lambda: search("beta-t162-fixture").get("hits"),"incremental create")
    (volume/"beta-t162-fixture.txt").rename(volume/"gamma-t162-fixture.txt")
    wait_for(lambda: search("gamma-t162-fixture").get("hits"),"incremental rename")
    wait_for(lambda: not search("beta-t162-fixture").get("hits"),"old rename name removed")
    (volume/"gamma-t162-fixture.txt").unlink()
    wait_for(lambda: not search("gamma-t162-fixture").get("hits"),"incremental delete")
    (volume/"alpha-t162-fixture.txt").unlink()
    wait_for(lambda: not search("alpha-t162-fixture").get("hits"),"pre-existing file delete")
    report["preexisting_file_delete"]=True
    report["incremental_create_rename_delete"]=True
    (volume/"persist-on-sigterm-t162.txt").write_text("save without waiting 3600s")
    wait_for(lambda: search("persist-on-sigterm-t162").get("hits"),"pre-stop pending mutation")
    report["status"]=request({"type":"status"})
    stop();report["sigterm_exit_code"]=proc.returncode
    assert index.exists();report["saved_bytes"]=index.stat().st_size
    start()
    wait_for(lambda: search("persist-on-sigterm-t162").get("hits"),"saved index restart search")
    stop();report["restart_search"]=True;report["passed"]=True
finally:
    if proc is not None and proc.poll() is None:
        proc.terminate()
        try:proc.wait(timeout=15)
        except subprocess.TimeoutExpired:proc.kill();proc.wait()
    log.close()
    (work/"result.json").write_text(json.dumps(report,indent=2)+"\n")
print(json.dumps(report,indent=2))

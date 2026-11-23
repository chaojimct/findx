"""Exercise real Rust apply/rollback code with a SIMULATED systemd user manager.
No live systemctl/service/index is touched. Actual engine/IPC is tested separately.
"""
import os
from pathlib import Path
import subprocess
import tempfile
with tempfile.TemporaryDirectory(prefix="findx-user-manager-") as tmp:
    root = Path(tmp)
    (root / "index.bin").write_bytes(b"fixture index preserved byte-for-byte")
    (root / "state").write_text("Running")
    helper = root / "systemctl"
    helper.write_text('''#!/usr/bin/env python3
import os, pathlib, shlex, sys
r=pathlib.Path(os.environ["FINDX_TEST_USER_MANAGER"])
a=[s for s in sys.argv[1:] if s!="--user"]
command=a[0]
state=(r/"state").read_text()
config=r/"config/systemd/user/findx2.service.d/90-findx-settings.conf"
if command=="show":
    args=["/opt/FindX/findx2-service", "--index", str(r/"index.bin"), "--pipe", "findx2", "--volume", str(r/"volume")]
    if config.exists():
        lines=[s[len("ExecStart="):] for s in config.read_text().splitlines() if s.startswith("ExecStart=") and s!="ExecStart="]
        args=shlex.split(lines[-1])
    quoted=" ".join('"'+s+'"' for s in args)
    print("Id=findx2.service\\nLoadState=loaded\\nActiveState="+("active" if state=="Running" else "inactive")+"\\nUnitFileState=enabled\\nMainPID="+("4242" if state=="Running" else "0")+"\\nExecStart={ path=/opt/FindX/findx2-service ; argv[]="+quoted+" ; }")
elif command in ("start","stop"):
    marker=r/"fail-next-start"
    if command=="start" and marker.exists():
        marker.unlink(); print("simulated start failure", file=sys.stderr); sys.exit(1)
    (r/"state").write_text("Running" if command=="start" else "Stopped")
elif command!="daemon-reload":
    raise SystemExit("unexpected command "+repr(a))
''')
    helper.chmod(0o755)
    bus = root / "busctl"
    bus.write_text('''#!/usr/bin/env python3
import json, os, pathlib, shlex, subprocess
r=pathlib.Path(os.environ["FINDX_TEST_USER_MANAGER"])
s=subprocess.check_output([str(r/"systemctl"),"--user","show"],text=True)
command=s.split("argv[]=",1)[1].split(" ;",1)[0]
args=shlex.split(command)
print(json.dumps({"type":"a(sasbttttuii)","data":[[[args[0],args,False,0,0,0,0,0,0,0]]]}))
''')
    bus.chmod(0o755)
    env = dict(os.environ, PATH=str(root)+os.pathsep+os.environ["PATH"], FINDX_TEST_USER_MANAGER=str(root), XDG_CONFIG_HOME=str(root/"config"))
    subprocess.run(["cargo", "test", "--locked", "-p", "findx2-gui-native", "--lib", "simulated_user_manager_transaction_roundtrip", "--", "--nocapture", "--test-threads=1"], env=env, check=True)

"""Run dpkg ONLY in a throwaway chroot, preserving fixture user data.
Needs passwordless sudo for chroot; never installs into the real /opt or dpkg DB.
Dependencies are deliberately force-ignored here: runtime linkage is tested on host,
and this test makes no claim to validate a clean distro's dependency resolver.
"""
import argparse
import json
from pathlib import Path
import shutil
import subprocess
p=argparse.ArgumentParser();p.add_argument("--old",required=True);p.add_argument("--new",required=True);p.add_argument("--root",required=True)
a=p.parse_args(); root=Path(a.root).resolve(); root.mkdir(parents=True,exist_ok=False)
# dpkg scripts execute inside this root with just dash/libc. No host session,
# service manager, desktop cache helper or real user data is visible there.
for path in ("bin","lib64","lib/x86_64-linux-gnu","var/lib/dpkg/updates","var/log","home/fixture/.local/share/FindX"):
    (root/path).mkdir(parents=True,exist_ok=True)
for source, target in (("/bin/dash","bin/dash"),("/lib64/ld-linux-x86-64.so.2","lib64/ld-linux-x86-64.so.2"),("/lib/x86_64-linux-gnu/libc.so.6","lib/x86_64-linux-gnu/libc.so.6")):
    shutil.copy2(source,root/target)
(root/"bin/sh").symlink_to("dash");(root/"var/lib/dpkg/status").touch()
data=root/"home/fixture/.local/share/FindX"
expected={"index.bin":b"fixture precious index", "index.bin.exclude.json":b"fixture exclusions", "settings.json":b"fixture preferences"}
for name, value in expected.items():(data/name).write_bytes(value)
log=root.parent/(root.name+".log")
with log.open("w") as f:
    for args in (["--install",str(Path(a.old).resolve())],["--install",str(Path(a.new).resolve())],["--remove","findx"],["--purge","findx"]):
        command=["sudo","-n","dpkg","--root="+str(root),"--log="+str(root/"var/log/dpkg.log"),"--force-depends",*args]
        subprocess.run(command,stdout=f,stderr=subprocess.STDOUT,check=True)
        for name,value in expected.items():assert (data/name).read_bytes()==value
assert not (root/"opt/FindX/FindX").exists()
report={"scope":"throwaway chroot only; dpkg same-version refresh/upgrade, removal and purge", "user_index_config_preserved":True,"dependency_resolution_tested":False,"root":str(root),"log":str(log),"passed":True}
(root.parent/(root.name+"-result.json")).write_text(json.dumps(report,indent=2)+"\n")
print(json.dumps(report,indent=2))

"""Print the strongest required GLIBC version; weak optional symbols are excluded.
Used by Linux packaging; this is an ABI floor, NOT a distro compatibility promise.
"""
import re
import subprocess
import sys
versions = []
for path in sys.argv[1:]:
    result = subprocess.run(["readelf", "--version-info", path], capture_output=True, text=True)
    if result.returncode:
        # Dummy scripts are supported by unit tests; real ELF inspection must work.
        with open(path, "rb") as f:
            if f.read(5) == b"\x7fELF\x02":
                sys.exit(f"Cannot inspect real ELF {path}: {result.stderr}")
        continue
    for line in result.stdout.splitlines():
        match = re.search(r"Name: GLIBC_([0-9.]+)\s+Flags: none", line)
        if match:
            versions.append(tuple(map(int, match.group(1).split("."))))
if versions:
    print(".".join(map(str, max(versions))))

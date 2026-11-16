"""Tests for official Unix packaging helpers. No network, no /opt install."""
from __future__ import annotations
import os, stat, subprocess, sys, tarfile, tempfile, unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
PACK = ROOT / "installer" / "unix" / "pack-linux.sh"
MAC_PACK = ROOT / "installer" / "macos" / "pack-macos.sh"
ASSETS = ROOT / "scripts" / "check_release_assets.py"
INSTALL = ROOT / "installer" / "unix" / "install.sh"

class UnixPackagingTests(unittest.TestCase):
    def test_pack_macos_refuses_non_darwin(self):
        if os.uname().sysname == "Darwin":
            self.skipTest("running on macOS; refusal path is for other OSes")
        r = subprocess.run(["bash", str(MAC_PACK)], capture_output=True, text=True)
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("must run on macOS", r.stderr + r.stdout)
        self.assertIn("refusing to emit a fake .dmg", r.stderr + r.stdout)

    def test_asset_guard_protects_windows_native_setup(self):
        with tempfile.TemporaryDirectory() as tmp:
            d = Path(tmp)
            (d / "FindX-2.5.2-setup.exe").write_bytes(b"x" * 2048)
            (d / "FindX-2.5.2-linux-x64.deb").write_bytes(b"x" * 2048)
            (d / "FindX-2.5.2-linux-x64.tar.gz").write_bytes(b"x" * 2048)
            (d / "FindX-2.5.2-macos-aarch64.dmg").write_bytes(b"x" * 2048)
            r = subprocess.run(
                [sys.executable, str(ASSETS), "--dist", str(d), "--expected-version", "2.5.2"],
                capture_output=True, text=True,
            )
            self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
            # Historical footgun: a differently named Windows installer.
            (d / "FindX_2.5.2_x64-setup.exe").write_bytes(b"x" * 2048)
            r = subprocess.run(
                [sys.executable, str(ASSETS), "--dist", str(d), "--expected-version", "2.5.2"],
                capture_output=True, text=True,
            )
            self.assertNotEqual(r.returncode, 0)
            self.assertIn("non-official Windows installer", r.stdout + r.stderr)
            (d / "FindX_2.5.2_x64-setup.exe").unlink()
            (d / "FindX-2.5.2-isolated-fixture-setup.exe").write_bytes(b"x" * 2048)
            r = subprocess.run(
                [sys.executable, str(ASSETS), "--dist", str(d), "--expected-version", "2.5.2"],
                capture_output=True, text=True,
            )
            self.assertNotEqual(r.returncode, 0)
            self.assertIn("isolated", r.stdout + r.stderr)

    def test_install_script_layout_with_destdir(self):
        with tempfile.TemporaryDirectory() as tmp:
            stage = Path(tmp) / "stage"
            dest = Path(tmp) / "dest"
            stage.mkdir()
            for name in ("FindX", "findx2", "fx", "findx2-service"):
                p = stage / name
                p.write_bytes(b"#!/bin/sh\necho " + name.encode() + b"\n")
                p.chmod(0o755)
            (stage / "FindX.installed").write_bytes(b"1\r\n")
            (stage / "version").write_text("2.5.2\n")
            (stage / "findx.desktop.in").write_text(
                Path(ROOT / "installer/unix/findx.desktop.in").read_text()
            )
            (stage / "findx2.service.in").write_text(
                Path(ROOT / "installer/unix/findx2.service.in").read_text()
            )
            (stage / "icons").mkdir()
            (stage / "icons" / "findx.png").write_bytes(b"\x89PNG\r\n")
            r = subprocess.run(
                ["bash", str(INSTALL), "--prefix", "/opt/FindX", "--stage", str(stage),
                 "--destdir", str(dest), "--no-service"],
                capture_output=True, text=True,
            )
            self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
            exe = dest / "opt" / "FindX" / "FindX"
            self.assertTrue(exe.is_file())
            self.assertTrue(os.access(exe, os.X_OK))
            self.assertEqual((dest / "opt" / "FindX" / "FindX.installed").read_bytes(), b"1\r\n")
            desktop = (dest / "usr" / "share" / "applications" / "findx.desktop").read_text()
            self.assertIn("Exec=/opt/FindX/FindX", desktop)
            self.assertIn("Version=2.5.2", desktop)
            self.assertTrue((dest / "usr" / "bin" / "findx").is_symlink())
            self.assertEqual(os.readlink(dest / "usr" / "bin" / "findx"), "/opt/FindX/FindX")
            # A binary named findx2-gui-native is not what we install.
            self.assertFalse((dest / "opt" / "FindX" / "findx2-gui-native").exists())

    def test_pack_linux_with_dummy_release_dir(self):
        if os.uname().sysname != "Linux":
            self.skipTest("linux pack script only runs on Linux")
        with tempfile.TemporaryDirectory() as tmp:
            rel = Path(tmp) / "rel"
            out = Path(tmp) / "out"
            rel.mkdir(); out.mkdir()
            for name in ("findx2-gui-native", "findx2", "fx", "findx2-service"):
                p = rel / name
                p.write_bytes(b"\x7fELF dummy " + name.encode() + b"\n" + b"\0" * 64)
                p.chmod(0o755)
            r = subprocess.run(
                ["bash", str(PACK), "--skip-build", "--release-dir", str(rel),
                 "--out", str(out), "--version", "2.5.2"],
                capture_output=True, text=True,
            )
            self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
            deb = out / "FindX-2.5.2-linux-x64.deb"
            tar = out / "FindX-2.5.2-linux-x64.tar.gz"
            self.assertTrue(deb.is_file(), r.stdout)
            self.assertTrue(tar.is_file())
            self.assertFalse(any(out.glob("FindX-*-setup.exe")))
            with tarfile.open(tar, "r:gz") as tf:
                names = tf.getnames()
            self.assertIn("FindX-2.5.2-linux-x64/FindX", names)
            self.assertIn("FindX-2.5.2-linux-x64/FindX.installed", names)
            self.assertIn("FindX-2.5.2-linux-x64/install.sh", names)
            listing = subprocess.check_output(["dpkg-deb", "-c", str(deb)], text=True)
            self.assertIn("/opt/FindX/FindX", listing)
            self.assertIn("/usr/share/applications/findx.desktop", listing)
            self.assertIn("/usr/lib/systemd/user/findx2.service", listing)
            control = subprocess.check_output(["dpkg-deb", "-f", str(deb)], text=True)
            self.assertIn("Package: findx", control)
            self.assertIn("Version: 2.5.2", control)
            self.assertIn("native", control.lower())
            self.assertNotIn("Package: findx-tauri", control.lower())

if __name__ == "__main__":
    unittest.main()

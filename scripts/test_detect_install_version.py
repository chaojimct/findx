import json
import os
import shutil
import tempfile
import unittest
from pathlib import Path

import detect_install_version as detect
import isolated_upgrade_tx as tx

REPO = Path(r"C:\Users\chaoj\dev\tools\findx")
OLD_PAYLOAD = REPO / "target/t154-stage-c/old-payload-2410"
PACKAGE = REPO / "target/t156-packaging/candidate-stage"
OFFICIAL = Path(r"C:\Program Files\FindX")


def _copy_tree(src, dest, names=None):
    dest = Path(dest)
    dest.mkdir(parents=True, exist_ok=True)
    for name in names or ["FindX.exe", "FindX.installed", "findx2-service.exe", "findx2.exe", "fx.exe"]:
        src_path = Path(src) / name
        if src_path.exists():
            shutil.copy2(src_path, dest / name)


class DetectInstallVersionTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        if not (OLD_PAYLOAD / "FindX.exe").is_file():
            raise unittest.SkipTest("missing isolated 2.4.10 payload")
        if not (PACKAGE / "FindX.exe").is_file():
            raise unittest.SkipTest("missing 2.5.0 candidate stage")

    def setUp(self):
        self.td = Path(tempfile.mkdtemp(prefix="findx-t156-detect-"))

    def tearDown(self):
        shutil.rmtree(self.td, ignore_errors=True)

    def test_fresh_when_target_missing_or_empty(self):
        missing = detect.detect(self.td / "no-such-dir", package_dir=PACKAGE)
        self.assertEqual(missing["decision"], "fresh")
        self.assertEqual(missing["action"], "install_fresh")
        self.assertTrue(missing["execution_allowed"])
        empty = detect.detect(self.td / "empty", package_dir=PACKAGE)
        (self.td / "empty").mkdir()
        empty = detect.detect(self.td / "empty", package_dir=PACKAGE)
        self.assertEqual(empty["decision"], "fresh")

    def test_upgrade_from_2410_fixture_to_250_package(self):
        dest = self.td / "inst-2410"
        _copy_tree(OLD_PAYLOAD, dest)
        result = detect.detect(dest, package_dir=PACKAGE)
        self.assertEqual(result["decision"], "upgrade")
        self.assertEqual(result["action"], "run_upgrade_transaction")
        self.assertTrue(result["execution_allowed"])
        self.assertEqual(result["installed"]["version"], "2.4.10")
        self.assertEqual(result["package_version"], "2.5.0")
        self.assertTrue(result["installed"]["identity_ok"])
        self.assertEqual(result["installed"]["location"], "isolated_or_other")

    def test_same_version_is_a_no_op(self):
        dest = self.td / "inst-250"
        _copy_tree(PACKAGE, dest)
        result = detect.detect(dest, package_dir=PACKAGE)
        self.assertEqual(result["decision"], "same")
        self.assertEqual(result["action"], "none")
        self.assertFalse(result["execution_allowed"])
        self.assertEqual(result["installed"]["version"], "2.5.0")

    def test_downgrade_is_blocked(self):
        dest = self.td / "inst-250"
        _copy_tree(PACKAGE, dest)
        result = detect.detect(dest, package_dir=OLD_PAYLOAD)
        self.assertEqual(result["decision"], "downgrade_blocked")
        self.assertEqual(result["action"], "refuse")
        self.assertFalse(result["execution_allowed"])

    def test_partial_install_is_refused(self):
        dest = self.td / "partial"
        _copy_tree(OLD_PAYLOAD, dest)
        (dest / "findx2-service.exe").unlink()
        result = detect.detect(dest, package_dir=PACKAGE)
        self.assertEqual(result["decision"], "partial_or_corrupt")
        self.assertEqual(result["action"], "refuse")
        self.assertIn("findx2-service.exe", result["installed"]["missing_files"])

    def test_corrupt_empty_gui_is_refused(self):
        dest = self.td / "corrupt"
        _copy_tree(OLD_PAYLOAD, dest)
        (dest / "FindX.exe").write_bytes(b"")
        result = detect.detect(dest, package_dir=PACKAGE)
        self.assertEqual(result["decision"], "partial_or_corrupt")
        self.assertEqual(result["action"], "refuse")

    def test_identity_mismatch_is_refused(self):
        dest = self.td / "mismatch"
        dest.mkdir()
        # A real PE that is not FindX.
        system = Path(os.environ.get("SystemRoot", r"C:\Windows")) / "System32" / "notepad.exe"
        if not system.is_file():
            self.skipTest("notepad.exe not available")
        shutil.copy2(system, dest / "FindX.exe")
        (dest / "FindX.installed").write_bytes(tx.MARKER_BYTES)
        result = detect.detect(dest, package_dir=PACKAGE)
        self.assertEqual(result["decision"], "identity_mismatch")
        self.assertEqual(result["action"], "refuse")
        self.assertFalse(result["installed"]["identity_ok"])

    def test_official_2410_is_readable_but_not_executable(self):
        if not (OFFICIAL / "FindX.exe").is_file():
            self.skipTest("official install not readable")
        result = detect.detect(OFFICIAL, package_dir=PACKAGE)
        self.assertEqual(result["installed"]["location"], "official")
        self.assertEqual(result["installed"]["version"], "2.4.10")
        self.assertEqual(result["decision"], "upgrade")
        self.assertEqual(result["action"], "refuse")
        self.assertFalse(result["execution_allowed"])
        self.assertIn("protected location", result["reason"])

    def test_package_companions_have_consistent_pe_250(self):
        pkg = detect.package_from_dir(PACKAGE)
        self.assertEqual(pkg["version"], "2.5.0")
        self.assertEqual(pkg["companions_missing_pe"], [])
        for name in detect.EXPECTED_EXES:
            meta = pkg["files"][name]
            self.assertEqual(meta.get("product_version"), "2.5.0", name)
            self.assertEqual(meta.get("file_version"), "2.5.0", name)
            self.assertEqual((meta.get("product_name") or "").lower(), "findx", name)


if __name__ == "__main__":
    unittest.main(verbosity=2)

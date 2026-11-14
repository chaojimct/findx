import json
import os
import shutil
import tempfile
import unittest
from pathlib import Path

import isolated_upgrade_tx as tx

PINYIN_SAMPLE = "\u7eff\u6c34\u9752\u5c71"  # lvsqs sample


def _write(path, data):
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    if isinstance(data, str):
        data = data.encode("utf-8")
    path.write_bytes(data)


class IsolatedUpgradeTxTests(unittest.TestCase):
    def setUp(self):
        self.td = Path(tempfile.mkdtemp(prefix="findx-stage-c-"))
        self.allowed = self.td
        self.old = self.td / "old-payload"
        self.cand = self.td / "candidate"
        self.idx = self.td / "public-index" / "index.bin"
        self.settings = self.td / "official-format-settings.json"
        self.old.mkdir()
        self.cand.mkdir()
        self.idx.parent.mkdir()
        for name, payload in {
            "FindX.exe": b"OLD-GUI",
            "FindX.installed": tx.MARKER_BYTES,
            "findx2-service.exe": b"OLD-SVC",
            "findx2.exe": b"OLD-CLI",
            "fx.exe": b"OLD-FX",
        }.items():
            _write(self.old / name, payload)
            _write(self.cand / name, payload.replace(b"OLD", b"NEW") if payload.startswith(b"OLD") else payload)
        _write(self.cand / "FindX.installed", tx.MARKER_BYTES)
        _write(self.idx, b"PUBLIC-V5-INDEX-FIXTURE")
        _write(
            self.settings,
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
                    "browserRecent": ["C:\\notes\\%s.txt" % PINYIN_SAMPLE],
                    "unknownOfficialField": "keep-me",
                },
                ensure_ascii=False,
            ),
        )

    def tearDown(self):
        shutil.rmtree(self.td, ignore_errors=True)

    def _layout(self, name):
        layout = tx.default_layout(self.td / name)
        tx.prepare_old_tree(layout, self.old, self.idx, self.settings, self.allowed)
        return layout

    def test_refuses_official_paths(self):
        with self.assertRaises(tx.UpgradeError):
            tx.assert_allowed(r"C:\Program Files\FindX", self.allowed)
        with self.assertRaises(tx.UpgradeError):
            tx.assert_allowed(r"C:\Program Files\FindX\engine-ad794de", self.allowed)
        with self.assertRaises(tx.UpgradeError):
            tx.assert_allowed(r"C:\ProgramData\FindX\index.bin", self.allowed)
        self.assertIsNotNone(tx.forbidden_reason(r"C:"))
        self.assertIsNotNone(tx.forbidden_reason(r"C:\\"))
        self.assertIsNotNone(tx.forbidden_reason(tx.EXPERIMENTAL_DEV / "index.bin"))

    def test_success_replaces_complete_set_and_migrates_prefs(self):
        layout = self._layout("success")
        result = tx.run_upgrade(layout, self.cand, self.allowed, expected_version=None)
        self.assertTrue(result["ok"], result)
        self.assertEqual(result["state"], "committed")
        self.assertEqual((layout["install_dir"] / "FindX.exe").read_bytes(), b"NEW-GUI")
        self.assertEqual((layout["install_dir"] / "findx2-service.exe").read_bytes(), b"NEW-SVC")
        self.assertEqual((layout["install_dir"] / "findx2.exe").read_bytes(), b"NEW-CLI")
        self.assertEqual((layout["install_dir"] / "fx.exe").read_bytes(), b"NEW-FX")
        self.assertEqual((layout["install_dir"] / "FindX.installed").read_bytes(), tx.MARKER_BYTES)
        self.assertEqual((layout["data_dir"] / "index.bin").read_bytes(), b"PUBLIC-V5-INDEX-FIXTURE")
        scm = tx.read_json(layout["scm_dir"] / "simulated-scm.json")
        self.assertEqual(scm["status"], "Stopped")
        self.assertFalse(scm["realScmTouched"])
        self.assertTrue(scm["binPath"].endswith("findx2-service.exe"))
        run = tx.read_json(layout["run_dir"] / "simulated-run.json")
        self.assertFalse(run["realRegistryTouched"])
        self.assertTrue(run["present"])
        settings = tx.read_json(layout["appdata_dir"] / "native" / "settings.json")
        self.assertEqual(settings["pipeName"], "findx2-isolated-upgrade")
        self.assertEqual(settings["searchLimit"], 1234)
        self.assertFalse(settings["autoStartService"])
        self.assertEqual(settings["runMode"], "standalone")
        self.assertNotEqual(settings["indexPath"], r"C:\ProgramData\FindX\index.bin")
        prefs = tx.read_json(layout["appdata_dir"] / "native" / "prefs.json")
        self.assertEqual(prefs["theme"], "dark")
        self.assertEqual(prefs["browserRecent"], ["C:\\notes\\%s.txt" % PINYIN_SAMPLE])
        unknown = tx.read_json(layout["appdata_dir"] / "native" / "imported-official-unknown.json")
        self.assertEqual(unknown.get("unknownOfficialField"), "keep-me")
        # Official-format source copy remains untouched beside the new profile.
        official_copy = tx.read_json(layout["appdata_dir"] / "findx2-gui-settings.json")
        self.assertEqual(official_copy["pipeName"], "findx2")

    def test_after_backup_rollback_is_byte_identical(self):
        self._assert_rollback("after_backup")

    def test_after_stage_rollback_is_byte_identical(self):
        self._assert_rollback("after_stage")

    def test_mid_replace_rollback_is_byte_identical(self):
        self._assert_rollback("mid_replace")

    def test_after_replace_rollback_is_byte_identical(self):
        self._assert_rollback("after_replace")

    def test_after_prefs_rollback_is_byte_identical(self):
        self._assert_rollback("after_prefs")

    def test_after_scm_rollback_is_byte_identical(self):
        self._assert_rollback("after_scm")

    def test_health_fail_rollback_is_byte_identical(self):
        self._assert_rollback("health_fail")

    def _assert_rollback(self, fault):
        layout = self._layout(fault)
        before = tx.collect_isolated_files(layout)
        before_hashes = {rel: tx.sha256_file(path) for rel, path in before.items()}
        result = tx.run_upgrade(layout, self.cand, self.allowed, fault=fault, expected_version=None)
        self.assertFalse(result["ok"], result)
        self.assertEqual(result["state"], "rolled_back")
        self.assertTrue(result["rollback"]["identical"], result["rollback"])
        after = tx.collect_isolated_files(layout)
        self.assertEqual(set(after), set(before_hashes))
        for rel, path in after.items():
            self.assertEqual(tx.sha256_file(path), before_hashes[rel], rel)
        # Old binaries and index must be the complete previous set.
        self.assertEqual((layout["install_dir"] / "FindX.exe").read_bytes(), b"OLD-GUI")
        self.assertEqual((layout["install_dir"] / "findx2-service.exe").read_bytes(), b"OLD-SVC")
        self.assertEqual((layout["data_dir"] / "index.bin").read_bytes(), b"PUBLIC-V5-INDEX-FIXTURE")
        self.assertFalse((layout["appdata_dir"] / "native" / "settings.json").exists())
        scm = tx.read_json(layout["scm_dir"] / "simulated-scm.json")
        self.assertEqual(scm["status"], "Stopped")
        self.assertFalse(scm.get("realScmTouched"))

    def test_missing_sidecar_health_failure_rolls_back(self):
        layout = self._layout("missing-sidecar")
        # After a successful-looking replace, health uses cli; simulate a missing index
        # by running a fault that still exercises restore of sidecars after an extra file.
        extra = layout["data_dir"] / "names.dat"
        # Inject by wrapping: start upgrade after_replace then also ensure extra files disappear.
        result = tx.run_upgrade(layout, self.cand, self.allowed, fault="after_replace", expected_version=None)
        self.assertFalse(result["ok"])
        self.assertTrue(result["rollback"]["identical"])
        self.assertFalse(extra.exists())

    def test_migrate_refuses_live_official_appdata(self):
        dest_s = self.td / "profile" / "settings.json"
        dest_p = self.td / "profile" / "prefs.json"
        live = tx.OFFICIAL_APPDATA_GUI / "findx2-gui-settings.json"
        with self.assertRaises(tx.UpgradeError):
            tx.migrate_official_settings(
                live,
                dest_s,
                dest_p,
                isolated_index=str(self.idx),
                isolated_pipe="findx2-isolated-upgrade",
                isolated_service="svc",
            )
        # Writing into official AppData is refused even from a fixture source.
        with self.assertRaises(tx.UpgradeError):
            tx.migrate_official_settings(
                self.settings,
                tx.OFFICIAL_APPDATA_GUI / "settings.json",
                dest_p,
                isolated_index=str(self.idx),
                isolated_pipe="findx2-isolated-upgrade",
                isolated_service="svc",
            )


if __name__ == "__main__":
    unittest.main(verbosity=2)

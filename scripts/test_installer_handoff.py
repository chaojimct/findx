#!/usr/bin/env python3
"""Isolated source contract for the 2.5.3 installer handoff.

This is NOT production Windows acceptance. It does not launch setup, touch
ProgramData, or talk to a running FindX.
"""

import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
PS1 = (ROOT / "installer" / "upgrade-transaction.ps1").read_text(encoding="utf-8")
ISS = (ROOT / "installer" / "FindX.iss").read_text(encoding="utf-8")
RUST = (ROOT / "crates" / "findx2-gui-native" / "src" / "gui_quit.rs").read_text(encoding="utf-8")

CONTRACT = {
    "trayCaption": "退出 FindX（不停止索引服务）",
    "normalQuitMessage": "FindX.Native.NormalQuit.v1",
    "trayMessage": "FindX.Native.Tray.v1",
    "settingsTitle": "FindX 设置",
    "mainTitle": "FindX",
}


class InstallerHandoffContract(unittest.TestCase):
    def test_rust_and_installer_share_the_shipped_quit_names(self):
        for value in CONTRACT.values():
            self.assertIn(value, RUST)
            self.assertIn(value, PS1)
        self.assertNotIn('trayCaption=\'退出\'', PS1.replace(" ", ""))

    def test_quit_search_is_bounded_and_does_not_dump_a_stack(self):
        self.assertNotIn("UIAutomationClient", PS1)
        self.assertNotIn("AutomationElement", PS1)
        self.assertNotIn("run Setup again", PS1)
        self.assertNotIn("Previous interrupted upgrade", PS1)
        self.assertNotIn("Safe upgrade preparation failed", ISS)
        self.assertNotIn("Copy(UTF8Decode(FailureText), 1, 4096)", ISS)
        self.assertIn("TxFailureMessage", ISS)
        self.assertIn("$child.Kill()", PS1)
        self.assertNotIn("taskkill", PS1.lower())
        self.assertNotIn("Stop-Process", PS1)
        self.assertIn("-QuitReport", PS1)
        self.assertIn("/setintegritylevel", PS1)
        self.assertIn("LocalApplicationData", PS1)

    def test_reason_codes_have_short_chinese_sentences(self):
        for code in (
            "gui-quit-unavailable",
            "gui-settings-open",
            "upgrade-lock-held",
            "upgrade-owner-live",
            "upgrade-foreign",
            "upgrade-recover-failed",
            "legacy-save-unconfirmed",
        ):
            self.assertIn(code, PS1)
            self.assertIn(code, ISS)
        self.assertIn("不要删除锁文件", PS1)
        self.assertIn("不要删除锁文件", ISS)
        self.assertIn("recover-and-continue", PS1)

    def test_windows_powershell_51_scripts_keep_a_utf8_bom(self):
        # Windows PowerShell 5.1 decodes a BOM-less UTF-8 script as the
        # system ANSI code page. ACP936 then fails to parse the Chinese text.
        required = (
            "installer/upgrade-transaction.ps1",
            "scripts/test-installer-handoff.ps1",
            "scripts/test-installer-handoff-ci.ps1",
        )
        for rel in required:
            raw = (ROOT / rel).read_bytes()
            self.assertTrue(raw.startswith(b"\xef\xbb\xbf"), rel)
            self.assertNotIn("\ufeff", raw.decode("utf-8-sig"))


if __name__ == "__main__":
    unittest.main()

# FindX 2.5.0 Stage C — isolated upgrade transaction and candidate

Baseline commit for the candidate binaries: `5400360658272eb0fa84b46a205001c52fd0b187` (Stage B).
This document records the isolated-install upgrade transaction, failure/rollback drill, and the 2.5.0 release+pinyin candidate. It does **not** approve publication or an official-install replacement.

## What was implemented

- `scripts/isolated_upgrade_tx.py` performs a checkout-contained transaction over a complete set:
  install exes + `FindX.installed`, all index sidecars present in the fixture, config, simulated SCM (`simulated-scm.json`), simulated HKCU Run (`simulated-run.json`).
- Journal states: `started` → `backed_up` → `staged` → `replaced` → `prefs_migrated` → `scm_run_updated` → `health_checked` → `committed`.
- On any injected or real failure after a backup exists, rollback restores the previous complete set and verifies SHA-256 identity (including removal of files the upgrade added).
- The script refuses official paths (`C:\Program Files\FindX`, `C:\ProgramData\FindX`, `%APPDATA%\tools.findx.gui`), the live experimental dev tree (`%LOCALAPPDATA%\FindX-native-dev`), and volume roots. It never calls the real SCM, never starts FindX2Search, never writes HKCU, and never uses the experimental C library as an official-index stand-in.
- Official 2.4.10 binaries are copied **read-only** into the isolated payload. `engine-*` trial subfolders and `unins*` are not copied.
- Preference migration: known official `findx2-gui-settings.json` fields (theme, search limit, auto-start flag, optional `browserRecent` / favorites if present in that JSON) are imported into a **new** native profile (`native/settings.json` + `native/prefs.json`). Unknown fields are preserved in `imported-official-unknown.json`. The official-format copy is not overwritten. Live official AppData is not read or written. Experimental or official index/pipe/service paths are dropped in favor of isolated values. Isolated `autoStartService` is forced off; simulated SCM stays Stopped.
- `installer/stage-inno.ps1` now also synthesizes `FindX.installed` (`31 0D 0A`) inside the checkout StageDir and includes it in the SHA-256 manifest. Staging still never installs.
- Public two-volume v5 fixture export: `upgrade_candidate::export_stage_c_legacy_fixtures_if_requested` (env `FINDX_EXPORT_LEGACY_FIXTURE`), includes a `\u7eff\u6c34\u9752\u5c71.txt` entry for a pinyin probe.

## What was verified (isolated only)

- Protocol unit tests in `scripts/test_isolated_upgrade_tx.py`: success path plus rollback after `after_backup`, `after_stage`, `mid_replace`, `after_replace`, `after_prefs`, `after_scm`, and `health_fail`. Official path refusal. Live official AppData refusal.
- A later full drill (see the evidence JSON under `target/t154-stage-c/`) uses real 2.4.10 copies, the 2.5.0 candidate, and the public v5 fixture, including CLI `status` and a pinyin search probe. That drill is still not an official-install upgrade, not a real power-loss test, not a controller-cache test, and not a full-volume USN test.

## Candidate and installer

- Windows candidate: native GPUI as `FindX.exe`, pinyin enabled, workspace version 2.5.0, staged inside the checkout with SHA-256 `stage-manifest.json`.
- Inno Setup (`FindX.iss`) refuses to compile unless `/DUpgradeGatesApproved=1`. Those gates are **not** actually met, so a setup.exe is **not** produced. Compiling with the macro would falsely claim approval.

## Remaining publish gates (kept closed)

Do not claim untested real power-loss, controller cache, full-volume USN, or official-install upgrade success.

Still open / incomplete for 2.5.0 publication:

1. Physical power-loss and controller-cache durability of the generational publish protocol.
2. Official-install upgrade transaction (UAC, real SCM, real HKCU Run, Program Files / ProgramData). Isolated success is not official success.
3. Full-volume / USN coverage of the user's official index (the experimental C library must not stand in).
4. Service identity / IPC / single-instance handoff on a real running official GUI.
5. Health-gated installer rollback on the official unins/Inno path. Status still lacks a complete durable-generation / last-persist-error / per-volume acceptance API for the official health gate.
6. WebView localStorage history/layout, tabs/selection/scroll/directory filter, and a real auto-update channel.

## Prefs / startup / single-instance — exact gaps and minimal viable migration

Completed in this isolated task:

- Theme (`light`/`dark`/`system`) and known official settings fields imported into a new native profile from a **fixture copy** of official-format JSON.
- `autoStartApp` is reflected only in `simulated-run.json`. Real HKCU is not written.
- History: `browserRecent` / `browserFavorites` are copied only when they already appear in the official-format JSON schema used here.

Not completed (do not silently invent them):

- Old WebView localStorage theme/search history/layout is **not** parsed or imported. Minimal viable: user exports or re-enters history; or a later explicit export/import bridge. Do not rewrite the WebView private database.
- Tabs, selection, scroll, directory and filter restoration are still not persisted.
- Native single-instance / activation handoff with the old Tauri GUI is **not** implemented. Minimal viable: exit the old GUI first, then start native `FindX.exe`; do not globally `taskkill`. Official GUI PID 22120 was already missing and must not be ended by this work. Taskbar pins depend on AppUserModelID and are not guaranteed.
- Official startup (real HKCU Run) is unchanged. Minimal viable after a later authorized official switch: point the existing `FindX` Run value at the new `FindX.exe` after commit, or remove it if `autoStartApp` is false; never write that during isolated drills.
- Native update card remains a demo.

## Official install and trial engine

The official 2.4.10 directory `C:\Program Files\FindX` was **not** replaced in this run. Official ProgramData, SCM, and HKCU were not written.

A trial-engine switch onto the same experimental index/pipe was **not** performed: a trial `findx2-service.exe` also exists under `C:\Program Files\FindX\engine-ad794de`, which is inside the official install directory, and overwriting that tree is forbidden. PID 39932 was left running; its image path was not writable-confirmed outside the official tree. Canceling or skipping that switch is final for this run.

No push, tag, or public release.

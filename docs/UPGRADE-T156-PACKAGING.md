# FindX 2.5.0 t156 — isolated packaging, version detection, packaged upgrade

This completes the local packaging + version-detection + upgrade-coverage pipeline.
It is **not** an official or public release. `release/2.5.0.json` `publish_ready` stays **false**.
Official UpgradeGatesApproved is **not** used.

Candidate binaries: GUI (`FindX.exe`) is still the Stage C native+pinyin build from
`5400360658272eb0fa84b46a205001c52fd0b187`
(SHA-256 `0a4b7947d8a6d1cb7a36a43f5abb19f61b8200ca290697f4326e8a10f78424b6`).
CLI and service were rebuilt only to embed PE `FileVersion`/`ProductVersion` `2.5.0`.

## How to build the isolated setup

From the checkout (PowerShell):

```
# Stage a candidate that already has consistent PE 2.5.0 on FindX.exe, findx2.exe, fx.exe, findx2-service.exe.
powershell -File installer\stage-inno.ps1 -RepoRoot <checkout> -ReleaseDir <payload-dir> -StageDir <checkout>\target\t156-packaging\candidate-stage

# Pack the isolated-fixture setup. This never passes /DUpgradeGatesApproved.
powershell -File installer\pack-isolated.ps1 -RepoRoot <checkout> -StageDir <checkout>\target\t156-packaging\candidate-stage
```

Output (this run):

- Setup: `target/t156-packaging/setup/FindX-2.5.0-isolated-fixture-setup.exe`
- SHA-256: `6d33ef3f7ae334e05b772b9de654a42949857feb9ccaf345754876438ec57ebb`
- Size: 11,701,531 bytes
- Manifest: `target/t156-packaging/setup/isolated-setup-manifest.json` (`installation_performed=false`, `publish_ready=false`, `official_appid_used=false`)

The packager compiles `installer/FindX-isolated-fixture.iss` only. Official `installer/FindX.iss` remains gated and was not compiled.

Isolated setup safety properties:

- Distinct AppId `{C0F1A2B3-4D5E-6789-ABCD-0123456789EF}` (not the official `{7E8A9B0C-1D2E-3F4A-5B6C-7D8E9F0A1B2C}`)
- `PrivilegesRequired=lowest` (no UAC)
- Default directory is a compile-time checkout path, never `{autopf}\FindX`
- `UsePreviousAppDir=no`
- No SCM / `sc.exe` / service install or start
- No HKCU `Run\FindX` write or delete
- No `taskkill` of `FindX.exe` or `findx2-service.exe`
- `InitializeSetup` aborts if `/DIR` points at official Program Files\FindX, ProgramData\FindX, official GUI AppData, or FindX-native-dev

Install a fresh isolated tree (not official):

```
FindX-2.5.0-isolated-fixture-setup.exe /SILENT /SUPPRESSMSGBOXES /NORESTART /DIR=<checkout-isolated-dir>
```

The packaged **upgrade** of an existing 2.4.10 tree is **not** performed by Inno in-place. The driver runs `scripts/detect_install_version.py`, and only when the action is `run_upgrade_transaction` does it feed the extracted setup payload into `scripts/isolated_upgrade_tx.py` (the Stage C transaction).

## Version-detection matrix

`scripts/detect_install_version.py` (and `scripts/test_detect_install_version.py`):

| Existing tree | Package | decision | action |
|---|---|---|---|
| none / empty | 2.5.0 | `fresh` | `install_fresh` |
| complete 2.4.10 fixture (identity OK) | 2.5.0 | `upgrade` | `run_upgrade_transaction` |
| complete 2.5.0 | 2.5.0 | `same` | `none` |
| complete 2.5.0 | 2.4.10 | `downgrade_blocked` | `refuse` |
| missing companion / empty FindX.exe | 2.5.0 | `partial_or_corrupt` | `refuse` |
| FindX.exe whose ProductName is not FindX | 2.5.0 | `identity_mismatch` | `refuse` |
| official `C:\Program Files\FindX` 2.4.10 (read-only) | 2.5.0 | `upgrade` | `refuse` (protected location) |

Primary identity is `FindX.exe` `ProductName=FindX` plus `ProductVersion`/`FileVersion`. Official 2.4.10 companions have no PE version resource; that known gap is not treated as corruption when `FindX.exe` is a complete 2.4.10 FindX product and the other expected files exist. The 2.5.0 package requires consistent PE `2.5.0` on all four shipped exes.

The installer uses detection only for path safety in this isolated pack. The upgrade transaction is selected by the same Python detector. Official Inno must not be used until official gates actually pass.

## Drill results (this run, Asia/Shanghai)

- Version-detect unit tests: 9 passed; Stage C transaction tests: 11 passed (20 total).
- Isolated setup installed silently to `target/t156-packaging/fresh-install-run` (exit 0). Extracted payload PE versions are 2.5.0 on FindX.exe, findx2.exe, fx.exe, and findx2-service.exe.
- `/DIR=C:\Program Files\FindX` aborted in `InitializeSetup` (exit 1): log line `refusing official Program Files\FindX path`. Official file bytes were unchanged. Privileges were `None` (no UAC).
- Official read-only detect: decision `upgrade`, action `refuse`.
- Packaged upgrade using the **extracted setup payload** and Stage C transaction:
  - `fail-mid_replace`, `fail-after_replace`, `fail-health_fail`: rolled back to a SHA-256-identical previous complete set.
  - `success`: committed 2.5.0 with consistent PE, `lvsqs` pinyin hit, bounded compact.
- Official 2.4.10 was not replaced. FindX2Search stayed Stopped. Trial PID 39932 was left running. No trial binary under `engine-ad794de` was replaced or stopped.
- A leftover HKCU uninstall key may exist for the **isolated** AppId only, pointing at the fixture fresh-install directory. That is not the official product uninstall key and was not written under the official AppId.

## Remaining closed official gates

Do not claim these are met:

1. Physical power-loss and controller-cache durability.
2. Official-install upgrade (UAC, real SCM, real HKCU Run, Program Files / ProgramData). Isolated success is not official success.
3. Full-volume / USN coverage of the official index (the experimental C library must not stand in).
4. Service identity / IPC / single-instance handoff on a real official GUI.
5. Health-gated official Inno rollback (`FindX.iss` still requires `UpgradeGatesApproved`, which is not granted).
6. **Public / in-app auto-update channel.** There is still no public update feed, no signed released installer, and no in-app updater connected to a live channel. This work only implements local detect + isolated packaged upgrade. That channel stays closed and is not faked.

`publish_ready` remains false. No push, tag, or public release.

# FindX 2.5.0 — authorized trial publish (t157)

User authorization (Asia/Shanghai 2026-11-16, t157): the owner asked to publish
so they can detect the update in the installed 2.4.10 GUI and try the in-app
upgrade themselves (「发布一下呗，我自己检测更新，然后更新一下试试」).

This is a **public, non-draft, non-prerelease** GitHub release `v2.5.0` so
`GET https://api.github.com/repos/chaojimct/findx/releases/latest` returns it
for the installed 2.4.10 updater. It is **not** a claim that official publish
gates are met.

## What was published

- Official Windows installer `FindX-2.5.0-setup.exe`, compiled from
  `installer/FindX.iss` (official AppId `{7E8A9B0C-1D2E-3F4A-5B6C-7D8E9F0A1B2C}`,
  product name FindX, default dir `{autopf}\FindX`, `PrivilegesRequired=admin`).
- `/DUpgradeGatesApproved=1` was used **only** so the official script would
  compile for this authorized trial. `release/2.5.0.json` `publish_ready` remains
  **false**.
- Payload is the already-built 2.5.0 native+pinyin candidate:
  `FindX.exe` SHA-256 `0a4b7947d8a6d1cb7a36a43f5abb19f61b8200ca290697f4326e8a10f78424b6`
  plus CLI/service with PE FileVersion/ProductVersion 2.5.0. The default
  `FindX.exe` is **not** the old Tauri/WebView GUI.
- The isolated-fixture setup (`FindX-2.5.0-isolated-fixture-setup.exe`,
  different AppId, `PrivilegesRequired=lowest`, refuses Program Files) was
  **not** published.
- This release is **Windows-setup-only**. No macOS or Linux 2.5.0 official
  artifacts were produced or uploaded.

## What the installed 2.4.10 updater will do

The 2.4.10 GUI (`gui/src-tauri/src/app_update.rs`) will see `releases/latest`
tag `v2.5.0`, pick the first `FindX-*-setup.exe` asset, download it, and
`ShellExecute runas` with `/VERYSILENT /SUPPRESSMSGBOXES /NORESTART` (one UAC).

If the user accepts UAC, official `FindX.iss` behavior is:

1. **Admin / Program Files.** Silent install into the existing official AppId /
   `{autopf}\FindX` tree. This **replaces the official 2.4.10 install**.
2. **taskkill (no health-gated rollback).** `PrepareToInstall` force-kills
   `FindX.exe` and `findx2-service.exe`. That includes any process with those
   image names, so a running official GUI and a trial `findx2-service.exe`
   (including one whose image is named `findx2-service.exe`) can be killed.
   There is **no** health-gated installer rollback in this official script.
3. **Service re-register and start.** The default `full` type includes the
   `servicec` component. After files are copied, the script may `uninstall` /
   `sc stop` / `sc delete` `FindX2Search`, then
   `findx2-service.exe --index "%ProgramData%\FindX\index.bin" install` and
   `sc start FindX2Search`. It writes `FindX.installed`. It uses the official
   ProgramData index path, not the experimental C library.
4. **Optional PATH and shortcuts.** `addpath` and `desktopicon` are
   `checkedonce` (previous task choices may be reused on upgrade).
5. **Auto-launch GUI on silent install.** A `[Run]` entry with `Check: WizardSilent`
   and task `postlaunch` starts `{app}\FindX.exe` after a silent install if that
   task is selected (previous selection may apply).

Physical power-loss safety, official-directory upgrade success, controller cache,
and full-volume / USN coverage were **not** proven in the real environment before
this trial publish. The publisher did **not** run the installer or overwrite
the official 2.4.10 directory.

## Remaining closed gates

- Physical power-loss and controller-cache durability
- Official UAC / SCM / HKCU / Program Files upgrade (this trial is for the user to try)
- Official full-volume / USN coverage
- Official GUI service / IPC / single-instance handoff verification on a real upgrade
- Health-gated official installer rollback (the official iss does not implement it)
- Public in-app auto-update *channel* is now pointed at this GitHub latest release,
  but that is only the existing 2.4.10 GitHub `releases/latest` poller, not a new
  signed update service
- macOS / Linux official 2.5.0 packages (not included)
- `publish_ready` remains false

# t158 — 2.5.1 official launch-experience fix (trial publish)

User report after updating official 2.4.10 to published 2.5.0 via in-app update:
the new app did not auto-start when the update finished; manual launch first
opened a command window; the taskbar name was `FindX Native (live)`.

## Fixes shipped in 2.5.1

1. Official `FindX.exe` is a Windows-subsystem GUI in release builds. Normal
   product launches do not open a console and do not print live/isolated/mock
   diagnostic lines. `--version` / `--help` (and parse errors) attach or
   allocate a console only for those CLI paths.
2. Official installed product (default `FindX.exe` + `FindX.installed`,
   production pipe, not an explicit isolated/dev fixture) uses window title
   `FindX`, AppUserModelID `FindX.FindX`, shortcut display `FindX`, and PE
   ProductName `FindX`. Distinctive `FindX Native (live)` / `FindX Native
   (isolated)` titles remain only for explicit development or fixture launches.
3. Official `installer/FindX.iss` silent / very-silent `[Run]` always starts
   `{app}\FindX.exe` after install (`nowait postinstall skipifnotsilent`),
   independent of leftover `postlaunch` checkedonce task state. Interactive
   installs keep the checkbox (`skipifsilent` + `Tasks: postlaunch`).
4. Native login autostart already honors HKCU Run value name `FindX` and only
   writes the official `Program Files\FindX\FindX.exe` path when the user
   toggles the setting. 2.5.1 does not silently enable autostart for users who
   had it off, and does not rewrite the value on a normal launch.

## Trial-publish rules (same as t157)

- Version unified at 2.5.1 so already-installed 2.5.0 can detect a newer release.
  Replacing the v2.5.0 GitHub asset would not be detected.
- Official `FindX-2.5.1-setup.exe` is packed from official `installer/FindX.iss`
  with `/DUpgradeGatesApproved=1` only to compile. `publish_ready` stays false.
- Do not claim power-loss or official-path upgrade gates are proven.
- Do not pack Tauri/WebView as `FindX.exe`. Do not publish a drill/fixture setup.
- Do not rewrite tag `v2.5.0`. Do not force-push. Do not install onto
  `C:\Program Files\FindX` as a publisher action.

## Isolated verification (not official Program Files)

- Launching the new official `FindX.exe` (with a sibling `FindX.installed`
  marker, outside Program Files) must not open a console, and the window /
  taskbar title must be `FindX`.
- `--version` / `--help` still print to a console.
- A drill-only lowest-privilege packager that copies the official `[Run]` fix,
  with a distinct AppId and no SCM/taskkill, is used for a very-silent upgrade
  from a previous fixture that had `postlaunch` unchecked. That drill setup is
  not published.
- Unit tests cover official vs development titles.

# t159 — 2.5.2 official in-app updater (trial publish)

User report after 2.5.0/2.5.1: Settings → 关于 showed a disabled native-dev placeholder, so official 2.5.0 could not detect 2.5.1.

## Official vs isolated detection

A process is the official product when it is not `--mock` and is not an explicit isolated pipe, and either:

1. The executable is named `FindX.exe` and a sibling `FindX.installed` marker is present (the shipped install layout, including official-mode fixtures used for verification), or
2. The executable is running from the official install directory (`%ProgramFiles%\FindX\FindX.exe` or `%ProgramFiles(x86)%\FindX\FindX.exe`).

`findx2-gui-native`, `--mock`, and `FindX.exe --pipe <isolated>` stay isolated: no check, download, or install, and no fictional versions.

The updater channel is the same as 2.4.10 Tauri `app_update.rs`: `GET https://api.github.com/repos/chaojimct/findx/releases/latest`, semver compare, pick `FindX-*-setup.exe` (isolated/fixture/drill names are refused), download with progress, then `ShellExecute runas` `/VERYSILENT /SUPPRESSMSGBOXES /NORESTART`.

## Trial-publish rules (same as t157/t158)

- Version unified at 2.5.2. Do not rewrite tags `v2.5.0` or `v2.5.1`.
- Official `FindX-2.5.2-setup.exe` is packed from official `installer/FindX.iss` with `/DUpgradeGatesApproved=1` only to compile. `publish_ready` stays false.
- Do not pack Tauri/WebView as FindX.exe. Do not publish a drill/fixture setup.
- Do not install onto `C:\Program Files\FindX` as a publisher action.

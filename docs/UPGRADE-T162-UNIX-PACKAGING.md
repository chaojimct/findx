# t162 — 官方 Linux / macOS 打包、安装与应用内更新

Windows 2.5.2 的原生 GPUI 正式入口已经可用。本页把同一条产品线补到 Linux 和 macOS：官方入口仍是原生 GUI（`findx2-gui-native`，安装后改名为 `FindX`），不再把已退役的 Tauri/WebView 当作正式客户端。

`publish_ready` 保持 `false`。本轮不打新标签、不上传 GitHub Release。本地/CI 可以制包，供人工验收。

## 正式制品

### Linux x86_64（在 Linux 上实际构建）

| 文件 | 角色 |
| --- | --- |
| `FindX-<ver>-linux-x64.deb` | 首选官方安装包（Debian/Ubuntu） |
| `FindX-<ver>-linux-x64.tar.gz` | 其它发行版：自带 `install.sh` / `uninstall.sh` |
| `FindX-<ver>-linux-x64.sha256` | 校验和 |

安装布局：

- `/opt/FindX/FindX`、`findx2`、`fx`、`findx2-service`
- `/opt/FindX/FindX.installed`（与 Windows 相同的 `1\r\n` 标记）
- `/opt/FindX/FindX.pkgformat`（`deb` 或 `tar`，供更新通道挑选资产）
- `/opt/FindX/version`
- `/usr/bin/findx`、`findx2`、`fx` → `/opt/FindX/...`
- `/usr/share/applications/findx.desktop`
- `/usr/share/icons/hicolor/128x128/apps/findx.png`
- `/usr/lib/systemd/user/findx2.service`（**用户**单元，因为 Unix socket 在 `XDG_RUNTIME_DIR`）

默认索引：`~/.local/share/FindX/index.bin`。安装脚本会 `enable` 用户服务，但不会自动 `--now`，避免在安装当下立刻全盘建库。首次可在设置里启动，或 `systemctl --user start findx2.service`。

未提供 AppImage / RPM：AppImage 不好接 systemd 用户服务；RPM 可在后续补。不要把 Linux 二进制改名为 macOS 制品。

本地制包：

```bash
bash installer/unix/pack-linux.sh
# 或复用已有 target/release：
bash installer/unix/pack-linux.sh --skip-build --out dist
```

前缀安装（不写系统 `/opt`，用于夹具）：

```bash
./install.sh --prefix /tmp/FindX-prefix --no-service
```

只要二进制名叫 `FindX` 且旁边有 `FindX.installed`，就会被识别为正式客户端（与 Windows 夹具相同）。

### macOS（脚本已就绪，本轮没有可用的 Mac，故未产出真实 .dmg）

`installer/macos/pack-macos.sh` **必须在 macOS 上运行**。在 Linux/Windows 上会直接退出，并拒绝生成假 dmg。

计划制品：`FindX-<ver>-macos-aarch64.dmg` 或 `FindX-<ver>-macos-x64.dmg`，内含：

- `FindX.app/Contents/MacOS/FindX`（官方入口）及 sidecar、`FindX.installed`
- `Contents/Info.plist`（`CFBundleShortVersionString` = 产品版本）
- `Contents/Resources/AppIcon.icns`、`com.findx.findx2.plist`
- 旁路 `install.sh`：复制到 `/Applications` 并为当前用户安装 LaunchAgent

若本机没有 Developer ID，包保持**未签名、未公证**。这是诚实的现状，不是“已签名官方商店包”。补齐步骤：在已连接的 Mac 上安装 Rust/Xcode CLT，运行 `pack-macos.sh`，如有签名身份会尝试 `codesign`；公证需要付费 Apple 开发者账号，本任务不会自行申请。

## 正式 vs 隔离

进程被认定为正式产品当且仅当：不是 `--mock`、不是显式隔离管道，并且：

1. 可执行文件名为 `FindX.exe`（Windows）或 `FindX`（Unix），且旁边有 `FindX.installed`，或
2. 运行于官方目录：
   - Windows：`%ProgramFiles%\FindX\FindX.exe`（含 x86 / ProgramW6432）
   - Linux：`/opt/FindX/FindX`
   - macOS：`/Applications/FindX.app/Contents/MacOS/FindX`

`findx2-gui-native`、`--mock`、`FindX --pipe <isolated>` 仍不能检查、下载或安装更新，也不会展示虚构版本。

## 应用内更新

通道不变：`GET https://api.github.com/repos/chaojimct/findx/releases/latest`，比较 semver，再按平台挑选官方资产：

- Windows：`FindX-*-setup.exe`（拒绝 isolated/fixture/drill）
- Linux：若 `FindX.pkgformat=deb`（或未写明）优先 `.deb`，否则 `.tar.gz`
- macOS：`FindX-*-macos-aarch64.dmg` 或 `FindX-*-macos-x64.dmg`

Linux 安装走 `pkexec`（没有则 `sudo`）+ `apt-get install` 或解包后的 `install.sh --prefix /opt/FindX`。macOS 走 `osascript` 管理员权限，把 `.app` `ditto` 到 `/Applications`。隔离客户端仍然完全禁用。

若 GitHub 最新发行没有当前平台资产（例如现有 `v2.5.2` 只有 Windows 安装包），正式客户端会显示“暂无本平台安装包”，而不会假装可以下载。

## CI / 发布保护

`.github/workflows/release.yml` 的 Unix job 改为调用上述 pack 脚本，不再 `npm run tauri build`。`publish` 在上传前运行 `scripts/check_release_assets.py`：

- 必须有 `FindX-<ver>-setup.exe`（Windows 原生 Inno 包）
- 允许 Linux `.deb` / `.tar.gz` 与 macOS `.dmg` 及 sha256
- 拒绝第二种 `*setup.exe`、`.msi`、名称里带 tauri/webview/isolated/fixture/drill 的文件
- Unix job 若产出 `FindX-*-setup.exe` 会失败

`scripts/check_release.py --require-ready` 行为不变；在 `publish_ready=false` 时仍会挡住标签发布。不要为了让 CI 变绿而把该字段改成 `true`。

## 尚未完成

- 真实 macOS 构建、安装、GUI 验收（没有已连接的 Mac）
- 签名与公证
- Linux aarch64、RPM、AppImage
- 设置页里对 Unix 服务做压缩/重建/改参数（Start/Stop/开机与 GUI 自启已接 systemd 用户单元 / LaunchAgent）
- 把未发布的 Unix 包打成公开 GitHub Release（需另一次明确授权；建议随 2.5.3 或后续标签一并发布，以便和已发布的 Windows 2.5.2 区分资产）

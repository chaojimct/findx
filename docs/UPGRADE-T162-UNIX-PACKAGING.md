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

默认索引/GUI 设置：`~/.local/share/FindX/`。安装不 enable、不 start 用户服务，不在 root 安装阶段猜测登录用户。首次在设置里确认启动，或 `systemctl --user daemon-reload; systemctl --user start findx2.service`；登录自启需单独确认。

未提供 AppImage / RPM：AppImage 不好接 systemd 用户服务；RPM 可在后续补。不要把 Linux 二进制改名为 macOS 制品。

本地制包：

```bash
bash installer/unix/pack-linux.sh
# 或复用已有 target/release：
bash installer/unix/pack-linux.sh --skip-build --out dist
```

前缀安装（不写系统 `/opt`，用于夹具）：

```bash
./install.sh --destdir /tmp/FindX-fixture-root --no-service
# 所有系统路径均被重定位到夹具根；HOME/XDG_RUNTIME_DIR 也应独立设置。
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
- Linux：`FindX.pkgformat=deb` 只接受 `.deb`，`tar` 只接受 `.tar.gz`，不静默切换包管理权；未知格式优先 `.deb`
- macOS：`FindX-*-macos-aarch64.dmg` 或 `FindX-*-macos-x64.dmg`

Linux 安装使用固定 `/usr/bin/pkexec`，否则 `/usr/bin/sudo -n`（无图形提权器且 sudo 未授权时明确失败，不在无终端 GUI 中等密码），执行 `apt-get install` 或在普通用户私有目录中验证解包后执行官方 `install.sh`。tar 拒绝链接、越界、重复文件及未知布局。更新前停止当前用户已核对的服务，安装后恢复其原运行状态；不会启动原本停止的引擎。

macOS 使用明确挂载点挂载只读 DMG，`ditto` 到独立暂存目录，保留旧 `.app`，再 rename 替换；失败尝试回滚，trap 卸载 DMG。提权段不修改 root 的 LaunchAgent。完成后 GUI 重新打开新应用。隔离客户端完全禁用。下载只接受官方仓库的 HTTPS Release URL，缓存使用独占私有目录。

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
- 当前未推送的后续修复需要授权后推送并重跑 Darwin/Windows CI；没有物理 Mac GUI、签名/公证验收。
- 把未发布的 Unix 包打成公开 GitHub Release（需另一次明确授权；建议随 2.5.3 或后续标签一并发布，以便和已发布的 Windows 2.5.2 区分资产）


## 后续功能修复与验证（2026-11-16，Asia/Shanghai）

- Unix 设置维护复用实际安装版 CLI：停止已核对服务、完整备份 index/v6/排除边车、在副本 compact/rebuild、status 校验、原子提升并保留 rollback，恢复原运行状态。重建缺少明确 `--volume` 或排除格式未知时拒绝猜测，不扩大范围。
- 服务参数只改落盘间隔/回填/Everything 选项，保留索引/pipe/扫描范围/其它参数。Linux 使用当前用户 systemd drop-in；macOS 使用实际 LaunchAgent ProgramArguments，保留其它 plist 字段。读回失败尝试恢复配置与原运行状态。
- 正式 Unix 设置不再落到 `FindX-native-dev`；显式隔离/模拟运行仍使用独立目录，Unix 非正式入口不能操作正式服务。
- Linux 二进制以同目录完整临时文件 rename 替换，避免运行中 GUI 的 ETXTBSY/截断；卸载保留用户索引、排除规则、配置与未知文件。手动 apt/dpkg 删除前应先停止自己的用户服务，包脚本不替 root 猜测/操纵其它用户会话。
- 真实小夹具暴露并修复了 inotify From→To 改名导致墓碑条目不复活的问题；改为 CreatePending upsert，并补齐初始文件 inode 缓存。追赶缓存使用已有的 50k 上限，达到时报告 watch health；不宣称无特权 inotify 等同整盘持久 journal。
- `scripts/test_unix_engine_fixture.py` 验证真实 scan/search/create/rename/delete/重启；`examples/unix_fixture_probe.rs` 使用实际 GUI session/IPC 代码验证标记入口及正式存储路径，但不是渲染窗口的 GUI 验收。
- `scripts/test_unix_service_preferences.py` 使用 **模拟** systemd manager 验证配置写入/失败回滚与索引不变；不是实际 systemd 登录会话验收。
- 构建宿主为 Debian 13 (trixie), glibc 2.41；当前强 GLIBC 符号 ABI 下限由 readelf 自动生成包依赖（当前 2.35）。需要对应 X11/xkbcommon/Wayland/Vulkan 运行库与可用图形驱动。未对 Ubuntu/Fedora/全部 Linux 做运行验收；tar 包不自带运行库。
- 非发布工作流 `unix-validation.yml` 使用 `macos-15` arm64 与 `macos-15-intel` 原生测试/DMG 制包，仅保留 7 天 Actions 验证附件，无 release/tag 步骤。当前需授权推送后方可运行新修复。旧检查点的原生 Darwin 编译已在 CI 通过，但工作区测试因 core persist_tx 的并行环境故障注入竞态失败；CI 改为串行运行此类测试。

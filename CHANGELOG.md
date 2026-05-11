# Changelog

本文档记录 **FindX 2.x**（Rust / Tauri / Windows）面向用户的显著变更。版本号与 GUI 安装包、Git 标签 `v*` 对齐。

## [2.1.1] - 2026-05-07

### 修复

- **元数据回填失败时错误标记已完成**：快速首遍建库后，后台回填若因权限不足全部失败（成功 0 条），不再错误地标记 `metadata_ready=true` 并写盘，避免后续启动跳过回填导致搜索结果永远没有文件大小和修改时间。
- **服务探测 Tokio panic**：`probe_service_pipe_sync` 中 `tokio::runtime::Builder` 未启用 `enable_time()`，导致 `tokio::time::timeout` 触发 panic（`A Tokio 1.x context was found, but timers are disabled`）。
- **回填错误提示**：`NtQueryDirectoryFile` 和 `OpenFileById` 打开卷句柄失败时，新增日志提示"需管理员权限"，便于排查权限不足问题。

## [2.1.0] - 2026-04-23

### 新增

- **更新检测**：启动后（节流）从 GitHub `releases/latest` 对比语义化版本；主界面顶部提示条与**设置 → 高级**中「从 GitHub 检查更新」。
- **快捷键**：主窗口 **Ctrl+F / Cmd+F** 聚焦顶部检索框（避免 WebView 抢占「在页面中查找」）。

### 修复与体验（GUI / Windows）

- **混合 DPI 多显示器**：窗口位置/尺寸记忆改为**逻辑像素**存档，并从 `tauri-plugin-window-state` 中移除物理宽高持久化，避免在 100% 扩展屏与 200% 主屏之间切换后**窗口宽高「减半」**；旧版桌面状态文件一次性迁移。
- **不可见显示器**：恢复布局时若窗口与当前所有监视器几乎无交集，回退为默认居中布局，避免「脱屏启动」。
- **系统预览**：在扩展屏与主屏不同缩放下，Explorer 风格预览宿主与 `IPreviewHandler` 的 DPI 协调（含 WPS 等处理器），修正错位、空白与尺寸异常。
- **托盘**：服务已运行时菜单显示「停止」、退出时结束索引服务；托盘菜单异步刷新，减少右键菜单闪烁。
- **启动与退出**：应用清单默认 `asInvoker` 避免误要求管理员导致闪退；PowerShell / taskkill 使用无窗口启动；退出时停止服务改为非阻塞，减少卡顿。

### 协议与文档

- 仓库根目录补充 **MIT** 全文许可（`LICENSE`），与 `Cargo.toml` 工作区 `MIT OR Apache-2.0` 声明在 README 中说明对应关系。

[2.1.1]: https://github.com/chaojimct/findx/compare/v2.1.0...v2.1.1
[2.1.0]: https://github.com/chaojimct/findx/compare/v2.0.1...v2.1.0
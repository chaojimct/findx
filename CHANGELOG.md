# Changelog

本文档记录 **FindX 2.x**（Rust / Tauri / Windows）面向用户的显著变更。版本号与 GUI 安装包、Git 标签 `v*` 对齐。

## [2.1.2] - 2026-09-02

### 优化

- **拼音查询剪枝（trigram ∪ cjk_names）**：拼音 Auto 模式（GUI 默认）原本对全表跑 lita 拼音正则，现按「字面命中必含 needle 全部 trigram、拼音命中名字必含 CJK 字符」构造候选超集 `∩ᵢ (trigram(nᵢ) ∪ cjk_names)`，候选上仍走 lita 验证链，零漏报。新增运行时 `cjk_names` 位图（加载/建库后并行重建 ~10 ms，USN 增量同步维护）。实测 115 万条目：`weixin`（拼音命中中文名）11.1 ms → 0.45 ms（24.5x）、`jisuanqi` 9.7 ms → 0.25 ms（38.9x）、`startwith:config` 82.2 ms → 4.1 ms（20x）。case-sensitive、正则元字符 needle、候选超全表 1/3 时自动回退全表。
- **`startwith:` / `endwith:` 查询接入 trigram 剪枝**：前缀/后缀 needle 并入剪枝候选源（名字以 X 开头必含 X 的全部 trigram，候选仍是命中超集）。此前这两类查询走逐条 retain 慢路径，实测 115 万条目下 `startwith:kernel32` 72.5 ms → 0.22 ms（323.8x）、`startwith:readme` 75.6 ms → 2.6 ms（29.2x）。
- **mmap 预取（PrefetchVirtualMemory）**：`index.bin` 与 `.tri` 边车挂载后一次整段读入页缓存，消除启动后首查的 ~12k 次逐页 fault 抖动。Win8+ 经 GetProcAddress 动态解析，老系统静默 no-op。
- **GUI 搜索 debounce 250 ms → 60 ms**：搜索延迟进入亚毫秒级后原值过于保守，60 ms 在连续击键间合并中间态，打字跟手感对齐 Everything。
- **子串查询剪枝（trigram 倒排索引）**：参考 plocate 思路，为 case-insensitive 字面查询增加剪枝层——文件名拆三字节组合建倒排位图，查询先对 needle 的全部 trigram 求交得到候选集，再只对候选跑 memmem 验证（必要条件超集，绝不漏报）。实测 115 万条目：低频词 `kernel32` 5.12 ms → 0.34 ms（15.1x）、高频词 `config` 8.91 ms → 3.28 ms（2.7x）；候选超全表 1/3 时自动回退全表扫描。边车 `<index>.tri` mmap 挂载（私有 RSS 近零），USN 增量进 `tri_pending` 位图、超阈值后 service 后台重建热替换。
- **索引加载 mmap 快路径**：`index.bin` 读取改为内存映射 + entries 段对齐校验后整段 memcpy，替代百万级逐字段 syscall 读取；未对齐或布局漂移自动退回逐条解析，v3–v5 全兼容。
- **索引内存：构建期文件名去重（interning）**：重复文件名/目录名在 `names_buf` 中共享同一段字节（node_modules 的 `package.json`/`index.js`、`.git/objects`、winsxs 组件等场景重名率很高），目录名统一 null 终止使文件名与目录名可互相共享；去重哈希表在构建结束后整体释放，运行时零常驻开销，`index.bin` 与常驻 RSS 同步缩小。`index.bin` v5 格式不变，老索引直接兼容。


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

[2.1.2]: https://github.com/chaojimct/findx/compare/v2.1.1...v2.1.2
[2.1.1]: https://github.com/chaojimct/findx/compare/v2.1.0...v2.1.1
[2.1.0]: https://github.com/chaojimct/findx/compare/v2.0.1...v2.1.0
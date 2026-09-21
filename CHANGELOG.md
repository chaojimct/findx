# Changelog

本文档记录 **FindX 2.x**（Rust / Tauri / Windows）面向用户的显著变更。版本号与 GUI 安装包、Git 标签 `v*` 对齐。

## [Unreleased]

## [2.4.1] - 2026-09-21

### 功能

- **`path:` 查询提速约 6.9×（实测 414 万条目库 6553.7 ms → 950.2 ms）**：现代 v6 索引为省内存**刻意不物化**目录全路径（与 Everything 一致），旧实现对每条命中沿 `dir_idx` 父链重建路径再匹配，400 万条目级实测 5.8–6.5 s。现在改为**两段式过滤**：按查询临时建一张「目录下标 → 小写目录路径」表（目录仅约 55 万、路径算一次就够），再用 needle 的「跨分隔符变体」（`s\alice` → `s` / `alice`）在名字与目录上标记直接命中并沿父链传播出候选超集，最后只在候选上精确拼路径校验。新增回归测试 `path_two_phase_filter_matches_naive_full_path_scan` 以朴素全路径扫描为基准，10 组查询（含骑缝跨分隔符、首尾分隔符、目录条目自身等边界）逐用例比对等价；其余 10 项查询无回归。
- **设置页一键「压缩索引」**：此前压缩只能手动停服务、管理员终端跑 `findx2 compact`、再重启服务。现在 GUI 编排全程：自动停服务 → 提权运行 `findx2 compact`（已提权则直接执行；UAC 被取消也会把服务拉回，不会把用户留在「服务已停」状态）→ 自动重启服务 → 展示前后文件体积与回收量。Windows 走 `ShellExecute runas`，macOS / Linux 直接以当前用户运行（数据目录本就可写）。CLI 侧灾难性损失守卫（压缩后存活条目数异常时拒绝落盘）继续生效。
- **设置页「索引体检」**：service 经 IPC `Status` 新增上报 `tombstone_count`（墓碑数，旧版 service 未上报时为空）。设置页显示墓碑占比；条目 ≥ 10 万且占比 > 25% 时给出「建议压缩」警示，与 service 自身体检（`should_compact`）同阈值。

### 修复

- **Unix 卷 `path:` 查询漏报全部命中**：两段式过滤的候选表只含目录相对路径，而 Unix 的 haystack 是
  `root_prefix + / + 目录路径 + / + 名字`——「needle 跨过 root_prefix 与目录路径边界」的命中不被变体
  覆盖，候选集为空导致一无所获（`unix_path_syntax_with_pinyin` 抓到）。现 Unix 卷直接回退全量精确
  校验（保守但正确），并新增等价性回归 `path_unix_volume_matches_naive_full_path_scan` 逐 needle 对齐。
- **Unix/macOS 设置页「重建索引」误报「仅支持 Windows」**：`start_indexing_impl` 的 Unix 分支（遍历建库、日志、自动拉服务）此前已完整实现，但 `rebuild_index` 的非 Windows 分支却直接返回错误未接入。现已接入：停 service（pkill）→ 删 `index.bin` 与 sidecar → 走既有 Unix 建库流程。

### 其他

- 清理编译警告：`win_preview` 两个保留的 WebView2 宿主定位函数标注 `allow(dead_code)`（独立顶级窗口方案后不再调用，留作排查窗口层级）、闭包多余的 `mut`；`desktop` 的 `WindowMode::Quick` 标注保留原因（quick 布局已退役，为旧窗口状态文件反序列化兼容保留）。

## [2.4.0] - 2026-09-21

### 功能

- **索引加载过程可见**：此前 service 加载已存在的 `index.bin`（十万到上亿条目）期间，`Status` 只回报一个静止的 `loading=true`，界面长期显示「索引: 建库中」，与「卡死」无法区分。现在 core 的 `load_index_bin_with_progress` 会在每个阶段回调，service 把它落成进程级快照并经 IPC 透出 `loading_stage` / `loading_elapsed_secs` / `loading_phase_done` / `loading_phase_total`；状态栏显示真实阶段、已耗时与条目数，例如「索引: 加载中 · 解析条目（3/8 阶段，已 42s，共 3.1 亿条）」。字段全部 `#[serde(default)]`，与旧版 service / 旧版 GUI 双向兼容。
- **设置页「随系统启动」开关**：登录时自动启动 FindX 托盘界面，走 `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`，无需管理员、幂等、失败可回报（写后立即回读校验）。真源是注册表，设置文件里的 `auto_start_app` 只是记录。非 Windows 平台明确返回「暂不支持」，不静默失败。

### 修复

- **frns 段计数上限过紧，3 亿条目级索引被判「损坏」而拒绝加载**：`persist.rs` 用一个固定常量 `64 * 1024 * 1024`（6710 万）给 `frns` 段的计数字段设上限，而该字段**设计上恒等于 `entry_count`**。本机实测索引为 309,031,075 条，远超该常量 → 加载直接返回 `Persist("frns 计数异常（过大）")`。叠加当时的加载过程不可观测（见上），界面表现就是「一直卡在建库中」。现改为**相对 `entry_count`** 的阈值（`entry_count × 8 + 1 MiB`），既能挡住乱字节/段错位解出的天文数字，也不会再误杀合法大库。`index_layout_probe` 示例的阈值标注同步改为同一个公式。

- **墓碑只增不减，索引被撑爆（本机实测 98.75% 是墓碑）**：索引重建走「旧卷区间整段打墓碑 + 新条目后挂」的合并策略
  （`findx2-service/src/run.rs` 的 `merge_rebuilt_volume` → `old.delete_entry(i)`），**只标记不物理删除**。
  每次全卷重建都把该卷全部旧条目原地留为墓碑，条目数只增不减。本机 `index.bin` 实测：
  309,031,075 条中 305,160,707 条是墓碑（98.75%），**存活仅 3,870,368 条**，即约 **80 条墓碑撑 1 条有效数据**；
  文件 17.72 GB 中约 **16.5 GB 是废数据**；加载 **55–83 s**；内存下界约 21.9 GB，31 GB 机器上直接撞墙
  （`names_buf` 的 `to_vec()` 一次就要 4.6 GB）；搜索热路径 `fused_scan` 要对 3.09 亿条逐个判 `is_deleted()`。
  本版从**三个层面**一并解决：
  1. **新增物理压缩 `IndexStore::compact_tombstones`**：把墓碑条目从索引里真正删掉，并统一重映射所有
     与之耦合的下标（`entries` / `frns` / `dirs` / `dir_idx` / `dir_index` / `frn_to_entry` / `ext_filter` /
     `dir_path_ranges` / `volumes[].first_entry_idx` / `tri_pending` / `cjk_names`），做完清零 `deleted`。
     实现走「先算保留集、再整体重写」而不是逐个 `remove`，既避免 O(n²)，也不会漏改下标。
  2. **掐断墓碑的产生源**：`merge_rebuilt_volume` 不再对旧卷逐条打墓碑，改为
     `IndexStore::remove_volume_entries(letter)` —— 把该卷区间从 `entries` / `frns` **物理摘除**后
     再走上面同一套重映射。全卷重建从此不再留下永不回收的垃圾。
  3. **可日用入口 + 健康度体检**：
     - `findx2 compact --index <index.bin> [--dry-run]` 一键压缩 + 原子落盘 + 自动重建 trigram 边车；
     - `findx2 status` 增加墓碑数与占比展示；
     - service 在加载完成后做体检，墓碑比超 25%（且条目数 ≥ 10 万）时落 `warn!` 并在状态栏提示
       「索引 N% 条目是墓碑（tomb/n），建议「压缩索引」回收空间」。
  回归测试 `compact_tombstones_keeps_index_consistent_and_search_equivalent` 逐项钉住：平行数组等长、
  `ext_filter` 每桶下标与 `ext_hash` 一致、每条存活条目能解析出路径、`frn_to_entry` 与 `frns` 双向一致、
  搜索等价（存活条目仍命中、墓碑条目消失、父目录被删的条目连带消失）、压缩后存盘→加载往返自洽。

### 说明

- **本开关只管托盘界面**：索引服务 `FindX2Search` 由 SCM 按 `AutoStart` 拉起，与这个开关无关；勾掉它不会停掉索引。
- **压缩需要先停索引服务**：`index.bin` 归 `FindX2Search`（SYSTEM）所有，普通用户身份下无法独占写入。
  压缩前请先停服务（`Stop-Service FindX2Search`，需管理员），压缩完再启动。

### 修正的既有认知

- **排除目录并不缩小索引体积**：`mark_excluded_entries` 同样只打墓碑（`index.rs` 注释已自述「索引体积没省」）。
  因此想靠「把 `C:\Windows\WinSxS` 之类加进排除列表」来瘦身是无效的；必须走重建 + 在 USN 增量层用 sidecar 挡住。
  现在也可以直接用 `findx2 compact` 把这批墓碑物理回收掉。

## [2.3.0] - 2026-09-07

### 功能

- **Unix 建库加速**：Linux 用 `getdents64` + `openat` 并行扫盘，macOS 同样走目录工作队列；默认 fast 首遍不 stat，后台再补大小与时间。扫描根入库后回填能拼出真实路径。
- **Unix 元数据回填**：服务在 Linux / macOS 上按原始大小写拼路径做 `stat`，写入 overlay，不跟搜索抢全局线程池。`--full-stat` / `--no-backfill` 已接到 Unix 服务与 GUI。
- **拼音语法对齐 Windows**：全拼 / 简拼、`;py` `;en` `;np`、`file:` `folder:` `startwith:` `endwith:` `path:` `parent:`、`|` 与 `!` 均可叠拼音。Unix 支持 `/home/foo`、`~/Documents`。CLI 默认编进并开启拼音。

### 优化

- **mmap 预取**：Unix 用 `posix_madvise(WILLNEED)`，与 Windows `PrefetchVirtualMemory` 对齐。
- **Unix trigram 边车**：建库后写 `<index>.tri`，服务启动缺失时补建，拼音剪枝与 Windows 一致。

### 修复

- **设置文案**：不再写仓库里没有的 LaunchAgent / systemd；排除目录示例按平台；macOS 完全磁盘访问、Linux 无特权降级 inotify 会进状态栏。退出应用时 Unix 也会停掉索引进程。

## [2.2.3] - 2026-09-07

### 修复

- **换文件后预览好→坏循环、静置 2–3 秒空白**：32 位 PDF/WPS 预览卸下来后子窗口拆不掉，误判为脏宿主并整窗重建，新宿主先能画、数秒后白屏。改为换文件只 `Unload`、始终复用同一 HWND；列表滚动不再抬 Z 序；系统预览失败时只隐藏、不卸宿主。

## [2.2.2] - 2026-09-04

### 构建

- **macOS 发版编译失败**：`load_findx_settings` 里 `serde_json::from_str` 在非 Windows 上推不出类型（`error[E0282]`）。补上 `FindxGuiSettings` 标注。

### 功能

- **跨平台 / 兜底文档预览**：Windows 仍优先走系统 `IPreviewHandler`；没有预览器、或 macOS / Linux 上的 PDF / Word / Excel / PPTX / OFD，改用内置 `@file-viewer`。老格式 `.ppt` 不走内置引擎，避免水印。图片预览不再限制为仅 Windows。

### 修复

- **换文件后预览一拖/一滚就空白**：换文件时拆掉旧 HWND 再新建，第二个宿主先能画、随后 `SetWindowPos` 就把 prevhost 子窗口打掉，再换又恢复，形成循环。改为复用同一个预览宿主，只更换 `IPreviewHandler`（与资源管理器一致）。拖动主窗口改为 Rust `WindowEvent::Moved` 只挪位置；列表滚动停稳后只抬 Z 序。

## [2.2.1] - 2026-09-04

### 修复

- **macOS 索引一直为 0**：2.2.0 把 `index.bin` 写进 `.app/Contents/MacOS`（装到 /Applications 后只读），且 GUI 找不到 `Contents/Resources/bin` 里的 CLI sidecar。改为 `~/Library/Application Support/FindX`，补齐 sidecar 查找与执行位；建库失败会显示在状态栏。
- **macOS 增量监听误用 `C:`**：服务 clap 默认 `--volume C:`，Data 卷在索引里的前缀又是 `/`，FSEvents 会去听一个不存在的路径并触发反复重建。Unix 把 `C:` 当成未指定，监听落到 `/System/Volumes/Data`。
- **FSEvents 监听可能崩溃**：`CFArrayCreate` 未 retain 路径字符串就 `CFRelease`，随后创建 stream 读悬空指针。改为 `kCFTypeArrayCallBacks`。
- **预览面板滚动后空白**：已打开系统预览时拖动结果列表滚动条，会把未变化的矩形反复交给 `IPreviewHandler::SetRect`，Office / WPS / PDF 等处理器的子窗口会被打成白屏。列表现在视为兄弟滚动并忽略；矩形未变不再 `SetWindowPos` / `SetRect`。

## [2.2.0] - 2026-09-04

### 功能

- **跨平台索引骨架**：`index.bin` 升级为 v6（卷身份 + 根路径前缀，兼容只读加载 v5）。macOS 用 getattrlistbulk + FSEvents，Linux 用目录遍历 + fanotify/inotify；服务在 Unix 上走域套接字，GUI 设置改为挂载点与「增量监听」文案。Windows MFT/USN 行为不变。
- **CI 与跨平台打包**：PR / `main` 在 Windows、macOS、Linux 跑测试并编译 GUI。打 `v*` 标签时除原有 Inno 安装包外，同时产出 macOS dmg 与 Linux deb/AppImage，并附带各平台 CLI tarball。

## [2.1.6] - 2026-09-03

### 修复

- **Everything 兼容窗口在系统服务下不可见**：`FindX2Search` 跑在 Session 0，`FindWindow("EVERYTHING")` 只能看到用户会话。改为在活动用户桌面拉起 `--everything-host`，查询仍走命名管道。

## [2.1.5] - 2026-09-03

### 修复

- **安装器注册服务失败（退出码 2）**：`findx2-service install --index ...` 里 `--index` 写在子命令后面，clap 不认父级参数，安装器重试 15 次全失败。父级参数改为 `global`，安装脚本改为 `--index … install`。

## [2.1.4] - 2026-09-03

### 修复

- **正式安装后 USN「拒绝访问」**：升级时 `uninstall` 后立刻 `install`，旧服务还在「标记删除」就会注册失败且安装器不检查返回值；GUI 再沿用旧设置里的相对 `index.bin`，用普通权限拉起进程去开 `\\.\C:`。安装器改为等待 SCM 释放后重试注册，失败会弹窗；已安装布局把旧相对路径迁到 `ProgramData\FindX`；服务模式只通过 `FindX2Search` 启动，不再直拉无权限进程。

## [2.1.3] - 2026-09-03

### 索引创建与实时更新（HDD / 大批量复制专项）

- **USN 事件合并 + 延迟 stat**：watch 热路径不再同步 `OpenFileById`；同 FRN 批内去重，create 先入库（名字立刻可搜），size/mtime 由后台 worker 补。大批量复制/解压时不再把增量线程卡在随机寻道上。
- **Watch 保活**：`StartUsn < FirstUsn` / Journal ID 变化 / `ERROR_JOURNAL_ENTRY_DELETED` 视为断档，触发单卷重建并指数退避重启；故障文案经 IPC `watch_error` 进 GUI 状态栏。
- **Ensure Journal**：启动时 `FSCTL_CREATE_USN_JOURNAL`，默认 Journal 过小则放大（已更大的用户配置不动）。
- **阻塞式 READ + HDD 自适应**：空闲不再 500ms 轮询；回填按卷判 seek 惩罚（HDD 2 线程、SSD 高并发）；建库时 HDD 卷串行、SSD 仍并行；MFT 枚举缓冲 SSD 1MB / HDD 4MB，回填目录枚举缓冲 1MB；MFT 碎片超阈值只提示、不整理。
- **Journal 将满进状态栏**：剩余不足跨度 5% 时 GUI 提示，停机过久会触发全量重建。
- **ReFS / 无 journal 卷**：建库不再因 journal 探测失败整盘挂掉；该卷跳过增量监听并提示需手动重建。
- **回填断点续跑**：每卷完成后写 `<index>.overlay.bin` 边车，重启后过滤已完成条目再续跑。
- **状态栏回填文案**：不再把「扫描中 / 已关闭 / 权限失败」一律写成「未跑异步回填」；服务上报真实原因，JournalGap 重建后会重新拉起回填。
- **管理员服务管道 ACL**：提升权限后命名管道默认只给 Administrators，普通 GUI 会 `拒绝访问 (5)`；首实例写入本机已登录用户可读写的 DACL。
- **USN「将满」误报**：追上增量时把 `next-cursor`（恒为 0）当成剩余、把 `next-first` 当分母，状态栏会显示「剩余 0 / 分母一直涨」。改为仅在落后且 FirstUsn 逼近游标时预警。
- **IPC 拼音默认开启**：`Search.pinyin` 缺省从 false 改为 true，漏传不再静默关掉拼音。
- **右键改为常用 + 更多**：默认只出打开 / 打开路径 / 复制路径 / 复制文件名 / 删除，点「更多 Windows 操作」再弹完整系统菜单（不再每次 `QueryContextMenu`）。`Shift+右键` 仍直接出系统菜单。系统长菜单转发自绘消息、限制工作区高度，并用 `WH_MSGFILTER` 把滚轮折成 ↑/↓，不必再点顶部/底部箭头。

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

[Unreleased]: https://github.com/chaojimct/findx/compare/v2.4.0...HEAD
[2.4.1]: https://github.com/chaojimct/findx/compare/v2.4.0...v2.4.1
[2.4.0]: https://github.com/chaojimct/findx/compare/v2.3.0...v2.4.0
[2.3.0]: https://github.com/chaojimct/findx/compare/v2.2.3...v2.3.0
[2.2.3]: https://github.com/chaojimct/findx/compare/v2.2.2...v2.2.3
[2.2.2]: https://github.com/chaojimct/findx/compare/v2.2.1...v2.2.2
[2.2.1]: https://github.com/chaojimct/findx/compare/v2.2.0...v2.2.1
[2.2.0]: https://github.com/chaojimct/findx/compare/v2.1.6...v2.2.0
[2.1.6]: https://github.com/chaojimct/findx/compare/v2.1.5...v2.1.6
[2.1.5]: https://github.com/chaojimct/findx/compare/v2.1.4...v2.1.5
[2.1.4]: https://github.com/chaojimct/findx/compare/v2.1.3...v2.1.4
[2.1.3]: https://github.com/chaojimct/findx/compare/v2.1.2...v2.1.3
[2.1.2]: https://github.com/chaojimct/findx/compare/v2.1.1...v2.1.2
[2.1.1]: https://github.com/chaojimct/findx/compare/v2.1.0...v2.1.1
[2.1.0]: https://github.com/chaojimct/findx/compare/v2.0.1...v2.1.0
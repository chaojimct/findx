# findx

本仓库为 **FindX 下一代实现**（由原 `findx2` 代码树迁入 `chaojimct/findx`，作为唯一主线发版仓库）。

Rust 实现的 Windows 本地文件高速索引与搜索：MFT 全量枚举 + USN Journal 增量，**Everything IPC 兼容**，提供 CLI、常驻服务与 Tauri GUI。

> 目标：在 100 万 / 800 万 条目下做到 < 100 ms 的常规搜索延迟，内存占用对齐或超越 Everything。

## 性能与内存

实测于本机 8.5M 条目 / 1.25M 目录单库（D 盘 NTFS，service 默认开启异步 OpenFileById 元数据回填）：

| 指标 | findx2 | Everything 1.5 |
|---|---|---|
| 服务常驻 RSS | **691.9 MB** | ~700 MB |
| `index.bin` 体积（v5 紧凑布局） | **550 MiB** | n/a |
| 索引加载耗时 | 6.4 s | n/a |
| 常用单词搜索延迟（500 hits 截断） | 75 – 89 ms | ~80 ms |
| 全表扫描（429 万 hits、`n` 单字符） | 110 ms | n/a |

近期内存优化路径（1.2 GB → 691 MB）：

1. `FrnIdxMap`：`FxHashMap<u64, u32>` → `sorted: Vec<(u64, u32)> + overlay`，省 ~200 MB。
2. 删 `names_lower_buf`：搜索热路径用栈缓冲即时 ASCII 小写化（`name_lower_into`），省 ~175 MB。
3. `FileEntry` 紧凑化 40 B → 32 B：`mtime`/`ctime` 由 FILETIME u64 改为 unix 秒 u32（覆盖到 2106 年），对外 IPC/SearchHit 仍然返回 FILETIME，零兼容破坏；同时 `index.bin` 升 v5（v4 自动迁移），省 ~125 MB + 64 MiB 盘体积。

## 建索引与元数据回填

CLI / service 默认走 **fast 首遍**：
- `findx2 index -v C:` 仅枚举 MFT 拿到名字 + 父链 + FRN + **USN TimeStamp**（作为 mtime/ctime 近似值，零额外 IO），size 暂留 0；
- 加 `--full-stat` 时立刻走 **NtQueryDirectoryFile 批量快路径**：对每个目录 `OpenFileById(vol, dir_frn)` + `GetFileInformationByHandleEx(FileIdBothDirectoryInfo)`，一次 syscall 拿一批子项的 `(FRN, size, mtime, ctime)`，摊销到单文件 ~几百纳秒。兜底才走 `OpenFileById` 逐文件。

service 启动时若加载到的索引 `metadata_ready=false`，后台线程按**同样**的两阶段跑：
1. `findx2_windows::fetch_dir_meta_batched` — 按卷分组、一卷一个 rayon 池、每目录 1 次 open + K 次 `GetFileInformationByHandleEx`；
2. 未命中条目（reparse / 孤儿）走 `fill_metadata_by_id_pooled` 兜底；
3. 进度写 `metadata_overlay` + 周期 checkpoint，搜索全程零阻塞。

判据是 `FileEntry.size == 0 && !is_dir`（真空文件会多跑一次，代价可忽略）。可用 `FINDX2_DISABLE_BACKFILL=1` 关掉。

> 历史上还实现过"一次顺序读 `\\?\\X:\\$MFT` 建立 FRN→meta 表"（FindX C++ `LoadNtfsMftMetaMap` 同思路），实测在 Win10/11 用户态 100% 被 `ERROR_ACCESS_DENIED(5)` 拒访（即便管理员），已在 commit 中删除。

## 工作区

```
crates/
  findx2-core      # 索引、查询解析、搜索引擎、持久化（平台无关）
  findx2-windows   # MFT、USN、OpenFileById 等 Windows 专属
  findx2-cli       # findx2.exe / fx.exe（建索引、search、watch、remote）
  findx2-ipc       # 服务/客户端共享 DTO（JSON 协议）
  findx2-service   # findx2-service.exe（前台 / SCM 服务、命名管道、Everything IPC）
  findx2-linux     # 占位（验证平台抽象）
  findx2-macos     # 占位
gui/               # Tauri + React，Windows 下通过命名管道与服务通信
```

## 两种运行模式

`findx2-service` 提供索引、USN 监听、命名管道与 Everything IPC；`gui` 是纯客户端。两者组合方式：

### 模式 A：服务模式（推荐）

GUI 以普通用户权限运行，索引服务由 SCM 启动。

```bash
# 1) 一次性建索引（首次需要管理员权限读取 MFT/USN）
findx2 index --output index.bin

# 2) 注册并启动 Windows 服务（管理员）
findx2-service install --index <绝对路径>\index.bin
sc start FindX2Search

# 3) 运行 GUI（普通用户即可）
gui/  → npm run tauri dev   (开发)
       npm run tauri build  (打包)
```

GUI 启动时若管道连不上会引导你"安装服务"。**这种模式下 GUI 自身全程不需要管理员**。

### 模式 B：单体 UAC 模式

GUI + 服务跑在同一个 UAC 提权进程里，不依赖 SCM。适合便携使用、或没有服务安装权限的环境。

GUI 启动时按"以管理员身份运行"启动；首次自动建索引，关闭即结束服务。

模式选择保存在 GUI 设置 `runMode = "service" | "standalone"`，首次启动会让你选。

## CLI 速查

```bash
# 建索引（默认枚举本机全部固定/可移动盘；首次需管理员）
findx2 index --output index.bin
findx2 index --volumes C:,D: --full-stat   # 全量元数据（首遍较慢）

# 本地搜索（不依赖服务）
findx2 search --index index.bin "关键字 ext:txt"
findx2 search --index index.bin "readme" --columns name,path
findx2 search --index index.bin "test" --json

# 状态
findx2 status --index index.bin

# 增量监听（按 checkpoint 续跑）
findx2 watch --index index.bin --volume C: --save-interval-secs 30

# 通过命名管道远程查询服务
findx2 remote "关键字" --index index.bin
```

## 服务命令

```bash
# 前台调试（管理员）
findx2-service --index index.bin

# 注册 Windows 服务（管理员）
findx2-service install --index <绝对路径>\index.bin
findx2-service uninstall
```

服务监听管道 `\\.\pipe\findx2`（可改 `--pipe`），同时注册 Everything IPC 兼容窗口（`EVERYTHING` / `EVERYTHING_TASKBAR_NOTIFICATION`）。

## 查询语法（摘要）

- 顶层 `|` OR（尊重引号内 `|`）；多裸词 AND；token 前 `!` 排除。
- `func:` 值支持 `"双引号"` 包一段。
- **`parent:` + `nosubfolders:`**：父目录精确一层匹配。
- **`size:empty`** 零字节文件；**`dm:` / `dc:`** 含自然周/月（`thisweek`、`lastmonth`、`YYYY-MM` 等）。
- `nopath:` / `nowfn:` / `wildcards:` / `depth:` / `child:` / `empty:` / `dupe:` / `sizedupe:` / `content:`（慢路径读盘）。
- 未知 `xxx:...` 修饰符会**解析失败**（不静默忽略）。

## 可选特性

- **拼音搜索**：`cargo build -p findx2-cli --features pinyin`，运行加 `--pinyin`。
- 拼音 fixture 测试：`cargo test -p findx2-core --features pinyin --test pinyin_files_for_test`
- 拼音耗时基准：`cargo bench -p findx2-core --features pinyin --bench pinyin_perf`

## 索引文件格式

当前 `index.bin` 为 **v5**：FileEntry 32 字节紧凑布局（mtime/ctime u32 unix 秒），目录路径按需解析（不物化到磁盘）。`watch` 会写真实 `volume_serial` / `usn_journal_id` / `last_usn`，从上次游标续跑；Journal 被重建（ID 变化）时会全量重建。

加载兼容：v3 / v4 老索引在 load 时一次性迁移到 v5 内存布局，下次保存写出 v5。

## 许可证

MIT OR Apache-2.0

# FindX 2.0

本仓库为 **FindX 下一代（v2）** 的唯一主线：[Rust](https://www.rust-lang.org/) 实现的 Windows 本地文件高速索引与搜索（MFT 全量枚举 + USN Journal 增量），**Everything IPC 兼容**；提供 **CLI、常驻 Windows 服务、Tauri 图形界面**。

> 设计目标：在百万～千万级路径规模下，常规搜索保持毫秒级响应；内存与磁盘占用可对标或优于同场景下的 Everything 体验。

| 资源 | 链接 |
| --- | --- |
| **发行版（Windows 安装包 / macOS dmg / Linux deb·AppImage）** | [GitHub Releases](https://github.com/chaojimct/findx/releases) |
| **产品/介绍页 (GitHub Pages)** | <https://chaojimct.github.io/findx/> |
| **v1 源码归档 (.NET, 只读对照)** | 分支 [`findx-v1`](https://github.com/chaojimct/findx/tree/findx-v1)（最后一版为 `44d1d38`） |

## 仓库与工作目录说明

- **远程仓库**：`https://github.com/chaojimct/findx`（组织与用户名下仅此一处发版。）
- **日常开发（团队约定）**：本机将仓库克隆在 **`findx2` 目录** 下；路径名仅为习惯，**与 `chaojimct/findx` 远程一一对应**，提交与 CI 以该工作区为准即可。

**首次开启 Pages**：若 [站点](https://chaojimct.github.io/findx/) 未自动更新，请在仓库 **Settings → Pages** 中把 **Source** 选为 **GitHub Actions**（本仓库已含 [`.github/workflows/pages.yml`](.github/workflows/pages.yml)）。

**CI**：push / PR 走 [`.github/workflows/ci.yml`](.github/workflows/ci.yml)，在 Windows / macOS / Linux 上跑工作区测试并编译 GUI。

**自动发版**：对 `v*` 标签（例如 `v2.2.0`）推送会触发 [`.github/workflows/release.yml`](.github/workflows/release.yml)，三端并行打包后汇总到 [Release](https://github.com/chaojimct/findx/releases)：

- **Windows**：`tauri build --no-bundle` + Inno Setup → `FindX-<ver>-setup.exe`
- **macOS**：Tauri `app` + `dmg`（当前 runner 为 Apple Silicon），另附 CLI/服务 tarball
- **Linux**：Tauri `deb` + `AppImage`，另附 CLI/服务 tarball

## Windows 安装包（Inno Setup，与 v1 同宗）

- **流程**：`gui` 下 `npm run tauri build -- --no-bundle`（`beforeBuild` 会跑 `bundle:native-bins`，把 CLI/服务打进 `resources`）→ `npm run inno:stage` 将 `target/release` 同步到 `installer/stage` → 用 `ISCC` 编译 [`installer/FindX.iss`](installer/FindX.iss)（CI 中通过 `choco install innosetup` 与 `iscc /DMyAppVersion=...` 完成）。产出文件名为 **`FindX-<version>-setup.exe`**，发布于仓库根 `dist/`。
- **向导内容**：**中/英**、**开始菜单/桌面**、**是否注册 `FindX2Search` 服务**（默认开）、**是否把安装目录加入系统 PATH**（方便直接运行 `findx2` / `fx`）、**是否安装后启动 FindX**。逻辑在 [`FindX.iss` 的 `[Code]`](installer/FindX.iss)（安装后执行 `findx2-service install`、`sc start`、写 `FindX.installed` 等），风格对齐旧仓库 [`findx-v1:installer/FindX.iss`](https://github.com/chaojimct/findx/blob/findx-v1/installer/FindX.iss)。
- **与 GUI 的约定**：若选择安装服务，安装器会在 `FindX` 同目录写 **`FindX.installed`**，首次无本地设置时 GUI 会采用 **ProgramData 索引 + 服务模式**（见 `findx_settings.rs`）。若**不**选服务，则不写该标记，便于便携/单机模式。
- **本地手搓安装包**（已装 [Inno Setup 6](https://jrsoftware.org/isdl.php) 且已把 `ISCC` 加进 `PATH`）：`cd gui` → `npm run tauri:dir` → `npm run inno:stage` → `cd ../installer` → `iscc /DMyAppVersion=x.y.z FindX.iss`；`Languages\*.isl` 已随仓库放在 `installer/Languages/`，不依赖 Inno 安装目录下的 `compiler:Languages`（与 CI 一致）。版本号与 `tauri.conf.json` / tag 保持一致即可（输出在仓库根 `dist/`）。
- **占位文件**：`gui/src-tauri/bundled/` 下的 sidecar 仍为**占位**，满足 tauri 资源路径校验；正式构建由 `bundle:native-bins` 用本机 release 可执行文件覆盖。Windows 资源在 `tauri.windows.conf.json`，macOS / Linux 分别在对应平台 conf。

## macOS / Linux 打包

- 本机：`cd gui` → `npm ci` → `npm run tauri build`。macOS 产出 dmg，Linux 产出 deb 与 AppImage（需 webkit2gtk 4.1 / gtk3 等系统依赖）。
- CI 打出的 macOS 包为 **Apple Silicon**，**未做 Apple 公证 / 代码签名**。首次打开请右键「打开」，或到「隐私与安全性」里允许。
- sidecar（`findx2` / `fx` / `findx2-service`）打进 `Contents/Resources/bin/`（Linux 为包内 `bin/`）。
- **索引文件**写在用户数据目录：macOS 为 `~/Library/Application Support/FindX/index.bin`，不要放进 `.app` 包内。
- 若要搜到「桌面 / 文稿 / 下载」，到 **系统设置 → 隐私与安全性 → 完全磁盘访问权限** 勾选 FindX。

## 功能概览（v2）

- **索引与增量**：MFT 首建 + USN 续跑，支持多卷、checkpoint 与元数据后台回填（详见下文「建索引与元数据回填」）。
- **多入口**：`findx2` CLI 本地搜 / 建库；`findx2-service` 做命名管道与 Everything 兼容；**FindX**（Tauri）托盘与搜索 UI，可选服务模式或单进程提权模式。
- **查询语法**：顶层 `|` OR、多词 AND、排除 `!`、各类 `func:` 与时间/大小/路径修饰符等（详见下文「查询语法」）。

## 性能与内存

实测于本机 8.5M 条目 / 1.25M 目录单库（D 盘 NTFS，service 默认开启异步 OpenFileById 元数据回填）：

| 指标 | findx2 | Everything 1.5 |
| --- | --- | --- |
| 服务常驻 RSS | **691.9 MB** | ~700 MB |
| `index.bin` 体积（v5 紧凑布局） | **550 MiB** | n/a |
| 索引加载耗时 | 6.4 s | n/a |
| 常用单词搜索延迟（500 hits 截断） | 75 – 89 ms | ~80 ms |
| 全表扫描（429 万 hits、`n` 单字符） | 110 ms | n/a |

近期内存优化路径（1.2 GB → 691 MB）：

1. `FrnIdxMap`：`FxHashMap<u64, u32>` → `sorted: Vec<(u64, u32)> + overlay`，省 ~200 MB。
2. 删 `names_lower_buf`：搜索热路径用栈缓冲即时 ASCII 小写化（`name_lower_into`），省 ~175 MB。
3. `FileEntry` 紧凑化 40 B → 32 B：`mtime`/`ctime` 由 FILETIME u64 改为 unix 秒 u32（覆盖到 2106 年），对外 IPC/SearchHit 仍然返回 FILETIME，零兼容破坏；同时 `index.bin` 升 v5（v4 自动迁移），省 ~125 MB + 64 MiB 盘体积。
4. `names_buf` 构建期名字去重（interning）：重复文件名/目录名共享同一段字节（node_modules、`.git/objects`、winsxs 等场景重名率很高），目录名统一 null 终止使同名文件/目录可互相共享；去重哈希表构建结束即整体释放，**运行时零常驻开销**，`index.bin` 同步缩小且 v5 格式不变。实测本机 371 万条目（`C:\Windows` + 用户目录）names_buf 84.6 MB → 45.2 MB，省 46.6%（`cargo run -p findx2-core --release --example dedup_stat -- <目录>` 可复现）。

## 子串查询剪枝（trigram 倒排）与 mmap 加载

参考 plocate 的 trigram 倒排思路，为 case-insensitive 字面查询增加**剪枝层**：把每个文件名拆成三字节组合（trigram），建 `trigram → 命中条目位图` 的倒排表，查询先对 needle 的全部 trigram 求交得到候选集，再只对候选跑原有 memmem 验证链——trigram 是**必要条件**（名字含 needle 必含其所有 trigram），候选集是真实命中的超集，绝不漏报。

- **边车 `<index>.tri`**：mmap 只读挂载，posting 留在页缓存按需反序列化（私有 RSS 近零），带 64 MB 预算的位图 LRU。
- **增量维护**：USN 改名/新增进 `tri_pending` 位图（查询时并入候选）；超过 `max(条目数/32, 65536)` 后 service 后台重建边车并热替换（构建期间读锁共享，搜索零阻塞）。
- **回退条件**：needle <3 字节、case-sensitive、正则/glob 路径、候选超全表 1/3（高频词直接全表 SIMD 扫描更划算）。拼音路径不走此层，走下方专属剪枝。
- **`startwith:` / `endwith:` 同样剪枝**：它们是 AND 精确过滤，名字以 X 开头必含 X 的全部 trigram，候选仍是命中超集。
- **mmap 预取**：`index.bin` 与边车挂载后经 `PrefetchVirtualMemory`（Win8+，GetProcAddress 动态解析）一次整段读入页缓存，消除首查 ~12k 次逐页 fault 抖动。

实测 115 万条目（80.7 MiB 索引，`cargo run -p findx2-core --release --example tri_bench -- index.bin` 可复现）：

| 查询 | 全表扫描 | trigram 剪枝 | 加速 |
| --- | --- | --- | --- |
| `kernel32`（121 hits） | 5.05 ms | **0.35 ms** | **14.3x** |
| `.gitignore`（313 hits） | 5.51 ms | **0.60 ms** | **9.2x** |
| `template`（1902 hits） | 7.83 ms | **2.56 ms** | 3.1x |
| `config`（9627 hits） | 9.04 ms | **3.15 ms** | 2.9x |
| `startwith:kernel32`（40 hits） | 72.53 ms | **0.22 ms** | **323.8x** |
| `startwith:readme`（6167 hits） | 75.58 ms | **2.59 ms** | **29.2x** |
| `startwith:config`（1241 hits） | 76.44 ms | **3.80 ms** | **20.1x** |

前缀查询原来走逐条 retain 的慢路径（72–76 ms），剪枝后加速最显著。边车构建成本 0.59 s / 49.8 MiB（建库或 service 启动时后台完成一次）；查询越稀疏加速越大，高频词退化为全表扫描路径、无额外开销。

`index.bin` 加载自 v5 起走 **mmap + 整段 memcpy** 快路径（entries 段按 `align_of::<FileEntry>` 对齐校验后一次性拷入，未对齐退回逐条解析），115 万条目加载 63 ms、8.5M 条目约 0.7 s 量级，页缓存热态更快。

## 拼音查询剪枝（trigram ∪ cjk_names）

拼音 Auto 模式（GUI 默认）原本对全表跑 lita 拼音正则（dense DFA ~50 ns/条 + 中文走拼音表），百万条目量级 10–80 ms。剪枝思路：lita 命中一个名字**必居其一**——

1. **字面命中**：名字含 needle 的 ASCII（不区分大小写）字节序列 → trigram 倒排可检出；
2. **拼音命中**：名字至少含一个拼音字符（UTF-8 字节 ≥ 0xE2，码点 ≥ U+2000 量级）→ `cjk_names` 位图可检出。

故候选 = `∩ᵢ (trigram(nᵢ) ∪ cjk_names)`（多 needle AND）∪ `tri_pending`，恒为命中超集；候选上仍走 lita 正则验证链，零漏报。纯英文索引上 `cjk_names` 为空、剪枝退化为纯 trigram；中文名占比越高剪枝越保守（候选超全表 1/3 自动回退全表）。

- **`cjk_names` 位图**：运行时字段，加载/建库后并行扫一遍名字重建（115 万条目 ~10 ms），USN 增量由 `note_name_change` 与 `tri_pending` 同步维护。
- **守卫**：case-sensitive、含正则元字符的 needle（`.` `*` 等会改变字面语义）、全部 needle <3 字节时放弃剪枝回退全表。
- **一致性回归**：`pinyin_prune_consistency_with_trigram_sidecar`（fixture + 600 噪声条目，边车前后逐条比对）。

实测 115 万条目（同一份 `tri_bench`，`--features pinyin`）：

| 查询 | 拼音 Auto 全表 | 剪枝后 | 加速 |
| --- | --- | --- | --- |
| `config`（1000 hits） | 16.76 ms | **3.74 ms** | 4.5x |
| `weixin`（108 hits，拼音命中中文名） | 11.09 ms | **0.45 ms** | **24.5x** |
| `jisuanqi`（0 hits，无中文目标） | 9.73 ms | **0.25 ms** | **38.9x** |
| `startwith:config`（1000 hits） | 82.20 ms | **4.10 ms** | **20.0x** |
| `startwith:jisuanqi`（0 hits） | 76.88 ms | **0.00 ms** | >10⁴x |

## `path:` 查询两段式过滤

现代 v6 索引为省内存**刻意不物化**目录全路径（与 Everything 一致，`dir_paths_buf` 仅 v2–v4 旧格式
回填时存在）。旧实现做 `path:` 查询时对**每条命中**沿 `dir_idx` 父链重建小写全路径再子串匹配，
带堆分配的父链回溯乘上命中数就是灾难：414 万条目库实测 **6.5 s**。

两段式的关键观察：目录只有 ~55 万，而条目有 ~414 万——**同一目录下所有条目的目录路径完全相同**，
路径算一次就够：

1. **候选超集**：按查询临时建一张「目录下标 → 小写目录路径」表（单遍正向扫描，复用 `parent_idx < i`
   拓扑序）。needle 若出现在 `盘符:目录\名字` 里，要么完整落在某组件内部，要么跨过分隔符——
   被跨过的每个分隔符位置把左右两段（`s\alice` → `s`、`alice`）加入**变体表**。名字（大小写无关）
   或目录路径含任一变体的目录直接命中，命中性沿父链向下传播（带记忆，O(目录数)），得到候选集。
2. **精确校验**：只在候选条目上拼全路径做 memmem，结果与朴素全路径扫描**逐条等价**。

2.4.2 起两段式升级为**精确 / 疑似两级候选**：

- **两级分界**：完整落在组件内的命中可精确判定（名字 memmem 完整 needle；祖先目录路径含完整
  needle 则其子树整条全路径含之），只有跨分隔符的变体近似才需要拼路径校验。候选因此分成
  `exact`（直接放行，免拼路径）与 `check`（定向校验）两个位图——**needle 不含分隔符时 `check`
  恒空，第二段校验整体消失**（`path:users` 这类高频形态直接吃满）。
- **trigram 名字剪枝**：全部变体 ≥ 3 字节时先查既有 trigram 倒排取位图并集超集，条目侧只对位图内
  条目做小写化 + memmem；边车缺失 / 候选过密（> 全表 1/3）时回退全库扫描。
- **小命中集直通**：hits ≤ 5 万（先输名字再 `path:` 精化的交互场景）直接逐条校验，免建表、
  免全库候选扫描。
- **骑缝段 1 字节也进变体表**：`path:a\d`（跨 `data\deep`）两侧段各仅 1 字节，低于旧下限会漏报
  （等价性回归抓出），已放宽——正确性优先，超短骑缝查询走慢路。

回归测试两组钉住等价：`path_two_phase_filter_matches_naive_full_path_scan`（10 组查询：骑缝跨分隔符
`c:\us`、`s\alice`，首/尾分隔符，目录条目自身等边界）与 `path_two_phase_large_hits_exact_check_split_matches_naive`
（6 万条目、hits 超过小路阈值强制走两段式主体；trigram 挂载 / 摘除双态 × 6 组 needle）。

实测 414–425 万条目库（`ROUNDS=9 cargo run --release -p findx2-core --example perf_suite -- <index.bin>`）：

| 查询 | 朴素实现 | 两段式（2.4.1） | 两级候选 + trigram（2.4.2） |
| --- | --- | --- | --- |
| `path:users`（~1.2 万 hits） | **6553.7 ms** | **950.2 ms**（约 6.9x） | **313.2 ms**（约 21x） |

其余 10 项查询（子串 / ext / folder / startwith / endwith / 拼音 / 排序）全部无回归；剩余成本为
建表 + 目录侧判定 + 全库一遍轻量展开（每条仅一次数组查表）+ hits 遍历，约 300 ms 量级。

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
cd gui
npm run tauri dev    # 开发
npm run tauri build  # 打包
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

trigram 剪枝有两个边车：`<index>.tri`（倒排表，构建/重建时原子替换）与 `<index>.tri.pending`（增量位图 + 快照条目数，随 `index.bin` 一起落盘；加载时若两者快照数不一致则禁用剪枝回退全表扫描）。边车缺失/损坏一律静默降级，service 启动时后台补建。

## 版本号（GUI / 安装包）

- Tauri 与 GUI 以 **`gui/src-tauri/tauri.conf.json`** 与 **`gui/src-tauri/Cargo.toml`** 的 `version` 为准。  
- **Inno** 安装包版本在 CI 中由 **`/DMyAppVersion=`** 传入 [`installer/FindX.iss`](installer/FindX.iss)（与 tag 如 `v2.2.0` 的纯数字部分一致即可）；`iss` 内 `#define MyAppVersion` 为本地无参数编译时的默认。macOS / Linux 包版本直接取 Tauri `version`。

变更记录见仓库根目录 [`CHANGELOG.md`](CHANGELOG.md)。

## 随系统启动

设置页「随系统启动」勾选后，登录时自动拉起 FindX 托盘界面。实现走当前用户的
`HKCU\Software\Microsoft\Windows\CurrentVersion\Run`，**不需要管理员权限**；写入后立即回读校验，
失败会在界面回报而不是静默吞掉。真源是注册表，设置文件里的 `auto_start_app` 只是记录。

两点容易混：

- **这个开关只管托盘界面**（`FindX.exe`）。索引服务 `FindX2Search` 由 SCM 按 `AutoStart` 拉起，
  与开关无关；把它勾掉不会停掉索引。
- 非 Windows 平台该开关明确返回「暂不支持」，不做假动作。

同一页面另有一个「GUI 启动时自动拉起索引服务」，管的是 GUI 与会话内索引进程的关系，不是登录自启。

## 索引加载的可观测性

`index.bin` 加载（`load_index_bin`）要逐段反序列化并重建 trigram，条目多时可达几十秒到几分钟。
这期间 service 已监听命名管道，但引擎槽还是空的，`Status` 回报 `loading=true`。
2.4.0 起这段不再是一个静止的「加载中」：

| 字段 | 含义 |
| --- | --- |
| `loading_stage` | 当前阶段文案，含阶段序号、已耗时、条目数 |
| `loading_elapsed_secs` | 加载已耗时（秒） |
| `loading_phase_done` / `loading_phase_total` | 第几 / 共几阶段 |

八个阶段依次为：打开索引文件 → 读文件头 → 解析条目 → 解析目录 → 解析 FRN → 构建索引表 →
过滤器 → CJK 位图。字段全部 `#[serde(default)]`，旧版 service 不报这些字段时 GUI 退回
「加载中（已 Ns）」，旧版 GUI 遇到新 service 则忽略多余字段。

状态栏示例：`索引: 加载中 · 解析条目（3/8 阶段，已 42s，共 3.1 亿条）`。

### 超大索引的容量约束与回收

历史上一份 `C:\ProgramData\FindX\index.bin` 曾膨胀到 **3.09 亿条目 / 17.72 GB**，
按当时的内存布局下界约 **21.9 GB**：

| 组成 | 估算 |
| --- | --- |
| `entries`（3.09 亿 × 32 B） | 9.21 GB |
| `names_buf` | 4.63 GB |
| `MetaOverlay`（3.09 亿 × 16 B，非惰性） | 4.60 GB |
| `FrnIdxMap` | ≈3.45 GB |

物理内存不足时加载与随后的元数据回填会持续换页，耗时以分钟计。

#### 为什么会有 3.09 亿条目：墓碑只增不减

索引重建过去走「旧卷区间整段打墓碑 + 新条目后挂」的合并策略
（`findx2-service/src/run.rs` 的 `merge_rebuilt_volume` → `old.delete_entry(i)`），
**只标记不物理删除**。每次全卷重建都把该卷全部旧条目原地留为墓碑，条目数只增不减。

本机实测该份索引：

| 指标 | 实测值 |
| --- | --- |
| 条目总数 | 309,031,075 |
| 其中墓碑 | 305,160,707（**98.75%**） |
| 存活条目 | **3,870,368** |
| 墓碑 : 有效 | 约 **80 : 1** |
| 磁盘占用 | 61.6 字节/条目（17.72 GB） |
| 加载耗时 | **55–83 s** |

也就是说这份 17.7 GB 的索引里，约 16.5 GB 是废数据，真正有效的不到 4 百万条。
`fused_scan` 还要对全部 3.09 亿条逐个判 `is_deleted()`，实测 `readme` 子串查询
**31–44 ms**。

**已从三个层面解决**（2026-09-21 实测：删除重建后 **4,078,467 条目 / 234 MB**，
加载 **0.35 s**，常驻 **375 MB**，建库仅 **25.3 s**）：

1. **物理压缩 `IndexStore::compact_tombstones`**：把墓碑条目从索引里真正删掉，并统一重映射所有
   与之耦合的下标。走「先算保留集、再整体重写」而不是逐个 `remove`——后者既 O(n²)，又极易漏改某个下标。

   | 下标 | 处理 |
   | --- | --- |
   | `entries` / `frns` | 按保留集重写；**条目区与 FRN 区一起缩** |
   | `dirs` | 墓碑目录连带移除（否则父链会指向已消失的目录） |
   | `entries[].dir_idx` / `dirs[].parent_idx` | 查目录映射表重映射；**父目录被删的条目一并丢弃** |
   | `dir_index` / `frn_to_entry` | 按新下标重建 |
   | `ext_filter` | 按新下标重建（每桶要求升序，重建更稳） |
   | `dir_path_ranges` | 按目录映射表搬运（`dir_paths_buf` 保留原偏移，不重排） |
   | `volumes[].first_entry_idx` | 按条目映射表重定位；整卷皆墓碑则贴到末尾 |
   | `deleted` | 清空 |
   | `tri_pending` / `cjk_names` / `trigram` | 按新下标重映射；trigram 边车整体重建 |

   `names_buf` 是**共享的**（名字 intern 后多条目录复用同一段字节），所以不做逐字节日志式回收——
   那需要给每个名字引用计数，收益不抵复杂度；只回收墓碑条目独占的 `entries` + `frns` 已是大头。

2. **掐断产生源**：`merge_rebuilt_volume` 改为 `IndexStore::remove_volume_entries(letter)`，
   把该卷区间从 `entries` / `frns` **物理摘除**后再走同一套重映射。全卷重建从此不再留垃圾。

3. **可日用入口 + 体检 + 自动压缩**：`findx2 compact --index <index.bin> [--dry-run]` 一键压缩（原子落盘 +
   自动重建 `.tri` 边车）；`findx2 status` 展示墓碑数与占比。
   2.4.1 起 GUI 设置页也有一键「压缩索引」：自动停服务 → 提权跑 CLI（UAC 被取消也会把服务拉回）
   → 自动重启服务 → 展示回收量；设置页同时显示墓碑占比（service 经 IPC 上报 `tombstone_count`）。
   2.4.3 起无需手动：service 在**启动窗口内**（加载完成后、引擎挂上管道前的零并发空档）体检
   命中即自动物理压缩 + 原子落盘，压缩期间状态栏显示「自动压缩墓碑」阶段；墓碑日积月累
   每天只涨 2–4%，每次开机启动体检一次，占比永远到不了高水位。落盘失败只降级提示，
   守卫触发（存活数 < 预期）拒绝以可疑索引启动，与 CLI 同一逻辑。

> 也可以手动压缩（需管理员终端）：`Stop-Service FindX2Search` → `findx2 compact --index C:\ProgramData\FindX\index.bin` → 启回服务。
> CLI 侧带**灾难性损失守卫**：压缩后存活条目数若少于「压缩前 − 墓碑数」则拒绝落盘，原索引不动。

其余可继续压的方向（**尚未实施**）：

1. 缩小索引范围（例如只索引用户目录 + 常用盘），从源头压条目数；**注意给 `excluded_dirs` 加目录是无效的**
   ——`mark_excluded_entries` 同样只打墓碑，不缩体积（见 `index.rs` 注释自述）。真要让排除目录不占体积，
   得重建 + 在 USN 增量层用 sidecar 挡住，或事后跑一次 `findx2 compact`。
2. `MetaOverlay` 改惰性结构，省下与条目数成正比的 4.6 GB。
3. 加载改为 mmap 惰性映射，避免整段 `memcpy` 进堆。

在此之前，勾选「随系统启动」会让登录后更早开始这段加载，首查可用时间相应前移，
但可用时间点本身不变。

## 许可证

Rust 工作区在根 [`Cargo.toml`](Cargo.toml) 中声明为 **`MIT OR Apache-2.0`**（与 `SPDX-License-Identifier` 常见写法一致）：你可任选其中一种条款使用本仓库代码。

- **MIT**：全文见 [`LICENSE`](LICENSE)（与 [`gui/LICENSE`](gui/LICENSE) 一致，便于 Tauri 打包引用）。
- **Apache-2.0**：全文见 [`LICENSE-APACHE`](LICENSE-APACHE)。

第三方依赖各自遵循其许可证；Windows、WebView2、Everything 兼容协议等以相应厂商条款为准。

# T120 Listary 6 逆向研究报告：索引 / 速度 / 拼音

日期：2026-11-24
目标：`C:\Users\chaoj\Downloads\Listary.exe`（Listary Setup，SHA-256 `c62249e0…a0288`，Inno Setup 6.2.1，内含文件时间戳 2025-08-15，即 Listary 6.3 当期构建）
及用户重装后的 `C:\Program Files\Listary`（版本 **6.3.5.94**，2026-11-24 复核：同一构建，结论全部成立）
方法：Inno 静默解包 → Rust/UniFFI 符号提取 → ilspycmd 反编译全部 .NET 程序集 → PE 导出表解析。
说明：本机 REA MCP 服务不可用（`rea-agents` npm 缓存损坏），本次为手工 RE。
状态：分析用临时安装已卸载干净（用户后自行重装使用）；复核用哈希固化如下。

关键文件 SHA-256（6.3.5.94）：
- `listary_engine.dll`（14465064） `1671b885…0b73c0882e`
- `Listary.exe`（2132008） `be2aec5c…c6929e7b20d9`
- `cnmatch.bin`（62269） `3afab3fb…68102ccaaf82997`
- `Listary.Service.exe`（31784） `73749a9b…99526eca884da1d`
- `Hooks/ListaryHook64-6.1.10.1.dll`（287280） `379feda7…9916c0a0`
- `Hooks/ListaryHook32-6.1.10.1.dll`（233512） `3fbb58f7…5578ac2955d115a4b3`
- 导出表：`nfunc=106 nname=106`，`uniffi_listary_engine_fn_method_*` 20 个（见 §1）。
- `cnmatch.bin`：`size=62269 syllables=422`（见 §4）。

---

## 1. 架构总览

Listary 6 是"**Rust 引擎 + .NET 外壳**"：

| 组件 | 技术 | 职责 |
|---|---|---|
| `listary_engine.dll`（14MB） | Rust，经 UniFFI 暴露 21 个方法 | 索引 + 搜索全部在这里 |
| `Listary.exe`（2MB） | WPF / .NET Framework 4.8 | 纯 UI + 搜索编排（20+ `ISearchProvider` 扇出、ViewModel 映射） |
| `Listary.Service.exe`（31KB） | SYSTEM 服务，WCF `net.pipe://localhost/Listary/ListaryService` | **特权句柄中介**：`CreateFile(\\.\C:)` 打开卷句柄后 `DuplicateHandle` 给引擎进程；`CreateProcessAsUser`（处理 UAC 分裂 token）拉起 Hook 宿主 |
| `DeviceIOControlLib.dll` | 通用 DeviceIoControl 封装 | Disk/Volume/USN/MFT 等 raw IO |
| `cnmatch.bin`（62KB） | 拼音音节表（格式已解析，见 §4） | 字→音映射数据 |
| `listary_engine.dll` 导出表 | 106 个导出，`UNIFFI_META_NAMESPACE_LISTARY_ENGINE` | `new/start/shutdown/restart`、`open_session/search/get_search_results/release_session/cancel_request`、`add/remove/rebuild/poll/wait_for_indexers`、`get_indexer_infos/diagnostics/process_info` |
| `Hooks/ListaryHook{32,64}.dll`、`FileAppPlugins/`（Explzh/Files/OneCommander/WinRAR/WPS 等） | Explorer / 打开保存对话框集成 | 双 Ctrl 菜单、目录跟踪 |

引擎源码路径从 panic 字符串还原（构建机 `D:\Listary\ListaryX\engine\...`，见 `core_files.txt` 37 条）。关键 crate 依赖：`rust-rocksdb`（git）、`uniffi_core 0.28.3`、`tokio 1.46`、`aho-corasick 1.1.3`、`memchr 2.7.4`、`dashmap/rayon/crossbeam`、`notify`，自研 `me-hashbrown`、`snapshot-hashmap`。

### `listary-core` 模块树

```
db/: builder, file_db, hard_link, priority, read_only, serialize, snapshot, transaction, writer
indexer/ntfs/: mft_reader, volume_monitor_thread, volume_change, list_nfts_volumes
indexer/folder/: mod, watcher, schedule, id_pool
indexed_search/: service, score_based_searcher, path_filter
string_matcher/: bm_searcher, keyword_string_matcher, fuzzy_string_matcher,
                 pinyin_searcher, fuzzy_pinyin_searcher
fuzzy/position_bitmap · pinyin/mod · unicode/mod · utils/{chrono,logger,owned_handle,process,string}
ffi/mod (UniFFI)
```

---

## 2. 索引：NTFS 直读 + RocksDB

1. **NTFS 卷直接解析 MFT**（`indexer/ntfs/mft_reader.rs`）："Failed to read MFT data extents"、"$MFT::$DATA"。不是 Win32 FindFiles，而是读 `$MFT::$DATA` extent 全量建库。`volume_monitor_thread` + `volume_change.rs` 消费 USN（"rebuild_index: Start building DB. Next USN"）做增量。`list_nfts_volumes` 按 `Volume{serial_number,label,root}` 跟踪卷。
2. **存储是 RocksDB**（rust-rocksdb，column family，"Received null column family handle"，"Failed to create RocksDB directory"），**零 SQLite**。`FileDB{FileNode{flags,file_name,parent_id,last_write_time}}` 存文件；内存另有一棵 `FolderTreeNode{first_child,next_sibling,priority}` 邻接树——**路径拼装不查库**，内存树直接走。
3. **自研并发结构**：vendored `me-hashbrown`、`snapshot-hashmap` + `dashmap/rayon/crossbeam`——读（搜索）走无锁快照，写（USN 增量）走后台，搜索永远不被索引阻塞。另有 `db/read_only.rs`（搜索用只读快照）、`snapshot.rs/transaction.rs`。
4. **自愈**："Error count exceeds the threshold, the index will be forced to rebuild at next start"、"Failed to load FileDB"、"Finish rebuilding index"。硬链接有槽位处理（`NoHardLinkSlotAvailable`）。
5. **优先级是一等公民**：`FilePriorityRules` + 三种规则（`FolderPriorityRule` 路径规则、`FileAttributePriorityRule` 属性规则、`FileNameRegexPriorityRule{regex,apply_to_files,apply_to_folders}`），五档 `Ignored/Low/Normal/Internal/High`（另有 `Internal` 共六个枚举值），C# 侧 `DiskSearchPriority{High=1,Normal=0,Low=-1,Ignored=-2}` 与之对应，`PriorityRules` 经 protobuf 从 UI 同步到引擎。

## 3. 速度：查询路径的每一层都在省

- **会话式增量搜索**：`open_session` 一次，之后 `search` 复用结果集（UI 侧 `IStringMatcher.IncrementalSearchId` 也印证）。`GetSearchResults(PaginationParams{offset,length})` 分页，列表虚拟化。
- **可取消 + 限量**：`CancellationToken`、`MaxResults`、`cancel_request` 全链路传递，旧查询直接丢弃。
- **匹配器分档**：`StringMatcherType{Keyword,Fuzzy}` + `ScoreBy{MatchScore,Combined}`，BM 跑精确关键字，`aho-corasick` 多模式，`memchr` SIMD，fuzzy 走 `position_bitmap`（类 Bitap 位并行）+ `GetMatchedPosition(BitArray)` 直接给高亮位置（UI 零二次计算）。
- **零拷贝字符串**：`IStringPiece` 切片（`StringStringPiece/SubstringStringPiece/SubStringPiece`）+ `AggressiveInlining` + `OnlyAscii` 快路径（纯 ASCII 走字节比较，`RemoveDiacritics` 做去重音归一化）。
- **结果排序**：C# 侧自研 `ParallelQuickSort`（阈值 8192，以上 `Parallel.Invoke` 双路归并），`SortBy{NoSort,Score,FileName,LastWriteTime}`；`CombinedScore(ulong)` = match 分与启动频率/新近度等融合（`PriorFolders/InPriorFolder` 当前目录加权，`FolderPartMatchers/OrderedFolderParts` 分段匹配）。
- **按路径/类型先剪枝再算分**：`OnlySearchInPaths`、`FolderParts`、`FileExtensions`、`DateModifiedStart/End`、`SearchForFiles/Folders/LowPriorityFiles`、`OnlyMatchBaseName` 全是下推到引擎的参数，不是取回再过滤。
- gRPC 说明：`Listary.exe` 内有 `IndexedSearchService` protobuf/gRPC 桩代码（`IndexedSearchSearchParams{SessionId,Query,StringMatcherType,MaxResults,OnlySearchInPaths,SearchForFolders/Files,FileExtensions,OnlyMatchBaseName,ScoreBy,SearchForLowPriorityFiles,FolderParts,DateModifiedStart/End}`），但主搜索路径是 UniFFI 进程内调用；gRPC 桩供 `FileAppPlugin.DevTools` 等外进程使用。

## 4. 拼音：编译期音节表 + 运行时模糊

`cnmatch.bin` 已解析头部：**u16le 个数 422 + 422 个长度前缀音节串**（`a,ai,an,ang,ao,ba,bai…`，覆盖全部普通话音节），后跟 ~60KB 字→音映射。引擎侧 `pinyin/mod.rs` + `pinyin_searcher.rs` + `fuzzy_pinyin_searcher.rs`，即**拼音首字母/全拼/模糊音**（`fuzzy_pinyin` 专门处理 z/zh、n/ng 这类）都在引擎内做，UI 只透传 `Query` 字符串。后 60KB 映射区确切记录格式未完全解出（不是简单的 `(u16 char, u8 k, k×u16)` 排列，可能含头部/DFA 状态），记为 open 项，不影响借鉴结论。

## 5. findx 可借鉴点（按性价比）

1. 会话式增量搜索（`open_session` + 结果集复用 + 分页）：findx 现 `search_paged` 按整串 query 缓存，打字每次全量——改成续敲过滤，输入体验质变。
2. 取消令牌全链路：新输入 5ms 内丢弃旧查询。
3. 匹配器分档 + 快路径：精确走 memchr/BM 纯字节路径，短查询（1~2 字符）直扫，长查询走 trigram。
4. `CombinedScore(u64)` 融合排序：match 分 + 启动频率 + 新近度 + 当前目录亲和（需**新建** launch 历史表；`usage.rs` 是磁盘用量树，不能用）。
5. 拼音表编译进二进制（62KB 常量，零依赖零加载），全拼/首字母/模糊音。
6. 只读快照隔离：搜索读冻结快照、USN 增量后台提交后原子切换。
7. 索引自愈计数器：错误超阈值下次启动强制重建。
8. 全量评估 `$MFT` 直读替代 `ENUM_USN_DATA`（见 T121 P1-0）。
9. 并行排序阈值：小结果集避免并行开销反噬。
10. 远期：特权服务中介句柄、按卷序列号跟踪、`FolderTreeNode` 式内存邻接树。

详细计划见 T121。

## 6. Listary 是怎么做到的（2026-11-25 实证，可复用部分与未解部分分开说）

**已证实、可直接抄的：**
- 导入表只有 13 个函数，唯一与磁盘 IO 相关的是 `ntdll!NtDeviceIoControlFile`（绕过 Win32，直接发内核 ioctl）。
- handle.exe 实证：**未提权的 Listary.exe 进程内握着两个 `C:\$MFT` 句柄**（`F04`/`E98`），
  另有 RocksDB 活文件（`Index/*.sst`、`MANIFEST`、`LOG`、`LOCK`，落盘 `%%AppData%%\...\UserProfile\Cache\MZ\Index\`）。
- `C:\$MFT` + `FILE_READ_ATTRIBUTES` 可直接打开（admin 与 SYSTEM 任务上下文均一次成功，23 个 extent）；
  失败的都是别的姿势：`\\.\C:\$MFT`（0x03，命名空间无此路径）、GENERIC_READ（0x5）、
  `OpenFileById(FRN=0)`（0x57，元文件保护、与上下文无关）。
- 结论：**Listary 不走 FRN（`OpenFileById` 根本打不开 $MFT），走 `C:\$MFT` 路径 + NtDeviceIoControl 发 ioctl。**
  非提权进程的句柄来源是 SYSTEM 服务 `CreateFile` + `DuplicateHandle`（T120 §1 已逆向出该接口；P3-1 既定架构）。
- 对 findx 的直接含义：P1-0 生产路径改为 `CreateFileW("C:\\$MFT", FILE_READ_ATTRIBUTES, ...)` +
  `FSCTL_GET_RETRIEVAL_POINTERS`，admin/service 上下文一次打通；非提权 GUI 走 service 中介（与现图纸一致，
  只是把原来设想的"服务代开卷"具体化为"服务代开 `$MFT` 本体"）。

**未解（不影响上条结论的使用）：**
- `listary_engine.dll` 全表仅 13 个导入（无 CreateFileW/ReadFile/GetProcAddress），疑似运行时动态解析或静态链接；
  与"谁替它 open/read"的问题相关，但不影响借鉴（我们走常规 Win32 导入即可）。
- record0 runlist 只覆盖 83% 而 RETRIEVAL 给出 23 个 extent 的原因（stale runlist vs 解析盲区）：生产路径已绕过，
  列为纯研究项，不阻塞。

---

## 6. Listary 是怎么做到的（2026-11-25 实证回答：MFT 直读路径）

用户问题：Listary 的 $MFT 是怎么打开的？以下全部实测，非推测。

1. **它根本不用 `OpenFileById(FRN=0)`**：`listary_engine.dll` 的导入表只有 13 个函数，
   唯一的 NT 相关项是 `ntdll!NtDeviceIoControlFile`（另有 kernel32!GetLocaleInfoW 等各 1 个）。
   Rust 引擎做这么多事却几乎零 Win32 导入——要么运行时动态解析，要么刻意最小化 IAT。
   但关键是：**没有 OpenFileById/CreateFileW/ReadFile 的静态导入**。
2. **它直接握着 `C:\$MFT` 路径句柄**：handle.exe 实测，**未提权的** `Listary.exe` 进程内有两个打开的
   `C:\$MFT` 句柄（`F04`/`E98`），另有 RocksDB 活文件（`Index/*.sst`、`MANIFEST`、`LOG`、`LOCK`）
   证实 RocksDB 落盘位置 `%%AppData%%\Roaming\Listary\UserProfile\Cache\MZ\Index\`。
3. **路径形式是关键**：`C:\$MFT`（盘符路径）+ `FILE_READ_ATTRIBUTES` 可开（admin 实测一次成功，
   且 `FSCTL_GET_RETRIEVAL_POINTERS` 一次拿全 23 个 extent）；
   而 `\\.\C:\$MFT` 报 0x03（命名空间无此路径）、GENERIC_READ 报 0x5（拒绝访问）、
   OpenFileById(FRN=0) 报 0x57（元文件保护，SYSTEM 下亦然）。
   ——我们之前卡住 purely 是因为一直在试错误的打开方式。
4. **非提权进程的句柄来源**：`ListaryService`（SYSTEM）的 `CreateFile(targetProcessId, path, access, flags)` +
   `DuplicateHandle` 正是干这个的——服务代开特权句柄，复制进 UI 进程。
   C# 侧 `DeviceIOControlLib` 提供全套 NTFS ioctl 封装
   （`FileSystemGetRetrievalPointers`/`FileSystemEnumUsnData`/`FileSystemQueryUsnJournal`/`FileSystemGetNtfsFileRecord`
   `FileSystemReadUsnJournal`/`FileSystemGetVolumeBitmap` 等），`Listary.exe` 内有
   `ListNtfsVolumes/GetNtfsVolumeHandles/NtfsVolumeIndex/MftStartLcn` 等配套逻辑。
5. **引擎侧只做 ioctl 与计算**：唯一的 ntdll 导入 `NtDeviceIoControlFile` 即绕过 Win32 封装直发内核，
   引擎日志（`ListaryEngineLog.txt`）与字符串证实其自行解析 MFT extent（"Failed to read MFT data extents" 系列）。
6. **结论（可直接抄的作业）**：
   `CreateFileW("C:\$MFT", FILE_READ_ATTRIBUTES, share-all, OPEN_EXISTING, FILE_FLAG_BACKUP_SEMANTICS)`
   → `FSCTL_GET_RETRIEVAL_POINTERS` 全 extent → 按 extent 顺序 raw 读 → Rust 并行解析。
   admin/service 上下文一次成功；非提权 UI 由 service 代开 + DuplicateHandle（P3-1 既定架构）。
   不需要 OpenFileById，不需要卷 raw 偏移心算，不需要 record0 runlist 解析。

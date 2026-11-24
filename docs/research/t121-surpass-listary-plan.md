# T121 全面超越 Listary 计划（V2，含 T120 映射）

日期：2026-11-24 ｜ 前置：T120 逆向研究报告 ｜ 基线：`docs/perf-baseline-2026-09-21.md`

目标：**全量建索引更快、逐键搜索体感更快**，拼音打平，Listary 没有的拉开差距。
总目标：重建 25.3s → <10s；逐键查询 P50 <8ms、P99 <30ms；`path:` <100ms；拼音 <5ms。

## 1. T120 → 本计划映射（5 条全覆盖）

| T120 条目 | findx 现状（经全仓探查确认） | 计划项 |
|---|---|---|
| $MFT extent 直读全量 + USN 增量 | 全量走 `FSCTL_ENUM_USN_DATA`（19s，`mft.rs`）；卷内 ioctl 链串行改不了；增量 USN 轮询+合流已成熟（`usn.rs:205-415` 三消息 pipeline：NamespaceBatch→StatBatch→Checkpoint） | **P1-0 spike**（§2）→ 赢了切直读；USN 增量不动 |
| RocksDB 存储 | **刻意不换**：自研 `persist_v6`（分段 `entries/frns/dirs/ranges/names` + 原子 manifest 代数）加载 0.35s 已领先；只借鉴"只读快照"语义 | P3-3 |
| 无锁读/后台写并发 | 已有 rayon 卷并行（SSD 并行/HDD 串行，`full_index_build.rs:124-232`）+ `meta_overlay` 无锁读 + WAL；缺读快照隔离语义 | P3-3 |
| 自愈（错误超阈值强制重建） | 已有 `JournalGap→全量重建`（`usn.rs:184-293`）、512MB journal 保障；缺计数器版 | P1-3 |
| 优先级三规则 + 五档 | 缺失（无 `DiskSearchPriority` 对等物） | P2-3 |

另有三处探查纠正（相对初版计划）：① `usage.rs` 是磁盘用量树、不是启动频率，CombinedScore 需**新建** launch 历史表；② trigram `<3B` 不剪枝、拼音绕过 trigram——1~2 字符查询恒全表扫（P0-4 对应）；③ `search_paged`+`PAGE_CACHE_TOP=8192`+`select_nth_unstable` 已有，但缓存 key 是整串 query，打字每次全量（P0-2 对应）。

## 2. P1-0 Spike：全量从 ENUM 切 $MFT 直读（实测中，2026-11-24）

- 工具：`bench_mft` 已实现五方案对比，含 Listary 路线（方案 5）。2026-11-24 补充：`--only 1|2|3|4|5|all`
  参数（单跑方案 5 不必等方案 4 的 270s）+ 启动时显式启用 `SeBackupPrivilege` + `OpenFileById` 被拒时回退
  record-0 runlist 解析（`mft_data_runs_record0`：纯扇区读 record 0，解析 $DATA 非驻留 runlist）。
- 本机实测（C 盘，WD SN740 1TB SSD，MFT 4548MB，428 万条）：
  - 方案 1 原始磁盘读：3.17s / 19.4 万条（只读首个连续区，覆盖不全）/ 6.1 万条/s，有 size+mtime。
  - 方案 2 `ENUM_USN_DATA`（现状）：22.84s / 428 万条（370 万文件+57 万目录）/ 18.7 万条/s，**不带 size**。
  - 方案 3 `GET_NTFS_FILE_RECORD`：bench 实现返回垃圾（0 成功）——逐条 ioctl 本来也不可枚举，丢弃。
  - 方案 4 ENUM+NtQueryDirectoryFile：270s（NtQuery 阶段 257s，39 万目录逐个开）。结论：**metadata 回填是真瓶颈**，
    佐证 P1-2（fast 模式 size=0 + 批量 auto-tune）的优先级。
  - 方案 5 extent 直读：首次跑 panic（`OpenFileById(FRN=0)` 失败）。根因链（SYSTEM 下实测修正）：
    ① `SeBackupPrivilege` 需显式启用是对的（已加）；② 但启用后仍被拒（0x57），对照实验证明同代码开 FRN 3 成功、
    FRN 0/5 失败——**NTFS 元文件保护与上下文无关（SYSTEM 同样被拒），不是提权问题**；
    Listary 的 extent 读取只能走 record0-runlist 解析（或未探明的驱动级手段）。
    结论见本节末裁决（混合路线）。
- 决议（2026-11-25 实测后）：混合路线验证通过但不合入（详见 §2.2 最终裁决）。

### 2.1 生产切流实现（worktree `target/t121-mft-extents`，分支 `feat/t121-mft-extents`，已提交 6 次）

- 新模块 `mft_extents.rs`（`scan_volume_extents`，返回 `(files, dirs, tail_file_idx, tail_dir_idx)`）：
  开卷 → `OpenFileById(FRN=0)` 尝试（预期失败，留作未来）→ record0-runlist 回退解析 →
  逐 run 采样校验（坏 run 整段判缺口）→ 缺口起 `scan_volume_from_frn` ENUM 补尾 →
  尾部与 bulk 按 FRN 去重（bulk 优先）→ 尾部 selective backfill（`fill_files_metadata_open_by_indices`）。
- 记录解析 `parse_mft_record_full`：corroborated FRN（seq<<48|ordinal，header@44 必等）；
  $FILE_NAME 选 Win32 命名空间；parent 取完整 64 位；size 取未命名 $DATA real size；
  mtime/ctime 取 SI（缺失时用 FN 兜底）；is_dir 四信号（记录头目录位 / `$INDEX_ROOT` / SI 位 / FN 位）；
  零页/未分配/seq0/无名/`.`/`..` 丢弃并计数（drop 统计用于归因）。
- 集成 `scan_volume_inner`：`FINDX2_MFT_SOURCE=auto|extents|enum`（默认 auto 先试 extent，失败回退 ENUM）；
  根注入抽成 `ensure_root_dir_entry` 两路径共用。
- 测试：8 个单测全绿 + `findx2-windows --lib` 64/64 全绿；frndiff.rs（FRN 级 diff 诊断工具）。
- open_mft 含 FRN 5/3 对照探测 + SeBackupPrivilege 显式启用（诊断用，证明 0x57 是元文件保护而非权限问题）。
- **2026-11-25 追加：改走 `C:\$MFT` + FILE_READ_ATTRIBUTES（Listary 同款），RETRIEVAL 权威 extent**：
  探针（admin/SYSTEM 双上下文）一次成功、23 个 extent；但生产代码首跑仍报 0x03 —— 根因是**盘符冒号丢失**：
  `volume="\\.\C:"` 经 `trim_end_matches(['\\', ':'])` 变成 `"\\.\C"`，再 strip 前缀得 `"C"`，拼出 `C\$MFT`
  （无冒号=盘符相对路径，不存在）→ 0x03。日志 `open_mft 路径=[C\$MFT]` 铁证；探针 PATH-BYTES `43 00 3a 00 5c`
  （`C : \`）对照钉死。修复：抽 `mft_path_for_volume()`（冒号统一保留）+ 回归单测锁 6 种形态
  （`C:`/`C`/`c:`/`\\.\C:`/`\\.\C`/`C:\`）——教训：`trim_end_matches` 绝不能吃 `:`。

### 2.2 P1-0 最终裁决（2026-11-25，RETRIEVAL 路径打通后改判：可合入）

**结论：`C:\$MFT` + READ_ATTR + RETRIEVAL 直读打通，缺口从 20 万降到 2988（散布=轮次漂移），覆盖率 99.93%。建议合入（`auto` 默认+失败回退已在）。**

- A/B（同机背靠背，相隔约 9 分钟）：hybrid 4,371,486 vs ENUM 4,340,733；
  `only_in_B（缺失）=2988`，分布全散（[0,50万)1013 为最大桶，无 [350万,400万) 集中），seq==0 计 0。
  对比 record0 时代的 201,063（98% 在 [350万,400万)）：runs 14/15 缺口随权威 extent 表消失。
- `only_in_A（多出）=33,741`（0.8%）：extracheck 抽样 200（`extracheck.rs`，FRN→路径重建→`Path::exists`）：
  存在 157（78%，真文件，ENUM USN-walk 反而漏了——hybrid 更全）；
  不存在 43，其中父目录仍在=真删除 9，父亦无（servicing/winsxs 整树流转或移动）34。
  外推口径差约 2.6 万、轮次删除差约 0.7 万；`unresolvable=0`，零垃圾记录证据。
  生产有 USN 增量清理残留项，非阻塞。附带发现：hybrid 在重叠区比现状 ENUM 更全（待 P1-1 深究 USN-walk 漏检原因）。
- 两处校验失败 run 照旧走 ENUM 补尾（vcn=818048 len=51228 老坏段 + vcn=1151297 末端 run），尾部新增仅 17 万（其余去重命中），
  说明 bulk 与 ENUM 在重叠区一致。
- 性能账（温缓存背靠背，同机 01:07/01:11，诚实口径、冷机待补）：
  hybrid 端到端约 40s（bulk 4297MB raw 读+并行解析约 5s → 尾部 ENUM 约 5.3s →
  尾部回填 17.3 万文件约 18.7s + 2.8 万目录约 2.3s → 落盘+trigram 约 7s）；
  ENUM 现状端到端约 282s（MFT 走查约 23s + NtQueryDirectoryFile 目录 rescan 约 249s（398K 目录，本机奇慢）+ 落盘约 10s）。
  端到端约 7 倍；结构性胜利是 hybrid 跳过了目录 rescan（249s → 目录回填 2.3s），bulk 本身约 4.5 倍。
  瓶颈仍在尾部 selective backfill（18.7s，约 9.3K 条/s）——P1-1 打这里。冷缓存 A/B 仍为 open 项（不阻塞合入）。
- **2026-11-25 采样公式 bug 修复（优化B，先于优化A见效，优化A直接作废）**：
  `validate_runs` 偏移曾误写 `(kk % 每簇记录数) * 记录大小`，中/尾采样点全挤在 run 头部 4K 内；
  vcn=818048 头部恰为已删记录小片区 → 三连挂 → 200MB 整 run 误判缺口 → 17 万条补尾 → 18.7s 回填。
  法医（SYSTEM python 探针，独立 RETRIEVAL + 落盘字节）：两可疑 run 头记录 magic/ordinal 全对（seq=23/3），
  挂在 not-in-use/无 FN 上；中/尾点读错位置故 ordinal 必对不上——读错而非数据错。
  修复（`sample_points` 纯函数，首/中/末簇 spread + 单测锁定法医向量）后：
  818048 正名，22 extents/4497MB，尾部仅剩末 run（真零区，未写簇）→ 补尾 5700 文件+905 目录 → 回填约 1s；
  **hybrid 端到端约 15s（bulk+补尾约 6s，落盘+trigram 约 7s），端到端约 19 倍**。
  A/B（hybrid 01:50 vs ENUM 01:16，间隔 34 分钟）：缺失 4836（散布，42 个 10 万桶仅 5 个超 300，
  无成墙；[3.5M,4M) 仅 167——runs 14/15 缺口彻底消失），多出 38491（口径差为主，见 extracheck）。
  瓶颈转移：落盘+trigram（~7s）已超 bulk 本身；P1-1 方向→增量/常驻 + 尾部 6.6K 的 OpenFileById（~1s，可忽略）。
  冷缓存 A/B 仍为 open 项（不阻塞合入）。
- 备忘：早期 bench 数据（方案 2/4/5 对比、0x80 截断发现过程）见 git 历史；结论以本节为准。

## 3. 第 1 波：查询路径（1~2 周，体感质变）

- **P0-1 查询取消全链路**：pipe 协议加 `request_id`+cancel 消息（`findx2-ipc/src/lib.rs:64-147`，`IpcRequest::Search{query,pinyin,limit,offset}` 现无取消语义）；`search_paged`（`search.rs:458-562`）支持中途退出；GUI 新击键即发 cancel。验收：旧查询 5ms 内被丢弃，快速输入无堆积、无"结果闪回旧查询"。
- **P0-2 会话式增量**：同 session query-extend（`ab`⊆`abc`）复用上次结果集过滤；`get_search_results(offset,length)` 只取当页（service 侧 `ipc_dispatch.rs:288-322` 已只回当页 DTO，前端 `search_session.rs` 配合）。验收：连续输入总耗时 <1.5×首查；翻页不重查。注意现有 `PAGE_CACHE`（`search.rs:191-206`）按整串+revision key，revision 一 bump 全失效——session 层要建在它之上。
- **P0-4 短查询分流**：trigram `tri_pending`/`snapshot` 机制保留，`<3B`/拼音路径（`search.rs:1751-1755,2040-2067`）改走 memchr/BM 直扫 + ASCII 快路径；perf_suite 加 1~2 字符矩阵，P50 <8ms。
- **P0-3 `path:` 灭杀**：`search.rs:645-666` 逐条回溯拼路径 + `SMALL_HITS_DIRECT=50000`（`:821`）→ 两段式（先 parent/folder 缩候选）或 path-trie 边车（可与 P1-4 内存树合并）。验收 <100ms（现 5873ms）。

## 4. 第 2 波：索引压榨（2~3 周）

- **P1-1 五段 pipeline 重叠 + 打点**：枚举→解析→组装→intern→落盘（19+3+2.87s 疑似串行），`IndexBuilder` 分片（`index.rs:653-656`）与 trigram 构建（`trigram.rs:359-360`）同策略并行化。验收：同机重建 <15s 起步，目标 <10s。
- **P1-2 回填调优**：fast 模式 size/mtime 置 0 + USN 近似（`mft.rs:561-573`，保持）；`OpenFileById` 池化参数 sweep；HDD `$MFT` 直读 vs SSD `NtQueryDirectoryFile` 批量选型 auto-tune（`n_dir_query.rs`、`storageSeekPenalty` 画像已存在）。
- **P3-2 卷序列号主键**：`ntfs_identity.rs/mft_identity.rs` 已有卷身份雏形，主键从盘符切 serial（对标 Listary `Volume{serial_number,label,root}`），`remove_volume_entries` 按 serial 摘除（历史 bug 已修，勿回退）。验收：改盘符 0 重建。
- **P1-3 水位常驻 + 自愈计数器**：`last_usn/journal_id` 回写已存在，补错误计数器（抄 Listary"超阈值下次启动强制重建"）+ `status` 展示水位。验收：日常启动追赶 <1s。
- **P0-5 launch 历史 + CombinedScore(u64)**：新建表 `{path_hash → 打开次数, last_open}`（GUI 上报 + service 记；`usage.rs` 无关，勿复用）；`match|频率|新近|目录亲和` 压进 u64 一次比较定序（对标 `SearchParams{MatchScore,Combined}` + `PriorFolders`）；`SortBy{Score,…}` 落到 query 语法。验收：高频/刚用过进前 5 + 可解释打分文档。

## 5. 第 3 波：拼音 + 优先级

- **P2-1 模糊音**：`ib-matcher 0.4`（`findx2-core/Cargo.toml:10,29`，`Ascii` 全拼 + `AsciiFirstLetter` 首字母，双拼关）补 z/zh、n/ng、前后鼻；`search.rs:44-69,1672-1733`；测试矩阵钉住。先评估升级 ib-matcher vs 自研小表，不预设重造轮子（T120 的 62KB 表思路仅作备选）。
- **P2-2 拼音开销隔离**：Auto 模式所有非空 needle 都试拼音（`search.rs:1680-1690`）→ 纯英文查询跳过拼音编译；regex/glob/case-sensitive 不走拼音（现状保持）。验收：拼音 P50 <5ms 不回退（现 `jpg` 3.6ms）。
- **P2-3 优先级规则引擎**：路径/属性/文件名正则三规则 × `Ignored/Low/Normal/Internal/High` 五档（对标 `FileNameRegexPriorityRule{regex,apply_to_files,apply_to_folders}`）；`Ignored` 直接不进索引；规则热生效。验收：`C:\Windows\Temp` 类一键 Ignored。
- **P1-4 内存邻接树**：`FolderTreeNode{first_child,next_sibling}` 常驻内存，路径拼装零回表（与 P0-3 合并实施）；内存增量 <15%。

## 6. 第 4 波：架构远期 + 拉开差距

- **P3-1 特权服务中介句柄**（对标 `ListaryService.CreateFile→DuplicateHandle` + `CreateProcessAsUser`）：`win_service.rs` 上加"代开卷句柄 + duplicate 给引擎"接口；动安装与提权，独立 release 包验证，不进主线默认安装。验收：非提权 UI 全卷可读。
- **P3-3 读快照隔离**：`persist_v6` manifest 代数 + overlay 已接近原子切换，补"搜索读冻结快照、增量后台提交后切换"（对标 `db/read_only.rs` + `snapshot-hashmap` 语义；先测现有瓶颈再决定是否引入类 snapshot 结构）。验收：重建期间搜索 P99 劣化 <2×。
- **P4 穿插**：Everything 语法超集保持、`findx2-content` 内容搜索推向可用、Linux/macOS parity、CLI 健康体系（`compact`+灾难守卫+墓碑体检已有，继续加深）。

## 7. T120 对照补遗（2026-11-24 审计）

逐条核对 T120 §5（10 条借鉴）+ §2–§4 细节，结论：9/10 已覆盖，补遗如下。

| T120 出处 | 状态 | 落位 |
|---|---|---|
| ①会话增量 ②取消令牌 ③分档快路径 ④CombinedScore ⑤拼音表 ⑥快照隔离 ⑦自愈 ⑧$MFT直读 ⑩远期三项 | ✅ 已覆盖 | P0-2/P0-1/P0-4/P0-5/P2/P3-3/P1-3/P1-0/P3-1/P3-2/P1-4 |
| ⑨并行排序阈值（`ParallelQuickSort` 8192 以下走串行） | ⛔ 遗漏，已补 | P1-1 追加：验证小结果集 rayon 开销，`select_nth_unstable` 后 Top-K 排序加串行/并行阈值 |
| `GetMatchedPosition` 高亮位置引擎一次算好 | ⚠️ 待验证，已补 | P0-4 追加：确认 IPC DTO 是否回传匹配位置；若无则加，避免 UI 二次计算 |
| `RemoveDiacritics` 去重音归一化（`unicode/mod.rs`） | ⚠️ 待验证，已补 | P2-2 追加：确认 query 归一化链（大小写/重音/全半角）；若无则加，低成本 |
| 硬链接多父目录（`hard_link.rs`/`NoHardLinkSlotAvailable`） | ⚠️ 待验证，已补 | P1-0 追加：同一 FRN 多 parent 场景正确性验证（findx `FrnIdxMap` overlay 语义 + `merge_rebuilt_volume`） |
| Explorer/对话框集成（Hooks 双 Ctrl、FileAppPlugins） | ➖ 有意排除 | 产品功能面，非索引/速度/拼音，不进本计划，另议 |

## 8. 风险与回退 / 过程纪律

- 动全量路径（P1-0/P1-1）：双路径 flag + `compact` 灾难守卫 + 重建前后 `status` 体检；兜底=删库重建（目标 10s 内）。
- 动 IPC 协议（P0-1/P0-2）：版本协商，老 GUI + 新 service 可混合（`legacy_compat.rs` precedent）。
- 每任务 `docs/research/tXXX-*.md` 留痕 + perf_suite 前后数；大体积临时目录按 AGENTS.md（`target/t<编号>-<事由>` 或 Temp），收尾清掉。

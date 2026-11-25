# T122 查询路径第 1 波执行记录（2026-11-25，深夜一口气）

工作区：`target/t122-query-path`，分支 `feat/t122-query-path`（staging hygiene 见 AGENTS.md）。
范围：P0-1（取消）✅、P0-2（会话增量）✅、P0-3（目录表缓存）✅、P1-3（自愈计数器）✅。
未做：P0-4（短查询分流）、P0-5（launch 历史打分）——另排期；GUI 接线只到 IPC 客户端兼容层，
`search_session` 的 request/session 编排 + `__CANCELLED__` 吞掉 +  stale-drop 待 GUI 侧落实。

## P0-1 查询取消全链路

- 协议（`findx2-ipc`，全向后兼容，`#[serde(default)]`）：`Search{request_id, session_id}`（0=不跟踪），
  新增 `SearchCancel{request_id}`（幂等，未知 ID 也回 `Cancelled`），新增 `Cancelled{request_id}`，
  `SearchResult` 回显 `request_id`。
- core（`search.rs`）：`Error::Cancelled`；`search_paged` 多 `cancel: Option<&AtomicBool>`；
  `fused_scan_par`/`fused_scan_pinyin_par`/`slow_attr_candidates`/`path_candidate_ids` 的
  `fold/reduce` → `try_fold/try_reduce`（每 4096 条查一次，relaxed，中止延迟约一个 chunk）；
  miss 路径扫描后/排序前/构页前三检查点；缓存命中不取消（已完成的工作不丢弃）。
- service（`ipc_dispatch.rs`）：全局在飞表 `request_id → flag`（注册/摘除/取消），
  `SearchCancel` 置位，`Cancelled` 非 Error（GUI 不弹错）。
- GUI（`ipc.rs`）：`SearchOutcome.request_id`、`search_tracked(endpoint,…,request_id,session_id)`、
  `CANCELLED_PREFIX="__CANCELLED__:"` 约定；`mock_backend` 回显 + 取消幂等。`search()` 仍传 0（行为不变）。
- 测试：core 3 个（预置位立即中止/2ms 后飞行中止/缓存命中不取消）+ service 3 个（回显/幂等/预置位回 Cancelled 非 Error）；
  全量 core 单线程 82 通过（多线程下 fence/persist 个别抖动为本机 pre-existing，干净树同现）。

## P0-2 会话增量（核心结论：字面路径 sound，拼音路径暂不进）

- 机制：`SearchEngine.sessions: HashMap<u64, SessionEntry>`（cap 4，LRU；`Arc<Vec<u32>>` 零拷贝）；
  miss 时命中（同 opts+revision、结构除 needle 外全等、needle 收窄）→ 在基集上 fused 计数扫描（exact total）
  + top 复用（旧 top 按 needle 过滤覆盖本页即免 `sort_top_k`）；否则全量 + 存新基（收窄集）。
- soundness 证明写在代码里：新结果集 ⊆ 基结果集（AND 语义 + 同 filters + 同 revision），top 复用要求排序相同
  且过滤后覆盖 `min(want_end,total)`；任一条件不满足回退全量。`page_cache` 与会话正交（同文本走前者）。
- 拼音 excluded（sound）：Auto 模式任何非空 needle 都填 pin，而 lita 匹配的 needle 前缀单调性未经证明，
  赌召回是不可接受的。会话要求新/基查询 `pin_res` 均为空。GUI 默认（Auto+开）下会话暂不生效——
  生效条件是 P2-2（纯英文跳过拼音编译）落地。字面查询（`allow_pinyin=false` 或 Off/Explicit 下的非拼音串）全生效。
- 附带发现（修了两个真 bug）：①修饰符前置语法下文本前缀 ≠ 语义前缀，门控改用解析结构；
  ②`sort:size desc` 正确写法是 `sort:size:desc`（冒号分隔）。
- 测试：纯函数单测（struct_compat/needles_narrow）+ 集成 4 个（续写复用与全量逐条一致/top 复用计数/排序变化重排/
  非前缀回退/LRU 逐出）。真机（4.37M 索引）：`wi→win→wind→windo` 会话 54.4ms vs 全量 90.0ms = **1.32×（<1.5 PASS）**，
  命中数完全一致；trigram 强 needle 下会话几乎无增益（全扫本就 15-40ms），价值在短 needle/弱剪枝链。

## P0-3 目录表缓存

- `SearchEngine.dir_paths: Option<(rev, Arc<DirPathTable>)>`，`path:` 查询复用；unix 不建；非 path: 零开销；
  逃生开关 `FINDX2_NO_DIR_TABLE_CACHE=1`（回退旧行为：每次现建 + 小命中逐条回溯）。
- 小命中路径（≤50K）改用缓存表（缺表才回退空表语义）；大命中两段式用同一张表；候选构建函数同步取 `cancel`。
- 测试：缓存表与现建表逐字节相同、revision 变化重建（`Arc` 指针判等）。真机 A/B（同二进制，开关对照）：
  `path:windows` 303.7 vs 341.7ms，`path:system32` 254.8 vs 317.2ms（省一次建表 ~40-60ms），
  小命中 `path:windows readme` 12.5 vs 51.6ms（免逐条父链回溯，约 4×），`path:temp log` 105.4 vs 168.0ms。
- P0-3 原验收（<100ms，现 5873ms）的前提已过时：两段式落地后大 path: 本就是 250-500ms（scan+sort 主导），
  小查询 12-105ms。表缓存解决的是重复建表；大 path: 进 <100ms 需 scan 侧动手（P1-1/后续）。

## P1-3 自愈计数器

- 新模块 `service::rebuild_health`：`rebuild_health.json`（与 `index.bin` 同目录，原子替换写，损坏按 0），
  按卷计数；`record_failure`（饱和+1，阈值 3 对齐进程内 `gap_rebuilds<3`）、`record_success`（非零才写）、
  `take_force_rebuild`（超阈值返回 true 并消费清零）、`failures_for_letter`（Status 用）。
- 接线（`run.rs`）：启动登记 index 路径；watch 循环入口超阈值 → 强制全卷重建（失败再记一次）；
  进程内 3 次用尽 + `rebuild_volume` 失败 → 记；健康 Checkpoint 到达 → 清零。
- 协议：`StatusResult.rebuild_failures: u32`（default 0；老 GUI/服务互操作安全），service 三处构造 + mock + 4 个 GUI 测试固件已补。
- 测试：模块 4 个（缺失/损坏按 0、三次触发一次强制并消费、成功清零+分卷独立）。
- 未验证：真机重启走强制重建（动线上服务，留白天窗口做）。

## GUI 侧待落实（contract）

1. 输入框固定 `session_id`（换框/新意图换 ID）；每次击键 `request_id+1`，先发 `SearchCancel(prev)` 再发 `Search`。
2. 收到 `Err("__CANCELLED__:id")` 静默丢弃（不弹错、不清空旧结果）；`SearchOutcome.request_id != 最新 ID` 的回包丢弃（防闪回）。
3. `rebuild_failures > 0` 时状态栏提示"索引曾多次重建失败"（可选）。
4. `search_session.rs` 编排 + 实测打字链（见本段 1-2）。

## 遗留 open 项（2026-11-25 快照；重排见 T123）

- 冷缓存诚实计时（P0-1/P0-2/P0-3 均为温缓存数）。
- P1-3 真机重启验证（白天窗口）。
- pin 会话的 soundness（lita 前缀单调性证明或 P2-2 落地）。
- P0-4/P0-5 未启动。

# P1-D launch 历史（实现记录，2026-11-25）

纯记录层已落地；排序接线（`CombinedScore` 压 u64 + `sort_top_k`）为 T123
P1-D 第二步，归排序 owner。

## 管线

- **service**：`launch_hist.rs`。边车 `<index>.launch.json`（与 index.bin 同目录），
  `{path → {count, last_open}}`，原子替换写（tmp+rename），损坏按空表处理。
  上限 20_000 条，超限 LRU 淘汰（淘汰 `last_open` 最老）。进程级单例，
  `run_foreground` 经 `note_launch_index` 报备路径（与 rebuild_health 并列）。
- **IPC**：`RecordOpen { path }`（fire-and-forget，恒回 `Pong`；老对端不发即无行为）。
- **GUI**：`perform_system_open` 成功回调里 `ipc.record_open(endpoint, path)`
  （fire-and-forget；路径先克隆再 spawn，hit 所有权不冲突）。

## 评分口径（排序 owner 接线用，当前未接入）

```rust
// 高频且新 → 大；未知路径 → 0。
// recency: 30 天线性加权（<<32）；freq: count 对数档（低 32 位）。
pub fn score_for(rec: Option<LaunchRec>, now_unix: u64) -> u64
```

预留 `lookup(path)` 查询口；接线时在 `sort_top_k` 命中构建处按
`launch_hist::lookup` 取 `score_for` 参与 `SortField::Score` 合成即可，
无需触碰索引格式。

## 验证（dev 栈实测）

- 管道 `RecordOpen` ×2（原始/归一化写法各一）→ 边车 1 条 `count=2`（归一化
  到小写+反斜杠同键，正确）；
- 单测 5 项：计数/新近更新、LRU 淘汰+落盘重载、评分单调（高频新 > 低频老）。

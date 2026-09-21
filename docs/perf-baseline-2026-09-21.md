# FindX 索引重建 + 性能基线报告

日期：2026-09-21
执行人：小满（受马老师委托）
索引路径：`C:\ProgramData\FindX\index.bin`

---

## 一、重建前后对比

| 指标 | 重建前 | 重建后 | 变化 |
| --- | --- | --- | --- |
| 条目数 | 309,031,075 | **4,078,467** | ↓ 98.68% |
| 墓碑数 | 305,160,707（98.75%） | **0** | 清零 |
| 存活条目 | 3,870,368 | 4,078,467 | ↑ 5.4%（重建扫到更多当前文件） |
| 目录数 | 41,275,699 | **547,024** | ↓ 98.67% |
| `index.bin` | 19,025,939,620 B（17.72 GB） | **234,088,892 B（0.22 GB）** | ↓ 98.77% |
| `index.bin.tri` | 56,656,190 B | 162,842,964 B | ↑（旧边车对应墓碑库，语义已失效） |
| 目录总量（含孤儿 tmp） | 22.04 GB | 0.40 GB | ↓ 98.2% |

**删除释放：22.04 GB**（C 盘可用 55.55 GB → 77.58 GB）

### 重建耗时：25.3 秒

分阶段：

| 阶段 | 耗时 |
| --- | --- |
| MFT 枚举（`FSCTL_ENUM_USN_DATA` V0） | ~19 s（3,531,443 文件 + 547,024 目录） |
| 目录拓扑 + 条目组装 | ~3 s |
| `names_buf` interning | 4,078,467 → 1,548,316 段（46.6 MB） |
| 落盘 | 0.25 s（234 MiB） |
| trigram 边车 | 2.87 s（10,377 键 / 155.3 MiB） |

---

## 二、性能基线（`perf_suite`，9 轮取中位数）

### 加载

| 指标 | 实测 |
| --- | --- |
| `load_index_bin` | **351.3 ms（0.35 s）** |
| 加载前 RSS | 8.7 MB |
| 加载后 RSS | 304.5 MB |
| 加载净增 RSS | 295.8 MB |

> 对比旧库：加载 **55–83 s**、内存下界 ≈21.9 GB。**加载提速约 160–230 倍。**

### 索引构成

| 组成 | 大小 |
| --- | --- |
| 条目区 | 0.12 GB（4,078,467 × 32 B） |
| 目录区 | 0.01 GB（547,024 × 24 B） |
| 名字区 | 0.05 GB（48,831,340 B，**11.97 B/条**） |
| 磁盘字节/条目 | 57.4 |

### 查询延迟

| 查询 | 中位数 | 最小 | 命中 |
| --- | --- | --- | --- |
| 子串 `readme` | 4.6 ms | 3.7 ms | 1000 |
| 子串 `config` | 4.7 ms | 3.9 ms | 1000 |
| `ext:txt` | 2.8 ms | 2.5 ms | 1000 |
| `ext:pdf` | 1.5 ms | 1.2 ms | 525 |
| `ext:txt readme` | 1.8 ms | 1.5 ms | 301 |
| `folder:tmp` | 2.8 ms | 2.3 ms | 1000 |
| `startwith:test` | 14.9 ms | 13.9 ms | 1000 |
| `endwith:.log` | 4.6 ms | 3.9 ms | 1000 |
| 拼音 `jpg` | 3.6 ms | 3.1 ms | 1000 |
| `ext:txt sort:size desc 100` | 1.3 ms | 1.0 ms | 100 |
| **`path:users`** | **5873.8 ms** | 5445.1 ms | 1000 |

**常驻内存 RSS：375.2 MB**（旧库 691.9 MB 基线，且旧库是 850 万条目量级；本次是 407 万条目）

---

## 三、发现的遗留问题

### `path:` 查询 5.8 秒（既有行为，非本次引入）

`path_match` 的实现是对每条命中调 `path_full_lower(store, idx, nowfn)` —— 沿 `dir_idx` 父链
回溯拼全路径（路径不物化，与 Everything 一致的取舍）。407 万条目 × 逐条拼路径 → 5.8 s。

- 源码位置：`search.rs:645-666`
- 性质：**既有设计取舍**，与墓碑压缩/重建无关
- 缓解方向：若要让 `path:` 也进入毫秒级，需要路径前缀索引（tri 边车式的 path trie）
  或让 `path:` 走「先按 `folder:`/`parent:` 缩小候选集」的两段式。**需产品决策。**

---

## 四、本轮代码改动

| 文件 | 改动 |
| --- | --- |
| `crates/findx2-core/src/index.rs` | 新增 `should_compact` / `compact_tombstones` / `remove_volume_entries`；**修复 `remove_volume_entries` 区间覆盖整库时会清空索引的 bug** |
| `crates/findx2-core/tests/roundtrip.rs` | 新增压缩一致性测试 + `remove_volume_entries` 区间安全回归 |
| `crates/findx2-core/examples/perf_suite.rs` | **新增**：全套性能基线（加载/构成/RSS/查询矩阵） |
| `crates/findx2-service/src/run.rs` | `merge_rebuilt_volume` 改物理摘除；加载后墓碑健康度体检 |
| `crates/findx2-cli/src/main.rs` | 新增 `compact` 子命令 + **灾难性损失守卫**；`status` 展示墓碑 |
| `CHANGELOG.md` / `README.md` | 墓碑问题从「遗留（待决策）」改为「已修」 |

### 关键 bug：`remove_volume_entries` 区间推断

卷区间靠 `volumes[].first_entry_idx` 排序后取「本卷起点 → 下一卷起点」。
单卷且 `first_entry_idx == 0` 时区间退化成 `0..entry_count`（覆盖整库），
「摘掉该卷」被解释成「清空索引」——实测把 309,031,075 条全部删光（`entry_count=0`）。

**已修**：区间覆盖整库时直接拒绝（返回 0，不动数据）；CLI 加落盘前守卫
（存活 < 压缩前-墓碑数 则拒绝落盘）。回归测试 `remove_volume_entries_refuses_when_volume_range_covers_whole_index` 钉住。

---

## 五、结论

1. **墓碑问题彻底解决**：旧库 98.75% 是垃圾，删除重建后 0 墓碑。
2. **索引体积 17.72 GB → 0.22 GB（↓98.8%）**，纯数据 407 万条目。
3. **加载 55–83 s → 0.35 s**，内存下界 21.9 GB → 0.3 GB。
4. **查询全部毫秒级**（除 `path:` 这一既有慢路径）。
5. **建库仅需 25.3 秒** —— 以后遇到索引异常，直接删了重建比压缩更快更干净。

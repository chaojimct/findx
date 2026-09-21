//! 搜索执行：RoaringBitmap 候选 → SIMD / glob / regex /（optional pinyin）

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
// 用 parking_lot::RwLock 替代 std::sync::RwLock：
// - std 在 Windows 上是 SRW Lock，writer 一旦排队，**所有后续 reader 都被阻塞**——
//   导致 backfill 高频写期间，search read lock 长时间饥饿，IPC 表现为"卡死"；
// - parking_lot 默认 reader 不会被排队中的 writer 卡住（writer 之间公平排队，
//   reader 路径快速通过），更适合"99% 读 + 后台少量写"的本场景；
// - 同时去掉 poison 处理：parking_lot::RwLock 没有 poisoning 概念，API 更清爽。
use parking_lot::RwLock;

use memchr::memmem;
use rayon::prelude::*;
use regex::bytes::Regex;
use roaring::RoaringBitmap;

use crate::index::{hash_ext8, IndexStore};
use crate::meta_overlay::MetaOverlay;
use crate::query::{ParsedQuery, SortField};
use crate::Result;

/// 元数据未就绪时，按大小/时间的排序退化为按文件名（占位 0 无意义）。
fn effective_sort_field(store: &IndexStore, q: &ParsedQuery) -> SortField {
    if store.metadata_ready {
        q.sort_by
    } else {
        match q.sort_by {
            SortField::Size | SortField::Modified | SortField::Created => SortField::Name,
            other => other,
        }
    }
}

// 关键引擎选择（参考 IbEverythingExt 的实测对比）：
// - cp::Regex：通用 fallback，cp = "character properties"，对每条 entry 的拼音匹配开销 ~1 ms。
// - lita::Regex：专为「字面 pattern + ASCII haystack」优化的 meta engine，文档原话
//   "much better performance if and only if your pattern is often a literal string"。
//   内部 enum dispatch：HirKind::Literal → IbMatcher（最快路径），ASCII haystack → dense DFA。
// findx2 的搜索 needle 99% 是字面（如 android / mct / 拼音字符串），
// 因此应当一律使用 lita。bench 数据 ~100 ns/条，8.5M × 20 thread ≈ 42 ms 可达。
#[cfg(feature = "pinyin")]
use ib_matcher::matcher::{MatchConfig, PinyinMatchConfig};
#[cfg(feature = "pinyin")]
use ib_matcher::pinyin::PinyinNotation;
#[cfg(feature = "pinyin")]
use ib_matcher::regex::lita::Regex as IbRegex;

/// 拼音匹配的触发策略（默认 Auto，与 IbEverythingExt 行为对齐）。
///
/// - `Off`：永远走字面匹配。即使搜「mct」，也只命中 `mct.txt`，不命中「马春天.txt」。
/// - `Explicit`：仅 `xxx:py` 后缀（即 `q.pinyin_only=true`）启用拼音；普通 needle 字面。
/// - `Auto`（默认）：所有非空 needle 默认启用 lita+拼音表匹配。`mct` 既能命中 `mct.txt`
///   也能命中「马春天.txt」（首字母）和「mac车天.txt」（混合）。
///   ASCII fast-path（dense DFA）保证 ms 级响应。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PinyinMatchMode {
    Off,
    Explicit,
    Auto,
}

impl Default for PinyinMatchMode {
    fn default() -> Self {
        PinyinMatchMode::Auto
    }
}

#[derive(Debug, Clone, Default)]
pub struct SearchOptions {
    /// 总开关；false 时 `pinyin_match_mode` 直接被忽略。
    /// service 端从 GUI 设置/`findx2.config.json` 读取，默认 true。
    pub allow_pinyin: bool,
    /// 拼音匹配触发策略，详见 `PinyinMatchMode`。仅在 `allow_pinyin=true` 时生效。
    pub pinyin_match_mode: PinyinMatchMode,
}

/// 顶层 build 一次的拼音 matcher 切片类型。
/// 让函数签名统一写 `pin_res: &PinList<'_>`，避免 cfg-attr 撒在每个参数上。
/// no-pinyin feature 下退化为 `[()]`（空切片）以让代码继续编译。
#[cfg(feature = "pinyin")]
pub(crate) type PinList<'a> = [IbRegex<'a>];
#[cfg(not(feature = "pinyin"))]
pub(crate) type PinList<'a> = [()];

#[derive(Debug, Clone, serde::Serialize)]
pub struct SearchHit {
    pub entry_idx: u32,
    pub name: String,
    pub path: String,
    pub size: u64,
    pub mtime: u64,
    /// 文件名中与当前查询匹配、用于 UI 高亮的 Unicode 标量字符区间 [start, end)（与搜索层 `ib_matcher`/字面逻辑一致）
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub name_highlight: Vec<[u32; 2]>,
}

/// 后台元数据回填进度（与 `SearchEngine` 共享，供 IPC Status 读取）。
#[derive(Debug)]
pub struct BackfillProgress {
    pub done: AtomicU64,
    pub total: AtomicU64,
    /// 回填被关闭 / 全部失败 / 线程异常时的说明；无问题时为 `None`。
    pub error: parking_lot::Mutex<Option<String>>,
}

impl Default for BackfillProgress {
    fn default() -> Self {
        Self {
            done: AtomicU64::new(0),
            total: AtomicU64::new(0),
            error: parking_lot::Mutex::new(None),
        }
    }
}

pub struct SearchEngine {
    store: RwLock<IndexStore>,
    backfill: Arc<BackfillProgress>,
    /// 回填元数据的紧凑 overlay（取代之前的 `DashMap`）：
    /// - 长度固定 = entry_count，按 idx 平铺 16 字节/条；
    /// - 全程**无锁**：回填线程写、search 线程读，永远不竞 IndexStore 的 RwLock；
    /// - 持久化时（`flush_metadata_overlay_into_store`）才一次性合并进主索引。
    ///
    /// 见 `crate::meta_overlay` 的模块文档了解为什么不用 DashMap。
    metadata_overlay: Arc<MetaOverlay>,
    /// 索引变更计数：任何影响搜索结果集/排序的写入都 +1（USN 增量、回填合并、
    /// metadata_ready 翻转、overlay 写入/清空）。分页查询缓存凭它失效。
    /// trigram 边车重建不 bump——剪枝只影响候选超集，不影响最终命中与排序。
    revision: AtomicU64,
    /// 分页查询缓存（仅一条：最近一次 `search_paged` 的已排序 top-K，见 `CachedTop`）。
    /// 滚动翻页（同 query 不同 offset）命中时只做切片 + 当页构建，不重扫不重排。
    /// key 含原始 query 文本 + 拼音开关（同文本同开关 ⇒ 同解析结果），
    /// revision 对不上即失效。CLI / Everything 走 `search()` 不经过这里。
    page_cache: parking_lot::Mutex<Option<CachedTop>>,
}

/// 分页缓存条目：已排序的 top-K（K 见 `PAGE_CACHE_TOP`）+ 总数。
/// 实测 select_nth 耗时对 k 几乎不敏感（86k 命中下 top-500 与 top-5000 同为 ~7.6ms，
/// 都是 O(H) 主导），因此 miss 时直接算到 8192（GUI 上限），之后全部翻页都是纯切片。
#[derive(Debug, Clone)]
struct CachedTop {
    query_text: String,
    allow_pinyin: bool,
    pinyin_mode: PinyinMatchMode,
    revision: u64,
    /// 已按查询排序的前 K 个 idx（K = min(总数, max(请求页尾, PAGE_CACHE_TOP))）。
    top: Vec<u32>,
    total: u32,
}

/// 分页缓存的排序深度：与 GUI `limit` 上限（8192）对齐，保证可滚范围内翻页永不重排。
const PAGE_CACHE_TOP: usize = 8192;

/// 当页构建的分段耗时（`FINDX2_DEBUG_SEARCH` 用）。
struct PageBuildStats {
    path_us: u128,
    hl_us: u128,
    meta_us: u128,
    build_us: u128,
}

impl SearchEngine {
    pub fn new(store: IndexStore) -> Self {
        let entry_count = store.entries.len();
        Self {
            store: RwLock::new(store),
            backfill: Arc::new(BackfillProgress::default()),
            metadata_overlay: Arc::new(MetaOverlay::new(entry_count)),
            revision: AtomicU64::new(0),
            page_cache: parking_lot::Mutex::new(None),
        }
    }

    /// 索引变更计数（见字段文档）。service 经 `index_store_mut()` 直接改索引后，
    /// 调本函数使分页缓存失效（USN flush 每批调一次即可，不必每条）。
    pub fn note_external_mutation(&self) {
        self.revision.fetch_add(1, Ordering::Relaxed);
    }

    fn bump_revision(&self) {
        self.revision.fetch_add(1, Ordering::Relaxed);
    }

    /// overlay 已回填的条数（仅用于统计/进度，search 路径用不到）。
    pub fn metadata_overlay_len(&self) -> usize {
        self.metadata_overlay.filled_count()
    }

    /// overlay 是否已有该条目（回填断点续跑：过滤已完成条目用）。
    pub fn metadata_overlay_has(&self, idx: usize) -> bool {
        self.metadata_overlay.get(idx).is_some()
    }

    /// overlay 全量快照（断点续跑落盘用；调用方保证回填线程无并发 put，或接受极小撕裂——
    /// service 侧只在单卷完成后调用，此时该卷已 quiesce）。
    pub fn metadata_overlay_snapshot(&self) -> Vec<(usize, u64, u32, u32)> {
        self.metadata_overlay.snapshot()
    }

    /// 并行回填后批量写入 overlay。**完全无锁**。
    /// 入参 `(idx, size, mtime_filetime, ctime_filetime)`——为了兼容老调用方仍给 FILETIME，
    /// 这里转成 unix 秒存进紧凑 overlay。
    pub fn extend_metadata_overlay_batch(&self, items: &[(usize, u64, u64, u64)]) {
        if items.is_empty() {
            return;
        }
        for &(idx, size, m_ft, c_ft) in items {
            let mtime = crate::index::filetime_to_unix_secs(m_ft);
            let ctime = crate::index::filetime_to_unix_secs(c_ft);
            self.metadata_overlay.put(idx, size, mtime, ctime);
        }
        // overlay 参与 size/time 过滤与排序：回填写入即视为索引变更。
        self.bump_revision();
    }

    /// 把 overlay 一次性合并进主索引（单次写锁）。**只在持久化前/服务退出时调用**——
    /// 回填阶段 search 直接读 overlay，不需要这步。
    ///
    /// 持锁时间正比于 overlay 已填条数；8.5M 全填 ~2-3 秒，但发生时一般无 search 流量
    /// （CLI 退出 / 30s 定时落盘）——影响可控。
    pub fn flush_metadata_overlay_into_store(&self) -> Result<usize> {
        let snap = self.metadata_overlay.snapshot();
        if snap.is_empty() {
            return Ok(0);
        }
        let n = snap.len();
        let mut g = self.store.write();
        for (idx, size, mtime, ctime) in &snap {
            // patch_entry_metadata 入参 (size, mtime_FILETIME, ctime_FILETIME)，转回去。
            g.patch_entry_metadata(
                *idx,
                *size,
                crate::index::unix_secs_to_filetime(*mtime),
                crate::index::unix_secs_to_filetime(*ctime),
            )?;
        }
        drop(g);
        self.bump_revision();
        // 合并完不立刻 clear——并发 backfill 可能此刻还在 put 新条目，clear 会丢数据。
        // 留给上层（确认回填彻底完成后）显式 clear。
        Ok(n)
    }

    /// 显式清空 overlay。**只在 metadata_ready 翻 true 之后**或者明确确定回填线程已停时调用。
    /// 清空改变 size/time 的可见值（回退到主索引），同样 bump revision。
    pub fn clear_metadata_overlay(&self) {
        self.metadata_overlay.clear();
        self.bump_revision();
    }

    /// `(done, total)`，回填未开始时可为 `(0,0)`。
    pub fn backfill_progress_snapshot(&self) -> (u64, u64) {
        (
            self.backfill.done.load(Ordering::Relaxed),
            self.backfill.total.load(Ordering::Relaxed),
        )
    }

    pub fn set_backfill_total(&self, total: u64) {
        self.backfill.total.store(total, Ordering::Relaxed);
        self.backfill.done.store(0, Ordering::Relaxed);
    }

    /// 回填关闭 / 失败原因；GUI 状态栏优先展示。
    pub fn set_backfill_error(&self, msg: Option<String>) {
        *self.backfill.error.lock() = msg;
    }

    pub fn backfill_error_snapshot(&self) -> Option<String> {
        self.backfill.error.lock().clone()
    }

    pub fn add_backfill_done(&self, n: u64) {
        self.backfill.done.fetch_add(n, Ordering::Relaxed);
    }

    /// 把 done 直接置为 `n`（用于回填扫描完后从"目录扫描估算量"切回"实际命中量"，
    /// 避免双重计数）。
    pub fn reset_backfill_done_to(&self, n: u64) {
        self.backfill.done.store(n, Ordering::Relaxed);
    }

    fn clear_backfill_progress(&self) {
        self.backfill.done.store(0, Ordering::Relaxed);
        self.backfill.total.store(0, Ordering::Relaxed);
        *self.backfill.error.lock() = None;
        // overlay 不在这里清——上层 flush 落盘成功后再显式 clear。
    }

    /// 批量写主索引（**只在 fast 索引建库期、单线程独占场景下使用**——例如 USN watcher 增量更新走的是另一条
    /// 单条 `patch_entry_metadata`；这里保留是为了兼容老用例，回填路径已经改走 overlay）。
    pub fn patch_entries_metadata_batch(
        &self,
        updates: &[(usize, u64, u64, u64)],
    ) -> Result<()> {
        if updates.is_empty() {
            return Ok(());
        }
        let mut g = self.store.write();
        for &(idx, size, mtime, ctime) in updates {
            g.patch_entry_metadata(idx, size, mtime, ctime)?;
        }
        drop(g);
        self.bump_revision();
        Ok(())
    }

    /// 返回 `(命中, 真实匹配总数)`：
    /// - `命中` 受 `q.limit` 截断（例如 GUI 默认 500/5000）；
    /// - `total` 是「截断与排序前」的全部匹配条目数，用于 GUI 状态栏「匹配 N 条」。
    ///   Everything 左下角显示的就是这个数；查询 `android` 全盘可能 8M+。
    pub fn search(&self, q: &ParsedQuery, opt: &SearchOptions) -> Result<(Vec<SearchHit>, u32)> {
        let store = self.store.read();
        Self::search_inner(&store, q, opt, &self.metadata_overlay)
    }

    /// 分页搜索（GUI 无限滚动 / IPC 分页用）。
    ///
    /// 语义与 `search()` 完全一致（同扫描、同排序），区别：
    /// - 只构建 `[offset, offset+limit)` 当页的 `SearchHit`（path 解析 + 高亮只做当页，
    ///   不再为 5000 条全量构建），IPC 只传当页 JSON；
    /// - 服务侧单条「已排序 top-K 缓存」：同 query 文本 + 同拼音开关 + 同 revision 时，
    ///   滚动翻页只做切片 + 当页构建，不重扫不重排。
    ///   miss（新输入）成本 ≈ `search()`（同一次 scan + 同量级 select，只是 k 取到 8192）。
    ///
    /// `query_text` 必须与 `q` 的来源一致（service 传收到的原始串），用作缓存 key；
    /// 同文本同开关 ⇒ 同解析结果（parse 确定性），revision 保证索引未变。
    /// `offset` 越界 → 空页（`total` 照常返回）。
    pub fn search_paged(
        &self,
        query_text: &str,
        q: &ParsedQuery,
        opt: &SearchOptions,
        offset: usize,
        limit: usize,
    ) -> Result<(Vec<SearchHit>, u32)> {
        // pin matcher 构建（与 search_inner 同样的一次性编译，下游扫描/高亮复用）。
        #[cfg(feature = "pinyin")]
        let pin_needles: Vec<String> = collect_pinyin_needles(q, opt);
        #[cfg(feature = "pinyin")]
        let pin_res: Vec<IbRegex<'_>> = pin_needles
            .iter()
            .map(|s| build_pinyin_matcher(s.as_str()))
            .collect::<Result<Vec<_>>>()?;
        #[cfg(feature = "pinyin")]
        let pin_res_ref: &PinList<'_> = pin_res.as_slice();
        #[cfg(not(feature = "pinyin"))]
        let pin_res_ref: &PinList<'_> = &[];

        let want_end = offset.saturating_add(limit);
        let rev_now = self.revision.load(Ordering::Relaxed);
        // 1) 缓存命中？要求缓存的已排序深度覆盖本页页尾（只读 clone，不占锁做构建）。
        let cached: Option<CachedTop> = {
            let g = self.page_cache.lock();
            match &*g {
                Some(c)
                    if c.query_text == query_text
                        && c.allow_pinyin == opt.allow_pinyin
                        && c.pinyin_mode == opt.pinyin_match_mode
                        && c.revision == rev_now
                        && c.top.len() >= want_end.min(c.total as usize) =>
                {
                    Some(c.clone())
                }
                _ => None,
            }
        };

        let (top, total) = match cached {
            Some(c) => (c.top, c.total),
            None => {
                // miss：读锁下全量扫描 + 排到 K（K 覆盖将来翻页，见 CachedTop）。
                // 读锁持有期间 writer 进不来，扫完再取 revision 打标签，
                // 保证「数据 ⇒ 标签」的时序正确。
                let store = self.store.read();
                let unordered =
                    Self::search_unordered(&store, &self.metadata_overlay, q, opt, pin_res_ref)?;
                let tag = self.revision.load(Ordering::Relaxed);
                let total = unordered.len() as u32;
                let k = want_end.max(PAGE_CACHE_TOP).min(unordered.len());
                let top = Self::sort_top_k(&store, &unordered, q, k);
                drop(unordered);
                let entry = CachedTop {
                    query_text: query_text.to_string(),
                    allow_pinyin: opt.allow_pinyin,
                    pinyin_mode: opt.pinyin_match_mode,
                    revision: tag,
                    top: top.clone(),
                    total,
                };
                *self.page_cache.lock() = Some(entry);
                (top, total)
            }
        };

        // 2) 切页 + 当页构建。
        let page: Vec<u32> = if offset >= top.len() {
            Vec::new()
        } else {
            top[offset..].iter().take(limit).copied().collect()
        };
        drop(top);
        let store = self.store.read();
        let dbg_page = std::env::var("FINDX2_DEBUG_SEARCH").is_ok();
        let (out, _) = Self::build_hit_page(
            &store,
            &page,
            q,
            opt,
            &self.metadata_overlay,
            pin_res_ref,
            dbg_page,
        )?;
        Ok((out, total))
    }

    fn search_inner(
        store: &IndexStore,
        q: &ParsedQuery,
        opt: &SearchOptions,
        overlay: &MetaOverlay,
    ) -> Result<(Vec<SearchHit>, u32)> {
        let dbg = std::env::var("FINDX2_DEBUG_SEARCH").is_ok();
        let needle_for_log: String = q.substring.clone().unwrap_or_default();

        // 顶层一次性 build 拼音 matcher 列表（如果 mode + needle 满足条件）。
        // pin_needles 必须 outlive pin_res（lita::Regex 借用 needle 字节）；
        // Rust 局部变量 drop 顺序与声明相反，故先 needles 后 res 才能让 res 借用合法。
        // 这是「IbEverythingExt 1 次 search_compile + N 次 search_exec」模型在 findx2 里的对应实现。
        #[cfg(feature = "pinyin")]
        let pin_needles: Vec<String> = collect_pinyin_needles(q, opt);
        #[cfg(feature = "pinyin")]
        let _t_pin_build = std::time::Instant::now();
        #[cfg(feature = "pinyin")]
        let pin_res: Vec<IbRegex<'_>> = pin_needles
            .iter()
            .map(|s| build_pinyin_matcher(s.as_str()))
            .collect::<Result<Vec<_>>>()?;
        #[cfg(feature = "pinyin")]
        if dbg && !pin_res.is_empty() {
            eprintln!(
                "[search-dbg] pin_compile needles={} took={:.2}ms",
                pin_res.len(),
                _t_pin_build.elapsed().as_micros() as f64 / 1000.0,
            );
        }
        #[cfg(feature = "pinyin")]
        let pin_res_ref: &PinList<'_> = pin_res.as_slice();
        #[cfg(not(feature = "pinyin"))]
        let pin_res_ref: &PinList<'_> = &[];

        let t_flat = std::time::Instant::now();
        let mut hits = Self::search_unordered(store, overlay, q, opt, pin_res_ref)?;
        let flat_us = t_flat.elapsed().as_micros();
        let total = hits.len() as u32;
        let t_fin = std::time::Instant::now();
        let out = Self::finalize_hits(store, &mut hits, q, opt, overlay, pin_res_ref)?;
        let fin_us = t_fin.elapsed().as_micros();
        if dbg {
            eprintln!(
                "[search-dbg] needle={:?} entries={} total={} returned={} flat={:.2}ms finalize={:.2}ms",
                needle_for_log,
                store.entries.len(),
                total,
                out.len(),
                flat_us as f64 / 1000.0,
                fin_us as f64 / 1000.0,
            );
        }
        Ok((out, total))
    }

    /// 无序全量命中：OR 走多分支 union，否则单分支 flat。不排序、不构建。
    /// `search()`（经 finalize 全量构建）与 `search_paged()`（分页缓存）共用。
    fn search_unordered(
        store: &IndexStore,
        overlay: &MetaOverlay,
        q: &ParsedQuery,
        opt: &SearchOptions,
        pin_res: &PinList<'_>,
    ) -> Result<Vec<u32>> {
        if !q.or_branches.is_empty() {
            Self::search_or_union(store, overlay, q, opt)
        } else {
            Self::search_flat_indices(store, overlay, q, opt, pin_res)
        }
    }

    /// OR 多分支 union（各分支独立扫描，见内联注释）。顶层 pin_res 不参与扫描
    /// （各分支按自己的 needles 预编译 matcher），只用于外层 finalize 高亮。
    fn search_or_union(
        store: &IndexStore,
        overlay: &MetaOverlay,
        q: &ParsedQuery,
        opt: &SearchOptions,
    ) -> Result<Vec<u32>> {
        // OR 分支各自独立（只读共享 store/overlay），并行扫描后 union。
        // 语义：union 后走同一排序；分支间相对顺序本来就不保证（HashSet 去重）。
        // pin matcher 按分支预编译一次（串行；构建便宜），扫描中各线程再 clone。
        let mut branch_qs: Vec<ParsedQuery> = Vec::with_capacity(1 + q.or_branches.len());
        branch_qs.push(Self::strip_or(q));
            for br in &q.or_branches {
                branch_qs.push(Self::strip_or(br));
            }
            #[cfg(feature = "pinyin")]
            let branch_needles: Vec<Vec<String>> = branch_qs
                .iter()
                .map(|b| collect_pinyin_needles(b, opt))
                .collect();
            #[cfg(feature = "pinyin")]
            let branch_matchers: Vec<Vec<IbRegex<'_>>> = branch_needles
                .iter()
                .map(|ns| {
                    ns.iter()
                        .map(|s| build_pinyin_matcher(s.as_str()))
                        .collect::<Result<Vec<_>>>()
                })
                .collect::<Result<Vec<_>>>()?;

            let per_branch: Vec<Vec<u32>> = (0..branch_qs.len())
                .into_par_iter()
                .map(|i| {
                    #[cfg(feature = "pinyin")]
                    let m: &PinList<'_> = branch_matchers[i].as_slice();
                    #[cfg(not(feature = "pinyin"))]
                    let m: &PinList<'_> = &[];
                    Self::search_flat_indices(store, overlay, &branch_qs[i], opt, m)
                })
                .collect::<Result<Vec<_>>>()?;
            let mut uni: HashSet<u32> = HashSet::new();
            for v in per_branch {
                uni.extend(v);
            }
            Ok(uni.into_iter().collect())
    }

    fn strip_or(q: &ParsedQuery) -> ParsedQuery {
        let mut c = q.clone();
        c.or_branches.clear();
        c
    }

    /// 单分支候选下标（无排序截断前）
    fn search_flat_indices(
        store: &IndexStore,
        overlay: &MetaOverlay,
        q: &ParsedQuery,
        opt: &SearchOptions,
        pin_res: &PinList<'_>,
    ) -> Result<Vec<u32>> {
        // === Fast path: ext 全集 + 普通子串/无名匹配 + 非 regex/glob
        // 把 deleted/dir/file/attr/size/time/name 6+ 次全表 retain 融成 1 次并行 filter，
        // 并直接借用 names_lower_buf 完成 case-insensitive 子串匹配，不再每条 to_ascii_lowercase。
        // 这一条路径覆盖了 GUI 99% 的实际查询。
        let no_ext_filter = q.ext_list.is_empty() && q.ext.is_none();
        let no_complex_name = q.regex_pattern.is_none() && q.glob_pattern.is_none();
        // QueryParser 对每个普通 token 同时填 substring 和 name_terms，因此 name_terms 几乎永远不为空。
        // fast path 真正等价的是「name_terms 是 substring 的同义复制」这种最常见情况：
        //   - 0 个 name_term：substring 也为 None → fast path 只做属性/大小/时间过滤；
        //   - 1 个 name_term 且等于 substring：fused_scan 用 substring 即可；
        //   - 多 name_term 全部能用作 AND 子串过滤：交给 fused_scan_multi 处理。
        // 这条判断之前漏写，导致所有简单查询都被踢去 slow path 的 6+ 次 retain，是 406 ms 的根因。
        let name_terms_compatible = match q.name_terms.len() {
            0 => true,
            1 => q
                .name_terms
                .first()
                .map(|t| Some(t.as_str()) == q.substring.as_deref())
                .unwrap_or(true),
            _ => true, // 多 term 走 fused_scan_multi 的 AND 子串路径
        };

        // 调度逻辑（按 IbEverythingExt 的 search_compile/search_exec 模型）：
        //
        //   ┌───────────────────────┐
        //   │ pin_res 非空？         │
        //   │ (Auto/Explicit 触发)   │
        //   └─────┬─────────┬───────┘
        //         │ 是      │ 否
        //         ▼         ▼
        //   fused_scan_pinyin   ┌───────────────────────────┐
        //   （lita+拼音表）     │ no_ext_filter && no_complex│
        //                       │ _name && name_terms_compat│
        //                       └─────┬─────────┬───────────┘
        //                             │ 是      │ 否
        //                             ▼         ▼
        //                          fused_scan   slow path
        //                          （字面）     (regex/glob/复杂)
        //
        // pin_res 非空意味着调用方已经决定要走拼音匹配，整张表跑预编译的 IbRegex。
        // 这是 IbEverythingExt ms 响应的关键。
        let mut hits = if !pin_res.is_empty() && no_ext_filter && no_complex_name {
            #[cfg(feature = "pinyin")]
            {
                // 拼音候选剪枝：lita 匹配要求名字含 needle 字面（trigram 可检）
                // 或含拼音字符（cjk_names 可检），二者必居其一 → 候选是命中超集。
                let cands = pinyin_candidate_ids(store, q);
                fused_scan_pinyin(store, overlay, q, pin_res, cands)
            }
            #[cfg(not(feature = "pinyin"))]
            {
                let _ = pin_res;
                fused_scan(store, overlay, q)
            }
        } else if no_ext_filter && no_complex_name && name_terms_compatible {
            // trigram 剪枝：字面 case-insensitive 查询先求交倒排候选，命中稀疏时直接省掉全表扫描。
            match trigram_candidate_ids(store, q) {
                Some(ids) => fused_scan_cand(store, overlay, q, ids),
                None => fused_scan(store, overlay, q),
            }
        } else {
            // === Slow / 复杂 path：属性过滤单遍并行（slow_attr_candidates，
            // 替代原来的 deleted/dir/size/time/attrib 6 次串行全表 retain），
            // 之后 name_match_phase（regex/glob/pinyin/name_terms）与 post 链语义不变。
            let cand = slow_attr_candidates(store, overlay, q);

            if !q.name_terms.is_empty() {
                name_match_all_terms(store, q, opt, cand, &q.name_terms)?
            } else {
                let needle_bs = q
                    .substring
                    .as_ref()
                    .map(|s| {
                        if q.case_sensitive {
                            s.as_bytes().to_vec()
                        } else {
                            s.to_ascii_lowercase().into_bytes()
                        }
                    })
                    .unwrap_or_default();
                name_match_phase(store, q, opt, cand, &needle_bs)?
            }
        };

        if q.drive.is_some() || q.path_prefix.is_some() {
            hits.retain(|&idx| {
                path_matches_drive_prefix(store, idx as usize, q.drive, q.path_prefix.as_deref())
            });
        }

        if let Some(ref needle) = q.path_match {
            let nb = needle.as_bytes();
            if q.nopath {
                hits.retain(|&idx| {
                    let nb_name = store.name_bytes(&store.entries[idx as usize]);
                    let hay = if q.case_sensitive {
                        nb_name.to_vec()
                    } else {
                        nb_name
                            .iter()
                            .map(|b| b.to_ascii_lowercase())
                            .collect::<Vec<u8>>()
                    };
                    memmem::find(&hay, nb).is_some()
                });
            } else {
                // === `path:` 过滤 ===
                //
                // 历史演进：朴素逐条父链回溯（~6.5s）→ 查表 + 候选超集两段式（~0.95s）
                // → 精确/疑似两级候选 + trigram 名字剪枝（2.4.2）。
                //
                // 两级候选的关键观察：needle 的命中要么**完整落在某组件内**（名字 memmem
                // 完整命中 / 祖先目录路径含完整 needle——前缀含之则全路径含之，可精确判定），
                // 要么**跨分隔符**（变体超集近似，需拼路径校验）。于是：
                // - `exact` 位图直接放行，第二段拼路径校验整体免掉——needle 无分隔符时
                //   `check` 恒空（`path:users` 这类高频形态直接吃满）；
                // - 只有 `check` 位图条目才做第二段。
                //
                // 路由：
                // - unix 卷 / needle 过短：候选收不紧 → 逐条校验（查表 + 空表兜底父链回溯）；
                // - hits 本来就不多（先输名字再 `path:` refine 的交互场景）：直接逐条校验，
                //   免建表 + 免全库候选扫描——为几十条命中做 414 万全扫纯浪费；
                // - hits 大：两段式（建表 + 两级候选 + 定向校验）。
                const SMALL_HITS_DIRECT: usize = 50_000;
                let unix_store = store.volumes.iter().any(|v| v.is_unix());
                if unix_store || hits.len() <= SMALL_HITS_DIRECT {
                    // 空表：`path_full_lower_with_table` 的 Windows 分支对空表项回退
                    // `resolve_dir_path_lower`（父链回溯），即朴素语义；unix 分支本就不查表。
                    let empty_table = DirPathTable {
                        buf: Vec::new(),
                        ranges: Vec::new(),
                    };
                    hits.retain(|&idx| {
                        memmem::find(
                            &path_full_lower_with_table(store, &empty_table, idx as usize, q.nowfn),
                            nb,
                        )
                        .is_some()
                    });
                } else {
                    let dir_paths = build_dir_path_table(store);
                    match path_candidate_ids(store, &dir_paths, nb) {
                        Some(cands) => {
                            hits.retain(|&idx| {
                                // 精确命中免拼路径；疑似命中才做第二段校验。
                                cands.exact.contains(idx)
                                    || (cands.check.contains(idx)
                                        && memmem::find(
                                            &path_full_lower_with_table(
                                                store,
                                                &dir_paths,
                                                idx as usize,
                                                q.nowfn,
                                            ),
                                            nb,
                                        )
                                        .is_some())
                            });
                        }
                        // 候选集退化：仍用查表版逐条校验，至少省掉父链重复回溯。
                        None => {
                            hits.retain(|&idx| {
                                let pb = path_full_lower_with_table(
                                    store,
                                    &dir_paths,
                                    idx as usize,
                                    q.nowfn,
                                );
                                memmem::find(&pb, nb).is_some()
                            });
                        }
                    }
                }
            }
        }

        if let Some(ref pp) = q.parent_path {
            // Everything：`parent:` / `infolder:` 为父目录路径**全等**（仅该文件夹的直接子项）。
            // 旧版「父路径子串」请用 `parentcontains:`。
            if q.parent_path_substring {
                hits.retain(|&idx| match_parent_path(store, idx as usize, pp));
            } else {
                hits.retain(|&idx| match_parent_path_exact(store, idx as usize, pp));
            }
        }

        if let Some(ref not_n) = q.not_substring {
            let nb = if q.case_sensitive {
                not_n.as_bytes().to_vec()
            } else {
                not_n.to_ascii_lowercase().into_bytes()
            };
            let finder = memmem::Finder::new(&nb);
            #[cfg(feature = "pinyin")]
            let pin_not = if opt.allow_pinyin && !q.no_pinyin && !q.case_sensitive {
                build_pinyin_matcher(not_n).ok()
            } else {
                None
            };
            hits.retain(|&idx| {
                let raw = store.name_bytes(&store.entries[idx as usize]);
                let nb = if q.case_sensitive {
                    CowBytes::Borrowed(raw)
                } else {
                    let lo: Vec<u8> = std::str::from_utf8(raw)
                        .map(|s| s.to_ascii_lowercase().into_bytes())
                        .unwrap_or_else(|_| raw.iter().map(|b| b.to_ascii_lowercase()).collect());
                    CowBytes::Owned(lo)
                };
                if finder.find(nb.as_ref()).is_some() {
                    return false;
                }
                #[cfg(feature = "pinyin")]
                if let Some(re) = pin_not.as_ref() {
                    if let Ok(name) = std::str::from_utf8(raw) {
                        if re.find(name).is_some() {
                            return false;
                        }
                    }
                }
                true
            });
        }

        hits = apply_post_name_filters(store, q, opt, hits)?;

        if let (Some(lo), Some(hi)) = (q.depth_min, q.depth_max) {
            hits.retain(|&idx| {
                let d = path_depth(store, idx as usize);
                d >= lo && d <= hi
            });
        } else if let Some(lo) = q.depth_min {
            hits.retain(|&idx| path_depth(store, idx as usize) >= lo);
        } else if let Some(hi) = q.depth_max {
            hits.retain(|&idx| path_depth(store, idx as usize) <= hi);
        }

        let child_cmap = if q.child_exact.is_some() || q.empty_dir.is_some() {
            Some(child_count_map(store))
        } else {
            None
        };

        if let (Some(want), Some(ref cmap)) = (q.child_exact, child_cmap.as_ref()) {
            hits.retain(|&idx| {
                let e = &store.entries[idx as usize];
                if !e.is_dir_entry() {
                    return false;
                }
                let fr = store.frns.get(idx as usize).copied().unwrap_or(0);
                *cmap.get(&fr).unwrap_or(&0) == want
            });
        }

        if let (Some(want_empty), Some(ref cmap)) = (q.empty_dir, child_cmap.as_ref()) {
            hits.retain(|&idx| {
                let e = &store.entries[idx as usize];
                if !e.is_dir_entry() {
                    return false;
                }
                let fr = store.frns.get(idx as usize).copied().unwrap_or(0);
                let n = *cmap.get(&fr).unwrap_or(&0);
                if want_empty {
                    n == 0
                } else {
                    n > 0
                }
            });
        }

        if let Some(ref dk) = q.dupe_kind {
            hits = filter_dupe(store, &hits, dk)?;
        }

        if q.content_substring.is_some() || q.utf8content_substring.is_some() {
            hits = filter_content(store, &hits, q)?;
        }

        Ok(hits)
    }

    fn finalize_hits(
        store: &IndexStore,
        hits: &mut Vec<u32>,
        q: &ParsedQuery,
        opt: &SearchOptions,
        overlay: &MetaOverlay,
        pin_res: &PinList<'_>,
    ) -> Result<Vec<SearchHit>> {
        let limit = q.limit as usize;
        if limit == 0 {
            return Ok(vec![]);
        }
        let _dbg_fin = std::env::var("FINDX2_DEBUG_SEARCH").is_ok();
        let _t_sort = std::time::Instant::now();
        let _hits_total = hits.len();
        let ordered: Vec<u32> = Self::sort_top_k(store, hits, q, limit);
        let _sort_us = _t_sort.elapsed().as_micros();
        let (out, stats) =
            Self::build_hit_page(store, &ordered, q, opt, overlay, pin_res, _dbg_fin)?;
        if _dbg_fin {
            eprintln!(
                "[finalize] sort_field={:?} hits_total={} returned={} sort={:.2}ms build={:.2}ms (path={:.2}ms hl={:.2}ms meta={:.2}ms)",
                effective_sort_field(store, q),
                _hits_total,
                out.len(),
                _sort_us as f64 / 1000.0,
                stats.build_us as f64 / 1000.0,
                stats.path_us as f64 / 1000.0,
                stats.hl_us as f64 / 1000.0,
                stats.meta_us as f64 / 1000.0,
            );
        }
        Ok(out)
    }

    /// top-k 选择 + 排序：命中集远大于 k 时先 select_nth 定位分位点（O(H)），
    /// 截断后再排前 k（O(k log k)），避免 O(H log H) 全量排序。
    /// `search()` 传 k=limit；`search_paged()` 传 k=offset+limit，切页由调用方做。
    fn sort_top_k(store: &IndexStore, hits: &[u32], q: &ParsedQuery, k: usize) -> Vec<u32> {
        if k == 0 || hits.is_empty() {
            return vec![];
        }
        let sort_by = effective_sort_field(store, q);
        // 不在建索引时做三次全局排序；此处仅对命中集排序（与原先「全局序上扫描」等价）。
        match sort_by {
            SortField::Size => {
                let v = hits;
                if v.is_empty() {
                    vec![]
                } else {
                    let mut out = v.to_vec();
                    let desc = q.sort_desc;
                    let cmp = |a: &u32, b: &u32| -> std::cmp::Ordering {
                        let sa = store.entries[*a as usize].size;
                        let sb = store.entries[*b as usize].size;
                        if desc { sb.cmp(&sa) } else { sa.cmp(&sb) }
                    };
                    select_top_k_then_sort(&mut out, k, cmp);
                    out
                }
            }
            SortField::Modified => {
                let v = hits;
                if v.is_empty() {
                    vec![]
                } else {
                    let mut out = v.to_vec();
                    let desc = q.sort_desc;
                    let cmp = |a: &u32, b: &u32| -> std::cmp::Ordering {
                        let sa = store.entries[*a as usize].mtime;
                        let sb = store.entries[*b as usize].mtime;
                        if desc { sb.cmp(&sa) } else { sa.cmp(&sb) }
                    };
                    select_top_k_then_sort(&mut out, k, cmp);
                    out
                }
            }
            SortField::Created => {
                let v = hits;
                if v.is_empty() {
                    vec![]
                } else {
                    let mut out = v.to_vec();
                    let desc = q.sort_desc;
                    let cmp = |a: &u32, b: &u32| -> std::cmp::Ordering {
                        let sa = store.entries[*a as usize].ctime;
                        let sb = store.entries[*b as usize].ctime;
                        if desc { sb.cmp(&sa) } else { sa.cmp(&sb) }
                    };
                    select_top_k_then_sort(&mut out, k, cmp);
                    out
                }
            }
            // Name 排序热路径：避免 to_string() 堆分配（之前对每条 hit 都构造 String 塞 BinaryHeap，
            // 1852 hit + limit 1000 在 release 下要 ~600ms，是 GUI 主要慢源）。
            // 改为直接用 names_buf 的借用切片做字节序比较，并用 select_nth_unstable_by 取前 k。
            SortField::Name => {
                let v = hits;
                if v.is_empty() {
                    vec![]
                } else {
                    let mut out = v.to_vec();
                    let desc = q.sort_desc;
                    let cmp = |a: &u32, b: &u32| -> std::cmp::Ordering {
                        let na = store.name_bytes(&store.entries[*a as usize]);
                        let nb = store.name_bytes(&store.entries[*b as usize]);
                        if desc {
                            nb.cmp(na)
                        } else {
                            na.cmp(nb)
                        }
                    };
                    select_top_k_then_sort(&mut out, k, cmp);
                    out
                }
            }
            // Path 排序：entry_display_path 每次调用都走父链 walk + format!，
            // 若放在 cmp 里就是 O(k log k) 次路径重建（大命中集下比 Name 排序慢一个量级）。
            // Schwartzian：路径 key 一次性算好（并行），之后只比较 String；
            // key 构建失败回退空串，与旧 cmp 里 `unwrap_or_default` 语义一致。
            SortField::Path => {
                let v = hits;
                if v.is_empty() {
                    vec![]
                } else {
                    let desc = q.sort_desc;
                    let mut keyed: Vec<(u32, String)> = v
                        .par_iter()
                        // 小命中集不切分，理由同 finalize 构建。
                        .with_min_len(1024)
                        .map(|&idx| {
                            let p = store
                                .entry_display_path(idx as usize)
                                .unwrap_or_default();
                            (idx, p)
                        })
                        .collect();
                    let mut cmp_key =
                        |a: &(u32, String), b: &(u32, String)| -> std::cmp::Ordering {
                            if desc {
                                b.1.cmp(&a.1)
                            } else {
                                a.1.cmp(&b.1)
                            }
                        };
                    if keyed.len() > k {
                        let pivot = k - 1;
                        keyed.select_nth_unstable_by(pivot, &mut cmp_key);
                        keyed.truncate(k);
                    }
                    keyed.sort_unstable_by(&mut cmp_key);
                    keyed.into_iter().map(|(idx, _)| idx).collect()
                }
            }
        }
    }

/// 当页构建：已排序的 idx 切片 → `SearchHit`（name/path/highlight/meta）。
    /// 每 hit 相互独立，并行（rayon 对 indexed 源 collect 保序）；pin matcher
    /// 每线程 clone（`map_init`），`with_min_len` 保小页不切分。
    /// 错误整体返回 Err（并行下具体哪一个先报不确定，但实践中不可达）。
    fn build_hit_page(
        store: &IndexStore,
        page: &[u32],
        q: &ParsedQuery,
        opt: &SearchOptions,
        overlay: &MetaOverlay,
        pin_res: &PinList<'_>,
        dbg: bool,
    ) -> Result<(Vec<SearchHit>, PageBuildStats)> {
        let _t_build = std::time::Instant::now();
        // 高亮 needle / Finder / 正则 per-query 预计算一次（见 PreparedHighlight），
        // 不再每 hit 重复小写 + 构建 Finder + 编译正则。
        let prepared = PreparedHighlight::build(q);
        // finalize 构建并行化：每 hit 的 name / path / highlight / meta 相互独立；
        // store 与 overlay 只读共享（fused_scan 已是同模式），rayon 对 indexed 源
        // collect 保序，返回顺序与串行循环完全一致。pin matcher 每线程 clone 一份，
        // 避免多线程共享同一 IbRegex 内部 cache pool 的锁竞争（见 build_pinyin_matcher）。
        // 错误语义：仍整体返回 Err（并行下具体哪一个先报不确定，但这些错误在实践中
        // 不可达——名字必为合法 UTF-8、下标必在界内）。
        let path_us = AtomicU64::new(0);
        let hl_us = AtomicU64::new(0);
        let meta_us = AtomicU64::new(0);
        let out: Vec<SearchHit> = page
            .into_par_iter()
            // 小命中集不切分（单线程跑，免掉 rayon 切分/合并开销；行为与串行一致）。
            // per-hit 约 1–2µs，1024 条 ≈ 1–2ms，低于此规模并行无收益。
            .with_min_len(1024)
            .map_init(
                || pin_res.to_vec(),
                |pin_local, &idx| -> Result<SearchHit> {
                    let e = &store.entries[idx as usize];
                    let name = store.name_str(e)?.to_string();
                    let name_bytes = store.name_bytes(e);
                    let pin_slice: &PinList<'_> = pin_local.as_slice();
                    let _t = if dbg {
                        Some(std::time::Instant::now())
                    } else {
                        None
                    };
                    let name_highlight =
                        highlight_name_for_query(&name, name_bytes, q, opt, pin_slice, &prepared);
                    if let Some(t) = _t {
                        hl_us.fetch_add(t.elapsed().as_micros() as u64, Ordering::Relaxed);
                    }
                    let _t = if dbg {
                        Some(std::time::Instant::now())
                    } else {
                        None
                    };
                    let (size, mtime, _) = effective_meta(overlay, store, idx);
                    if let Some(t) = _t {
                        meta_us.fetch_add(t.elapsed().as_micros() as u64, Ordering::Relaxed);
                    }
                    let _t = if dbg {
                        Some(std::time::Instant::now())
                    } else {
                        None
                    };
                    let path = store.entry_display_path(idx as usize)?;
                    if let Some(t) = _t {
                        path_us.fetch_add(t.elapsed().as_micros() as u64, Ordering::Relaxed);
                    }
                    Ok(SearchHit {
                        entry_idx: idx,
                        name,
                        path,
                        size,
                        mtime,
                        name_highlight,
                    })
                },
            )
            .collect::<Result<Vec<_>>>()?;
        let stats = PageBuildStats {
            path_us: path_us.load(Ordering::Relaxed) as u128,
            hl_us: hl_us.load(Ordering::Relaxed) as u128,
            meta_us: meta_us.load(Ordering::Relaxed) as u128,
            build_us: _t_build.elapsed().as_micros(),
        };
        Ok((out, stats))
    }

    /// FRN → entry 下标（后台 stat worker 等外部增量用；墓碑条目也会返回下标，
    /// 调用方自行决定是否跳过）。
    pub fn entry_idx_by_frn(&self, frn: u64) -> Option<usize> {
        self.store.read().frn_to_entry.get_idx(frn).map(|i| i as usize)
    }

    pub fn index_store(&self) -> parking_lot::RwLockReadGuard<'_, IndexStore> {
        self.store.read()
    }

    pub fn index_store_mut(&self) -> parking_lot::RwLockWriteGuard<'_, IndexStore> {
        self.store.write()
    }

    /// 重建 trigram 边车并热替换进内存（service 运行期调用，不重启）。
    ///
    /// 时序与正确性：
    /// 1. **读锁**内记下 `pending_before`（读锁持有期间 USN apply 等写锁，`tri_pending` 冻结）
    ///    并全量构建新 `.tri`——构建用的就是此刻的 entries / 名字，天然覆盖 pending 条目；
    /// 2. **写锁**内：加载新边车替换 `store.trigram`，再把 `pending_before` 从 `tri_pending`
    ///    里减掉。构建结束后（读锁释放、写锁取得前）USN 登记的新 pending 不在 `pending_before`
    ///    里，得以保留——新 `.tri` 不含它们的最新名字，仍需 pending 兜底。
    ///
    /// 构建耗时数秒（8.5M 条目 ~3-5s），期间 search reader 不受影响（读锁共享），
    /// USN flush 短暂阻塞（journal 持久，事件不丢只延迟）。
    pub fn rebuild_trigram_sidecar(&self, index_path: &std::path::Path) -> Result<()> {
        let pending_before = {
            let store = self.index_store();
            let pending_before = store.tri_pending.clone();
            crate::trigram::build_and_save(&store, index_path)?;
            pending_before
        };
        let new_tri = crate::trigram::TrigramIndex::load(&crate::trigram::tri_sidecar_path(
            index_path,
        ))?
        .ok_or_else(|| crate::Error::Persist("trigram 重建后加载失败".into()))?;
        {
            let mut store = self.index_store_mut();
            store.trigram = Some(Arc::new(new_tri));
            store.tri_pending -= &pending_before;
        }
        Ok(())
    }

    /// 非阻塞读取——给 Status/Health 这种"宁可拿不到也别卡 IPC 线程"的场景。
    ///
    /// parking_lot 的 try_read 比 std 还宽松：std 在 Windows SRW 上 try_read 也会被排队中的
    /// writer 拒绝；parking_lot 这里只看锁是否真的被独占持有，writer 等待中并不阻塞 reader。
    pub fn try_index_store(&self) -> Option<parking_lot::RwLockReadGuard<'_, IndexStore>> {
        self.store.try_read()
    }

    pub fn metadata_ready(&self) -> bool {
        self.store.read().metadata_ready
    }

    /// USN watcher 的单条增量更新走这里：直接改主索引（写锁极短，单条 microsecond 级），
    /// **同时**把 overlay 对应槽位也填上，保证回填阶段如果同一个 idx 被回填线程之后再覆盖
    /// 也不会用脏数据反盖 USN 的最新值（USN 写来的是当下 ground truth）。
    pub fn patch_entry_metadata(
        &self,
        idx: usize,
        size: u64,
        mtime: u64,
        ctime: u64,
    ) -> Result<()> {
        let mut g = self.store.write();
        g.patch_entry_metadata(idx, size, mtime, ctime)?;
        drop(g);
        self.bump_revision();
        self.metadata_overlay.put(
            idx,
            size,
            crate::index::filetime_to_unix_secs(mtime),
            crate::index::filetime_to_unix_secs(ctime),
        );
        Ok(())
    }

    /// 标记元数据回填已完成。`ready=true` 时会把 overlay 一次性 flush 进主索引，并清空 overlay。
    /// 这一步**只发生在回填线程结束**，外部不再有写入，因此 flush + clear 无并发风险。
    pub fn set_metadata_ready(&self, ready: bool) -> Result<()> {
        if ready {
            self.flush_metadata_overlay_into_store()?;
        }
        let mut g = self.store.write();
        g.metadata_ready = ready;
        drop(g);
        self.bump_revision();
        if ready {
            self.clear_backfill_progress();
            self.metadata_overlay.clear();
        }
        Ok(())
    }
}

#[inline]
fn effective_meta(
    overlay: &MetaOverlay,
    store: &IndexStore,
    idx: u32,
) -> (u64, u64, u64) {
    if let Some((size, mtime, ctime)) = overlay.get(idx as usize) {
        return (
            size,
            crate::index::unix_secs_to_filetime(mtime),
            crate::index::unix_secs_to_filetime(ctime),
        );
    }
    let e = &store.entries[idx as usize];
    // entries.mtime/ctime 现存 u32 秒；对外接口仍是 FILETIME u64。
    (
        e.size,
        crate::index::unix_secs_to_filetime(e.mtime),
        crate::index::unix_secs_to_filetime(e.ctime),
    )
}

/// 与 [`effective_meta`] 同源：dm:/dc: 过滤必须用「展示用的」mtime/ctime。
/// 回填阶段主索引可能仍是 USN/首遍占位，overlay 已是磁盘读出的真值；只读 store 会导致侧栏时间条件与列表日期不一致。
#[inline]
fn entry_meta_for_filter(
    overlay: &MetaOverlay,
    store: &IndexStore,
    idx: u32,
) -> (u64, u32, u32) {
    if let Some(triple) = overlay.get(idx as usize) {
        return triple;
    }
    let e = &store.entries[idx as usize];
    (e.size, e.mtime, e.ctime)
}

enum CowBytes<'a> {
    Borrowed(&'a [u8]),
    Owned(Vec<u8>),
}

impl<'a> CowBytes<'a> {
    fn as_ref(&self) -> &[u8] {
        match self {
            CowBytes::Borrowed(b) => b,
            CowBytes::Owned(v) => v.as_slice(),
        }
    }
}

/// 融合扫：单次 par_iter 把 `0..N` 范围 + deleted/dir/file/属性/大小/时间/名字子串
/// 的过滤条件全部一次过做完，避免 6+ 次全表 `retain` 各自跑 cache miss。
///
/// 关键设计：
/// - 名字 needles 取 `q.name_terms`（多 token AND）或退化为 `[q.substring]`，case-insensitive
///   时一次性预小写，循环里只做 finder.find（SIMD），不再每条 `to_ascii_lowercase` 分配。
/// - case-insensitive 直接借用 `names_lower_buf`，完全消除热路径堆分配。
/// - `store.deleted.is_empty()` 时跳过 RoaringBitmap.contains，省去 8M 次 log 查询。
/// - 用 `Vec::with_capacity` + `par_iter().fold().reduce()` 而非 `.collect()`，
///   减少 rayon 内部 LinkedList 中间结构对大批命中的合并代价。
/// 决定本次查询里哪些 needle 应当用 lita+拼音表匹配（一次编译，下游全程复用）。
///
/// 返回 `Vec<String>`（owned，让 caller 在栈上存好后传 `&[IbRegex]`），空 Vec 表示「不启用拼音」。
/// 与 fused_scan 字面快路径 AND 语义对齐：name_terms 视为多 needle AND，否则用 substring。
#[cfg(feature = "pinyin")]
fn collect_pinyin_needles(q: &ParsedQuery, opt: &SearchOptions) -> Vec<String> {
    if !opt.allow_pinyin || q.no_pinyin {
        return Vec::new();
    }
    if q.regex_pattern.is_some() || q.glob_pattern.is_some() {
        // regex/glob 自有匹配引擎，不参与拼音；拼音 needle 概念在这里没有意义。
        return Vec::new();
    }
    let trigger = match opt.pinyin_match_mode {
        PinyinMatchMode::Off => false,
        PinyinMatchMode::Explicit => q.pinyin_only,
        // Auto：参考 IbEverythingExt 默认行为——任何非空 needle 都尝试拼音。
        // lita engine 对纯 ASCII haystack 走 dense DFA（不会比字面慢多少），
        // 因此对中文文件名集合自动获得「拼音匹配中文」能力。
        PinyinMatchMode::Auto => true,
    };
    if !trigger {
        return Vec::new();
    }
    let mut out: Vec<String> = Vec::new();
    if !q.name_terms.is_empty() {
        out.extend(q.name_terms.iter().filter(|s| !s.is_empty()).cloned());
    } else if let Some(s) = q.substring.as_ref() {
        if !s.is_empty() {
            out.push(s.clone());
        }
    }
    // startwith/endwith 与 Windows 一样走拼音：`startwith:bei` 能命中「北京」
    if let Some(s) = q.starts_with.as_ref() {
        if !s.is_empty() && !out.iter().any(|x| x == s) {
            out.push(s.clone());
        }
    }
    if let Some(s) = q.ends_with.as_ref() {
        if !s.is_empty() && !out.iter().any(|x| x == s) {
            out.push(s.clone());
        }
    }
    out
}

/// 一次性编译拼音 matcher，下游 par_iter / highlight 全部复用。
///
/// 注意：lita::Regex 是 `Send + Sync + Clone`，文档建议「在每个线程上 clone 一份」以避免
/// 内部 cache pool 的 spin-lock 竞争（短 haystack + 大并发场景）。
/// 本项目已实现 per-thread clone：`fused_scan_pinyin_par` 的 fold identity 与
/// `finalize_hits` 的 `map_init` 在每线程各持一份 clone，只读共享零竞争。
#[cfg(feature = "pinyin")]
fn build_pinyin_matcher(needle: &str) -> Result<IbRegex<'_>> {
    // PinyinNotation::Ascii = 全拼（machuntian）
    // PinyinNotation::AsciiFirstLetter = 首字母简拼（mct）
    // 双拼默认关闭（用户极少用且会让拼音表更大、build 时间更长）。与 IbEverythingExt 默认一致。
    let cfg = MatchConfig::builder()
        .pinyin(PinyinMatchConfig::notations(
            PinyinNotation::Ascii | PinyinNotation::AsciiFirstLetter,
        ))
        .build();
    IbRegex::builder()
        .ib(cfg)
        .build(needle)
        .map_err(|e| crate::Error::Query(format!("拼音 matcher 构建失败: {e}")))
}

fn fused_scan(store: &IndexStore, overlay: &MetaOverlay, q: &ParsedQuery) -> Vec<u32> {
    let n = store.entries.len() as u32;
    fused_scan_par(store, overlay, q, (0..n).into_par_iter())
}

/// 候选集版本：trigram 剪枝后只在候选 idx 上跑同一套融合过滤。
/// `ids` 为空 Vec 与全表扫描语义不同（空 = 0 候选），调用方负责语义。
fn fused_scan_cand(
    store: &IndexStore,
    overlay: &MetaOverlay,
    q: &ParsedQuery,
    ids: Vec<u32>,
) -> Vec<u32> {
    fused_scan_par(store, overlay, q, ids.into_par_iter())
}

/// trigram 剪枝候选（含 pending 并入）。返回 `None` 表示不适合剪枝，应回退全表扫描。
///
/// 剪枝条件：case-insensitive（索引只覆盖 ASCII 小写字节）、纯字面路径（拼音 /
/// regex / glob / ext 走各自路径）、needle ≥3 字节、候选规模 < 全表 1/3。
///
/// starts_with / ends_with 也并入候选源：它们是 AND 过滤（`apply_post_name_filters`
/// 精确 retain），而名字以 X 开头/结尾必包含 X 的全部 trigram，交集候选仍是命中超集。
fn trigram_candidate_ids(store: &IndexStore, q: &ParsedQuery) -> Option<Vec<u32>> {
    if q.case_sensitive {
        return None;
    }
    // 选最长 needle：多 term AND 里最长的通常最稀疏，位图求交收益最大。
    let needle: Vec<u8> = {
        let mut cands: Vec<&str> = Vec::new();
        if let Some(ref s) = q.substring {
            cands.push(s);
        }
        cands.extend(q.name_terms.iter().map(|s| s.as_str()));
        if let Some(ref s) = q.starts_with {
            cands.push(s);
        }
        if let Some(ref s) = q.ends_with {
            cands.push(s);
        }
        cands
            .into_iter()
            .max_by_key(|s| s.len())
            .map(|s| s.to_ascii_lowercase().into_bytes())?
    };
    let tri = store.trigram.as_ref()?;
    let n = store.entries.len();
    if n == 0 {
        return None;
    }
    let mut bm = tri.lookup_candidates(&needle)?;
    // 候选太密（如 "the" / "ing" 这类高频 trigram）：位图收集 + 收集后遍历不如直接扫全表。
    if bm.len() as usize > n / 3 {
        return None;
    }
    bm |= &store.tri_pending;
    Some(bm.iter().collect())
}

/// needle 里不能出现的正则元字符：lita 把 needle 按正则语法解析，
/// 含元字符时字面字节不再是命中的必要条件（"a.c" 能命中 "axb"），剪枝会漏。
#[cfg(feature = "pinyin")]
const REGEX_META_BYTES: &[u8] = b"\\.*+?()[]{}|^$";

/// 拼音路径候选剪枝（Auto 模式的 GUI 热路径）。
///
/// 超集论证：lita（plain 子串 + 拼音两套匹配）命中一个名字时，二者必居其一：
/// 1. 字面命中：名字含 needle 的（ASCII 不区分大小写）字节序列 → trigram 倒排可检出；
/// 2. 拼音命中：名字至少含一个拼音字符（≥ U+2000 量级）→ `cjk_names` 位图可检出。
///
/// 故候选 = `∩ᵢ (trigram(nᵢ) ∪ cjk_names)`（多 needle AND）∪ tri_pending，恒为命中超集。
///
/// 放弃剪枝（返回 `None`，回退全表）的条件：
/// - case-sensitive（trigram 索引只存小写）、边车缺失；
/// - 任一 needle 含正则元字符（正则语义超出字面超集）；
/// - 所有 needle 都 <3 字节（无 trigram 可用）；
/// - 最终候选太密（> 全表 1/3，位图开销超过省下的扫描）。
#[cfg(feature = "pinyin")]
fn pinyin_candidate_ids(store: &IndexStore, q: &ParsedQuery) -> Option<Vec<u32>> {
    if q.case_sensitive {
        return None;
    }
    let tri = store.trigram.as_ref()?;
    let n = store.entries.len();
    if n == 0 {
        return None;
    }
    let mut needles: Vec<&str> = Vec::new();
    if let Some(ref s) = q.substring {
        needles.push(s);
    }
    needles.extend(q.name_terms.iter().map(|s| s.as_str()));
    if let Some(ref s) = q.starts_with {
        needles.push(s);
    }
    if let Some(ref s) = q.ends_with {
        needles.push(s);
    }
    if needles
        .iter()
        .any(|s| s.bytes().any(|b| REGEX_META_BYTES.contains(&b)))
    {
        return None;
    }

    let mut acc: Option<RoaringBitmap> = None;
    let mut pruned_any = false;
    for s in needles {
        let lower = s.to_ascii_lowercase().into_bytes();
        if lower.len() < 3 {
            // <3 字节无 trigram 可用：该 needle 不参与剪枝（不约束候选集）。
            continue;
        }
        pruned_any = true;
        let mut needle_cand = tri.lookup_candidates(&lower)?;
        needle_cand |= &store.cjk_names;
        acc = Some(match acc {
            None => needle_cand,
            Some(a) => a & needle_cand,
        });
    }
    if !pruned_any {
        return None;
    }
    let mut bm = acc?;
    bm |= &store.tri_pending;
    if bm.len() as usize > n / 3 {
        return None;
    }
    Some(bm.iter().collect())
}
/// 融合扫描主体：`ids` 是并行迭代器（全表 Range 或 trigram 候选 Vec），过滤逻辑完全一致。
fn fused_scan_par<PI>(store: &IndexStore, overlay: &MetaOverlay, q: &ParsedQuery, ids: PI) -> Vec<u32>
where
    PI: rayon::iter::IndexedParallelIterator<Item = u32>,
{
    // 收集 needle 列表：name_terms 是 AND 关系；fallback 到 substring。
    let needles_owned: Vec<Vec<u8>> = if !q.name_terms.is_empty() {
        q.name_terms
            .iter()
            .map(|s| {
                if q.case_sensitive {
                    s.as_bytes().to_vec()
                } else {
                    s.to_ascii_lowercase().into_bytes()
                }
            })
            .filter(|v| !v.is_empty())
            .collect()
    } else if let Some(ref s) = q.substring {
        let v = if q.case_sensitive {
            s.as_bytes().to_vec()
        } else {
            s.to_ascii_lowercase().into_bytes()
        };
        if v.is_empty() {
            Vec::new()
        } else {
            vec![v]
        }
    } else {
        Vec::new()
    };
    let finders: Vec<memmem::Finder<'static>> = needles_owned
        .iter()
        .map(|n| memmem::Finder::new(n.as_slice()).into_owned())
        .collect();

    let only_files = q.only_files;
    let only_dirs = q.only_dirs;
    let attrib_must = q.attrib_must;
    let metadata_ready = store.metadata_ready;
    let size_min = q.size_min;
    let size_max = q.size_max;
    // entries 内部存 u32 秒（v5 紧凑布局）；查询参数还是 FILETIME u64，先转 1 次再进入热循环。
    let mtime_min = q.mtime_min.map(crate::index::filetime_to_unix_secs);
    let mtime_max = q.mtime_max.map(crate::index::filetime_to_unix_secs);
    let ctime_min = q.ctime_min.map(crate::index::filetime_to_unix_secs);
    let ctime_max = q.ctime_max.map(crate::index::filetime_to_unix_secs);
    let case_sensitive = q.case_sensitive;
    // RoaringBitmap.contains 单次约 50–200 ns；空集时整个 8M 全表会浪费 0.5–1.5s。
    let check_deleted_bm = !store.deleted.is_empty();
    let n = store.entries.len() as u32;
    let any_size_filter = metadata_ready && (size_min.is_some() || size_max.is_some());
    let any_time_filter = mtime_min.is_some()
        || mtime_max.is_some()
        || ctime_min.is_some()
        || ctime_max.is_some();

    // par_iter().fold + reduce：每个线程 chunk 内 push 进一个 Vec，最后一次性 extend 到主 Vec。
    // 这比 .collect::<Vec<_>>() 在大候选 + 大命中时更稳，避免 rayon 默认中间结构反复分配。
    let _dbg = std::env::var("FINDX2_DEBUG_SEARCH").is_ok();
    let _t_par = std::time::Instant::now();
    let _n_threads = if _dbg { rayon::current_num_threads() } else { 0 };
    let result = ids
        .fold(Vec::new, |mut acc, idx| {
            let e = unsafe { store.entries.get_unchecked(idx as usize) };
            // 1) deleted（最常 false，先剪枝）
            if e.is_deleted() {
                return acc;
            }
            if check_deleted_bm && store.deleted.contains(idx) {
                return acc;
            }
            // 2) dir/file 类型
            let is_dir = e.is_dir_entry();
            if only_files && is_dir {
                return acc;
            }
            if only_dirs && !is_dir {
                return acc;
            }
            // 3) attribute mask
            if attrib_must != 0 {
                let a = e.attrs & 0xff;
                if (a & attrib_must) != attrib_must {
                    return acc;
                }
            }
            // 4) 元数据：size 仅 metadata_ready 后可信；时间戳首遍即可筛（与侧栏 dm:/dc: 一致）
            if any_size_filter {
                if let Some(v) = size_min {
                    if e.size < v {
                        return acc;
                    }
                }
                if let Some(v) = size_max {
                    if e.size > v {
                        return acc;
                    }
                }
            }
            if any_time_filter {
                let (_, mt, ct) = entry_meta_for_filter(overlay, store, idx);
                if let Some(v) = mtime_min {
                    if mt < v {
                        return acc;
                    }
                }
                if let Some(v) = mtime_max {
                    if mt > v {
                        return acc;
                    }
                }
                if let Some(v) = ctime_min {
                    if ct < v {
                        return acc;
                    }
                }
                if let Some(v) = ctime_max {
                    if ct > v {
                        return acc;
                    }
                }
            }
            // 5) 名字 substring（AND）
            // 栈缓冲做即时 ASCII 小写，无堆分配；超长名（>256B，<0.1%）兜底走 heap。
            if !finders.is_empty() {
                let mut buf = [0u8; 256];
                let lower_cow;
                let nb: &[u8] = if case_sensitive {
                    store.name_bytes(e)
                } else {
                    lower_cow = store.name_lower_into(e, &mut buf);
                    &*lower_cow
                };
                for f in finders.iter() {
                    if f.find(nb).is_none() {
                        return acc;
                    }
                }
            }
            acc.push(idx);
            acc
        })
        .reduce(Vec::new, |mut a, mut b| {
            if a.len() < b.len() {
                std::mem::swap(&mut a, &mut b);
            }
            a.extend_from_slice(&b);
            a
        });
    if _dbg {
        eprintln!(
            "[fused_scan] entries={} hits={} threads={} took={:.2}ms needles={}",
            n,
            result.len(),
            _n_threads,
            _t_par.elapsed().as_micros() as f64 / 1000.0,
            finders.len()
        );
    }
    result
}

/// 拼音版全表融合扫描：与 `fused_scan` 同语义（dir/file/attr/size/time/AND-needles），
/// 但 needle 匹配统一走预编译的 `lita::Regex`（含 `PinyinNotation::Ascii | AsciiFirstLetter`）。
///
/// 核心区别（与 IbEverythingExt 的 `search_compile`/`search_exec` 等价）：
/// 1. 整个查询只 build 一次 IbRegex（在外层 collect_pinyin_needles + build_pinyin_matcher）；
/// 2. par_iter 在 8.5M entries 上对每条调一次 `re.find(name_bytes_str)`；
/// 3. 文件名是 UTF-8（来自 `store.name_str(e)`），lita 内部 dispatch：纯 ASCII haystack
///    走 dense DFA（~50 ns），含中文走 IbMatcher 拼音表匹配（~200-500 ns）。
///
/// 多 needle（name_terms.len() > 1）走 AND：所有 IbRegex 都命中才算命中，与字面 fused_scan 对齐。
///
/// `cands`：拼音候选剪枝位（`pinyin_candidate_ids` 产出）；`None` = 全表扫描。
#[cfg(feature = "pinyin")]
fn fused_scan_pinyin(
    store: &IndexStore,
    overlay: &MetaOverlay,
    q: &ParsedQuery,
    pin_res: &[IbRegex<'_>],
    cands: Option<Vec<u32>>,
) -> Vec<u32> {
    debug_assert!(!pin_res.is_empty(), "fused_scan_pinyin 调用方必须保证 pin_res 非空");
    match cands {
        Some(ids) => fused_scan_pinyin_par(store, overlay, q, pin_res, ids.into_par_iter()),
        None => {
            let n = store.entries.len() as u32;
            fused_scan_pinyin_par(store, overlay, q, pin_res, (0..n).into_par_iter())
        }
    }
}

#[cfg(feature = "pinyin")]
fn fused_scan_pinyin_par<PI>(
    store: &IndexStore,
    overlay: &MetaOverlay,
    q: &ParsedQuery,
    pin_res: &[IbRegex<'_>],
    ids: PI,
) -> Vec<u32>
where
    PI: rayon::iter::IndexedParallelIterator<Item = u32>,
{

    let only_files = q.only_files;
    let only_dirs = q.only_dirs;
    let attrib_must = q.attrib_must;
    let metadata_ready = store.metadata_ready;
    let size_min = q.size_min;
    let size_max = q.size_max;
    // u32 秒比较，与 fused_scan 一致。
    let mtime_min = q.mtime_min.map(crate::index::filetime_to_unix_secs);
    let mtime_max = q.mtime_max.map(crate::index::filetime_to_unix_secs);
    let ctime_min = q.ctime_min.map(crate::index::filetime_to_unix_secs);
    let ctime_max = q.ctime_max.map(crate::index::filetime_to_unix_secs);
    let check_deleted_bm = !store.deleted.is_empty();
    let any_size_filter = metadata_ready && (size_min.is_some() || size_max.is_some());
    let any_time_filter = mtime_min.is_some()
        || mtime_max.is_some()
        || ctime_min.is_some()
        || ctime_max.is_some();

    let _dbg = std::env::var("FINDX2_DEBUG_SEARCH").is_ok();
    let _t_par = std::time::Instant::now();
    let _n_threads = if _dbg { rayon::current_num_threads() } else { 0 };

    // pin matcher 每线程 clone 一份：lita::Regex 内部有 cache pool，多线程共享
    // 同一实例会在短 haystack + 大并发下自旋竞争（上游文档建议 per-thread clone）。
    // fold 的 identity 在每线程（及切分）调用一次，clone 数有界；匹配语义与共享引用一致。
    let result = ids
        .fold(
            || (Vec::new(), pin_res.to_vec()),
            |(mut acc, pin_local), idx| {
            let e = unsafe { store.entries.get_unchecked(idx as usize) };
            if e.is_deleted() {
                return (acc, pin_local);
            }
            if check_deleted_bm && store.deleted.contains(idx) {
                return (acc, pin_local);
            }
            let is_dir = e.is_dir_entry();
            if only_files && is_dir {
                return (acc, pin_local);
            }
            if only_dirs && !is_dir {
                return (acc, pin_local);
            }
            if attrib_must != 0 {
                let a = e.attrs & 0xff;
                if (a & attrib_must) != attrib_must {
                    return (acc, pin_local);
                }
            }
            if any_size_filter {
                if let Some(v) = size_min {
                    if e.size < v {
                        return (acc, pin_local);
                    }
                }
                if let Some(v) = size_max {
                    if e.size > v {
                        return (acc, pin_local);
                    }
                }
            }
            if any_time_filter {
                let (_, mt, ct) = entry_meta_for_filter(overlay, store, idx);
                if let Some(v) = mtime_min {
                    if mt < v {
                        return (acc, pin_local);
                    }
                }
                if let Some(v) = mtime_max {
                    if mt > v {
                        return (acc, pin_local);
                    }
                }
                if let Some(v) = ctime_min {
                    if ct < v {
                        return (acc, pin_local);
                    }
                }
                if let Some(v) = ctime_max {
                    if ct > v {
                        return (acc, pin_local);
                    }
                }
            }
            // 名字匹配（AND）：lita::Regex 接 &str；name_str 在 UTF-8 校验失败时会用 lossy。
            // 我们的索引 name 必为合法 UTF-8（建索引时强制），unsafe 走原始字节避免重复校验。
            let name_bytes = store.name_bytes(e);
            // SAFETY：建索引阶段已保证 entry.name 是合法 UTF-8（FRN→name 通过 OS API 取的 wide string 转过来）。
            let name_str = unsafe { std::str::from_utf8_unchecked(name_bytes) };
            // 用本线程 clone 的 matcher（pin_local），不碰共享实例的内部 cache pool。
            for re in pin_local.iter() {
                if re.find(name_str).is_none() {
                    return (acc, pin_local);
                }
            }
            acc.push(idx);
            (acc, pin_local)
        })
        .reduce(
            || (Vec::new(), pin_res.to_vec()),
            |(mut a, pa), (mut b, _pb)| {
                if a.len() < b.len() {
                    std::mem::swap(&mut a, &mut b);
                }
                a.extend_from_slice(&b);
                (a, pa)
            },
        )
        .0;
    if _dbg {
        eprintln!(
            "[fused_scan_pinyin] entries={} hits={} threads={} took={:.2}ms needles={}",
            store.entries.len(),
            result.len(),
            _n_threads,
            _t_par.elapsed().as_micros() as f64 / 1000.0,
            pin_res.len()
        );
    }
    result
}

/// 文件名最后一段后缀（小写 ASCII）；无 `.` 或空后缀则 `None`。
fn entry_ext_lower_ascii(store: &IndexStore, idx: u32) -> Option<String> {
    let e = store.entries.get(idx as usize)?;
    let name = store.name_str(e).ok()?;
    let (_, ext) = name.rsplit_once('.')?;
    if ext.is_empty() {
        return None;
    }
    Some(ext.to_ascii_lowercase())
}

/// `ext_filter` 仅按 `hash_ext8` 分 256 桶，**不同扩展名会碰撞**（例如 `pdf` 与 `yml` 同为桶 21），
/// 仅靠位图会误命中；必须在候选上按真实后缀与 `ext:` 列表精确比对。
fn entry_matches_ext_candidates(store: &IndexStore, idx: u32, ext_src: &[String]) -> bool {
    let Some(got) = entry_ext_lower_ascii(store, idx) else {
        return false;
    };
    ext_src.iter().any(|want| want == &got)
}

/// ext 256 桶位图并集（粗筛：不同扩展名会碰撞，调用方须再精确校验）。
/// 返回 None = 没有任何桶命中（调用方按空候选处理）；无 ext 约束的场景不调本函数。
fn ext_union_ids(store: &IndexStore, ext_src: &[String]) -> Option<Vec<u32>> {
    let mut acc: Option<RoaringBitmap> = None;
    for ext in ext_src {
        let h = hash_ext8(&format!("x.{ext}")) as usize;
        if let Some(bm) = &store.ext_filter[h] {
            acc = Some(match acc {
                Some(mut a) => {
                    a |= bm;
                    a
                }
                None => bm.clone(),
            });
        }
    }
    acc.map(|b| b.iter().collect())
}

/// slow path 属性过滤的单遍并行实现（替代原来的 6 次串行全表 retain）。
///
/// 原链（deleted → dir/file → size → mtime/ctime ×4 → attrib）每遍都全表扫，
/// regex/ext 类查询光这几遍在 8.5M 下就几百 ms。这里按 fused_scan 的模式一次过：
/// - 有 ext 约束：起点是 ext 桶位图并集（精确后缀校验并入同一谓词——与原来
///   `initial_candidates` 先精确校验再逐项 retain 等价，同为 AND，结果集与顺序一致）；
/// - 无 ext：直接并行扫 `0..N` range，避免先 `collect` 全表下标再过滤。
///
/// 语义与原 retain 链逐条对齐（含 `metadata_ready` 门控 size、overlay 时间源、
/// deleted 位图必查）。结果顺序：ext 起点保持位图迭代序，全表起点保持下标升序
/// （par filter / fold+reduce 保序），与原来一致。
fn slow_attr_candidates(store: &IndexStore, overlay: &MetaOverlay, q: &ParsedQuery) -> Vec<u32> {
    let ext_src: Vec<String> = if !q.ext_list.is_empty() {
        q.ext_list.clone()
    } else if let Some(ref e) = q.ext {
        vec![e.clone()]
    } else {
        Vec::new()
    };
    // 时间阈值 FILETIME→unix 秒，循环外一次（与 fused_scan_par 一致）。
    let mtime_min = q.mtime_min.map(crate::index::filetime_to_unix_secs);
    let mtime_max = q.mtime_max.map(crate::index::filetime_to_unix_secs);
    let ctime_min = q.ctime_min.map(crate::index::filetime_to_unix_secs);
    let ctime_max = q.ctime_max.map(crate::index::filetime_to_unix_secs);
    let any_time_filter = mtime_min.is_some()
        || mtime_max.is_some()
        || ctime_min.is_some()
        || ctime_max.is_some();
    let metadata_ready = store.metadata_ready;

    let pred = |idx: u32| -> bool {
        let Some(e) = store.entries.get(idx as usize) else {
            return false;
        };
        // deleted（slow path 保持原语义：位图必查，不做 is_empty 短路）。
        if store.deleted.contains(idx) || e.is_deleted() {
            return false;
        }
        let is_dir = e.is_dir_entry();
        if q.only_files && is_dir {
            return false;
        }
        if q.only_dirs && !is_dir {
            return false;
        }
        if metadata_ready {
            if let Some(v) = q.size_min {
                if e.size < v {
                    return false;
                }
            }
            if let Some(v) = q.size_max {
                if e.size > v {
                    return false;
                }
            }
        }
        if any_time_filter {
            let (_, mt, ct) = entry_meta_for_filter(overlay, store, idx);
            if let Some(v) = mtime_min {
                if mt < v {
                    return false;
                }
            }
            if let Some(v) = mtime_max {
                if mt > v {
                    return false;
                }
            }
            if let Some(v) = ctime_min {
                if ct < v {
                    return false;
                }
            }
            if let Some(v) = ctime_max {
                if ct > v {
                    return false;
                }
            }
        }
        if q.attrib_must != 0 {
            let a = e.attrs & 0xff;
            if (a & q.attrib_must) != q.attrib_must {
                return false;
            }
        }
        if !ext_src.is_empty() && !entry_matches_ext_candidates(store, idx, &ext_src) {
            return false;
        }
        true
    };

    // 有 ext 约束但没有任何桶命中 → 空候选（与原 `unwrap_or_default` 一致），
    // 绝不能回退全表扫描。
    let base: Option<Vec<u32>> = if ext_src.is_empty() {
        None
    } else {
        Some(ext_union_ids(store, &ext_src).unwrap_or_default())
    };
    match base {
        Some(ids) => ids.into_par_iter().filter(|&idx| pred(idx)).collect(),
        None => {
            let n = store.entries.len() as u32;
            (0..n)
                .into_par_iter()
                .fold(Vec::new, |mut a, idx| {
                    if pred(idx) {
                        a.push(idx);
                    }
                    a
                })
                .reduce(Vec::new, |mut a, mut b| {
                    if a.len() < b.len() {
                        std::mem::swap(&mut a, &mut b);
                    }
                    a.extend_from_slice(&b);
                    a
                })
        }
    }
}

fn path_matches_drive_prefix(
    store: &IndexStore,
    entry_idx: usize,
    drive: Option<char>,
    path_prefix: Option<&str>,
) -> bool {
    let vol = store.volume_for_entry(entry_idx);
    let unix = vol.map(|v| v.is_unix()).unwrap_or(false);

    if unix {
        if let Some(pref_in) = path_prefix {
            let needle = pref_in.replace('\\', "/").to_ascii_lowercase();
            let full = store.entry_full_path_lower(entry_idx).replace('\\', "/");
            return memmem::find(full.as_bytes(), needle.as_bytes()).is_some();
        }
        return true;
    }

    let vol_letter = vol
        .map(|v| v.volume_letter as char)
        .unwrap_or('C');
    let letter = drive.unwrap_or(vol_letter).to_ascii_uppercase();

    if let Some(pref_in) = path_prefix {
        let pref = pref_in.replace('/', "\\");
        let pref_trim = pref.trim_matches('\\').to_ascii_lowercase();
        let dir_ps = dir_path_lower(store, entry_idx);
        let combined = combine_vol_path(letter, dir_ps.as_ref());
        let needle = format!("{}\\{}", letter.to_ascii_lowercase(), pref_trim).to_ascii_lowercase();
        return memmem::find(&combined, needle.as_bytes()).is_some();
    }

    if let Some(want) = drive {
        let stored = vol.map(|v| v.volume_letter).unwrap_or(b'C');
        (stored as char).to_ascii_uppercase() == want.to_ascii_uppercase()
    } else {
        true
    }
}

fn dir_path_lower<'a>(store: &'a IndexStore, entry_idx: usize) -> Cow<'a, [u8]> {
    let e = &store.entries[entry_idx];
    store.resolve_dir_path_lower(e.dir_idx)
}

fn combine_vol_path(letter: char, dir_path: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(4 + dir_path.len());
    v.push(letter.to_ascii_lowercase() as u8);
    v.push(b':');
    v.extend_from_slice(dir_path);
    v
}

fn path_key(s: &str) -> String {
    s.replace('\\', "/")
        .trim_matches('/')
        .to_ascii_lowercase()
}

/// `path:` 两段式过滤的**第一段**：把「全路径含 `needle`」的条目集合压到一个候选超集。
///
/// 返回 `Some(ids)`：升序去重的候选条目下标（**超集**，仍需第二段精确校验）。
/// 返回 `None`：无法建立有效候选（`dir_paths_buf` 未物化、或 `needle` 太短不值得），
/// 调用方回退逐条全路径校验。
///
/// ## 为什么这是一个正确的超集
///
/// 令条目 `e` 的小写全路径为 `letter` + `:` + `dir_path(e)` + `\` + `name(e)`
/// （`dir_path` 自身以 `\` 开头、不含盘符，见 [`IndexStore::resolve_dir_path_lower`]）。
/// 查询内构建的「目录下标 → 小写目录路径」扁平表。
///
/// 现代索引**刻意不物化**目录路径（与 Everything 一致，省内存），
/// `dir_paths_buf` 因此常为空，任何按路径的操作都只能沿 `dirs[].parent_idx` 现拼。
/// 但目录只有 ~55 万、条目有 ~410 万，同一目录下所有条目的目录路径**完全相同**——
/// 所以按查询建一次表，就把「每条目回溯父链」降成「每目录回溯一次」。这是
/// `path:` 从 ~6.5 s 掉到毫秒级的核心。
struct DirPathTable {
    /// 扁平池：第 `i` 个目录的路径字节，`(off, len)` 指向这里。
    buf: Vec<u8>,
    ranges: Vec<(u32, u32)>,
}

impl DirPathTable {
    #[inline]
    fn get(&self, dir_idx: u32) -> &[u8] {
        match self.ranges.get(dir_idx as usize) {
            Some(&(o, l)) if l > 0 => self
                .buf
                .get(o as usize..o as usize + l as usize)
                .unwrap_or(&[]),
            _ => &[],
        }
    }
}

/// 一次拓扑遍历，为每个目录算出小写完整前缀路径（不含盘符，`\a\b` 形式）。
///
/// 复用 `IndexStore::rebuild_dir_paths` 的既有假设：`dirs` 按拓扑序排列
/// （父目录下标 < 子目录下标），因此单遍正向扫描即可让 `out[i]` 用上已算好的
/// `out[parent_idx]`，不需要第二轮。若该假设不成立（父下标大于子下标），
/// 这一项会在本轮拿不到父路径——回退逐条沿父链拼（`path_full_lower_with_table`
/// 里对空表项做了兜底），正确性不受影响，只损失这点性能。
fn build_dir_path_table(store: &IndexStore) -> DirPathTable {
    let n = store.dirs.len();
    let mut buf: Vec<u8> = Vec::with_capacity(n * 24);
    let mut ranges: Vec<(u32, u32)> = vec![(0, 0); n];
    let mut scratch = [0u8; 256];
    for i in 0..n {
        let d = &store.dirs[i];
        let nlen = d.name_len as usize;
        let Some(nb) = store
            .names_buf
            .get(d.name_offset as usize..d.name_offset as usize + nlen)
        else {
            continue;
        };
        // 目录名小写（超长兜底走堆）。
        let own_heap: Vec<u8>;
        let own: &[u8] = if nlen <= scratch.len() {
            for (dst, &b) in scratch[..nlen].iter_mut().zip(nb.iter()) {
                *dst = b.to_ascii_lowercase();
            }
            &scratch[..nlen]
        } else {
            own_heap = nb.iter().map(|b| b.to_ascii_lowercase()).collect();
            own_heap.as_slice()
        };

        let start = buf.len();
        let parent = d.parent_idx as usize;
        if d.parent_idx != 0 && parent < i {
            // 父路径已在池中（拓扑序成立）：直接续写 `父路径 + \ + 本名`。
            // 用下标切分避免同时可变/不可变借用 `buf`。
            let (po, pl) = ranges[parent];
            if pl > 0 {
                let (po, pl) = (po as usize, pl as usize);
                if po + pl <= buf.len() {
                    buf.extend_from_within(po..po + pl);
                }
            }
        }
        if !own.is_empty() {
            buf.push(b'\\');
            buf.extend_from_slice(own);
        }
        let len = buf.len() - start;
        ranges[i] = (start as u32, len as u32);
    }
    DirPathTable { buf, ranges }
}

/// 用预建目录表取「全路径小写字节」：卷符 + 目录路径 + `\` + 小写名。
///
/// 语义与朴素做法（每条目回溯父链拼路径）一致，但目录部分查表而非回溯父链；
/// 表项缺失（空表 / 拓扑序意外）时回退到 `resolve_dir_path_lower` 的按需解析。
fn path_full_lower_with_table(
    store: &IndexStore,
    table: &DirPathTable,
    entry_idx: usize,
    nowfn: bool,
) -> Vec<u8> {
    let e = &store.entries[entry_idx];
    let name_s = store
        .name_str(e)
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_else(|_| String::from_utf8_lossy(store.name_bytes(e)).to_ascii_lowercase());
    if nowfn {
        return name_s.into_bytes();
    }
    let vol = store.volume_for_entry(entry_idx);
    if vol.map(|v| v.is_unix()).unwrap_or(false) {
        // Unix 走既有完整路径（根前缀 + 相对路径），表不参与。
        let mut full = store.entry_full_path_lower(entry_idx).into_bytes();
        for b in &mut full {
            if *b == b'\\' {
                *b = b'/';
            }
        }
        return full;
    }
    let dir_ref = table.get(e.dir_idx);
    // 表项缺失（未建表 / 拓扑序意外兜底）：回退按需父链回溯。
    // 与文档一致——小路（hits 少）正是靠「传空表」复用本函数。
    let dir_owned: Cow<'_, [u8]>;
    let dir: &[u8] = if dir_ref.is_empty() {
        dir_owned = store.resolve_dir_path_lower(e.dir_idx);
        &dir_owned
    } else {
        dir_ref
    };
    let letter = vol
        .map(|v| v.volume_letter as char)
        .unwrap_or('C')
        .to_ascii_lowercase() as u8;
    let mut full = Vec::with_capacity(4 + dir.len() + name_s.len());
    full.push(letter);
    full.push(b':');
    full.extend_from_slice(dir);
    full.push(b'\\');
    full.extend_from_slice(name_s.as_bytes());
    full
}

/// [`path_candidate_ids`] 的产出：两个互斥的候选位图。
struct PathCandidates {
    /// **精确命中**：全路径必然含 needle，第二段校验可直接放行（免拼路径）。
    exact: RoaringBitmap,
    /// **疑似命中**（超集近似残留）：仍需第二段拼全路径 memmem 校验。
    check: RoaringBitmap,
}

/// `path:` 两段式过滤的**第一段**：把「全路径含 `needle`」的条目压进两个候选位图。
///
/// 返回 `None`：needle 太短、unix 卷、候选收不紧——调用方回退逐条校验。
/// 返回 `Some(cands)`：
/// - `exact`：**精确命中**（全路径必然含 needle），第二段可直接放行，免拼路径；
/// - `check`：**疑似命中**（超集残留），仍需第二段拼全路径精确校验。
///
/// ## 精确 / 疑似的分界
///
/// 令条目 `e` 的小写全路径为 `letter` + `:` + `dir_path(e)` + `\` + `name(e)`。
/// needle 的命中只有两种来源：
///
/// - **完整落在某组件内部**：needle 是 `name(e)` 或某祖先目录的 `dir_path` 的子串
///   → 可精确判定。名字 memmem 命中完整 needle 即精确；某祖先目录的**目录路径**
///   含完整 needle，则以其为前缀的整条全路径含之，该子树全部条目精确命中。
/// - **跨越组件边界**：needle 被 `\` / `/` / `:` 切开。对每个分隔符位置 `p`，
///   `needle[..p]` 以某组件结尾、`needle[p+1..]` 是后续某组件开头。取两侧变体判定
///   「组件**含**变体」是超集近似（「以它为头/尾」的放宽）→ 只能进 `check`。
///
/// 于是 **needle 不含分隔符时 `check` 恒空**——第二段校验整体消失（`path:users`
/// 这类高频形态直接吃满）；含分隔符时也只有变体目录子树 + 名字变体命中需校验。
///
/// ## 名字侧剪枝（trigram）
///
/// 名字候选不再无脑全库小写化 + memmem：全部变体 ≥3 字节时先走 trigram 倒排
/// （`store.trigram`，含 `tri_pending`）取**并集超集**，条目侧只对位图内条目做
/// 精确分类。边车缺失 / 变体过短 / 候选过密（> 全表 1/3）时回退全库扫描（原行为）。
///
/// ## 为什么候选是正确的超集
///
/// 具体取两部分并集：
/// 1. 名字（小写后）含任一 needle 变体的条目 —— 覆盖「命中落在文件名内」；
/// 2. 目录侧含任一变体的目录（目录路径含之，或自身目录名含之），
///    其**自身与全部后代**的条目 —— 覆盖「命中落在目录路径内 / 跨目录边界 / 目录条目自身被命中」。
///
/// ⚠️ 目录侧要「目录路径」与「自身名」**分开**判定，且条目侧对**目录条目**也从
/// `e.dir_idx`（指向父目录）起步。否则 `s\alice` 命中 `C:\Users\Alice` **目录条目
/// 本身**时会被漏掉——needle 骑的是「父目录名后缀 + 分隔符 + 本目录名前缀」这条缝。
fn path_candidate_ids(
    store: &IndexStore,
    table: &DirPathTable,
    needle: &[u8],
) -> Option<PathCandidates> {
    // 单字节 needle 命中面太大，候选收不紧，走查表逐条校验更省内存。
    if needle.len() < 2 {
        return None;
    }
    // ⚠️ Unix 卷回退全量精确校验：unix 的 haystack 是 `root_prefix + dir_path + / + name`
    // （见 `path_full_lower_with_table` 的 unix 分支），而目录表只含 dir_path 部分——
    // 「needle 跨过 root_prefix 与目录路径边界」的命中（如 root_prefix 尾段 + 首目录名）
    // 不被变体覆盖，候选集会漏报。unix 库规模小、性能非热点，正确性优先直接回退。
    // （回归 `unix_path_syntax_with_pinyin` 曾抓到此处漏报全部命中。）
    if store
        .volumes
        .iter()
        .any(|v| v.is_unix())
    {
        return None;
    }

    // needle 小写化由调用方保证（`q.path_match` 已是小写）；这里不再重复转换。

    // 收集所有参与匹配的 needle 变体。
    //
    // 对 `needle` 中每个分隔符位置 `p`，任一跨越该分隔符的匹配都满足：
    //   - `needle[..p]` 以某组件「结尾」（→ 该组件含它，作为子串即可），
    //   - `needle[p+1..]` 是后续某组件的「开头」（→ 该组件含它）。
    // 所以两侧各取一段作为补充 needle，即可覆盖所有跨边界命中。
    // 变体上限 32，防超长路径导致爆炸。
    let mut needles: Vec<&[u8]> = Vec::with_capacity(8);
    needles.push(needle);
    for p in 0..needle.len() {
        if needles.len() >= 32 {
            break;
        }
        let c = needle[p];
        if c != b'\\' && c != b'/' && c != b':' {
            continue;
        }
        // 左右段**哪怕只有 1 字节也要进表**：骑缝 needle（如 `a\d` 跨 `data\deep`）
        // 的两侧段可能都只有 1 字节，丢了就漏报（两级候选等价性回归抓出）。
        // 单字节变体会放大候选面（含该字节的组件全中）——正确性优先，这种超短
        // 骑缝查询走慢路可接受；变体上限 32 仍兜底防超长 needle 爆炸。
        if p >= 1 {
            needles.push(&needle[..p]);
        }
        // 右段：needle[p+1..]（右侧组件以它为头）
        let right = &needle[p + 1..];
        if !right.is_empty() {
            needles.push(right);
        }
    }

    // --- 目录侧：两级判定 + 沿父链向下传播 ---
    //
    // dir_cls[i]：0 = UNKNOWN（自身未命中且未求值）、1 = NONE（自身与祖先均未命中）、
    // 2 = VARIANT（祖先链仅变体命中）、3 = FULL（祖先链含完整命中 → 子树全部精确）。
    // 条目侧数组直接寻址 O(1)——这是把全库一遍展开压到几毫秒的关键。
    const CLS_UNKNOWN: u8 = 0;
    const CLS_NONE: u8 = 1;
    const CLS_VARIANT: u8 = 2;
    const CLS_FULL: u8 = 3;
    let ndirs = store.dirs.len();
    let mut dir_cls: Vec<u8> = vec![CLS_UNKNOWN; ndirs];
    let mut scratch = [0u8; 256];

    // 第一遍：自身判定。目录路径含**完整** needle → FULL（其子树免校验）；
    // 否则任一变体命中路径或自身名 → VARIANT（子树需校验）。
    for di in 0..ndirs {
        let dp = table.get(di as u32);
        let d = &store.dirs[di];
        let nlen = d.name_len as usize;
        let Some(nb) = store
            .names_buf
            .get(d.name_offset as usize..d.name_offset as usize + nlen)
        else {
            continue;
        };
        let own_heap: Vec<u8>;
        let own: &[u8] = if nlen <= scratch.len() {
            for (dst, &b) in scratch[..nlen].iter_mut().zip(nb.iter()) {
                *dst = b.to_ascii_lowercase();
            }
            &scratch[..nlen]
        } else {
            own_heap = nb.iter().map(|b| b.to_ascii_lowercase()).collect();
            own_heap.as_slice()
        };
        // FULL 只认「目录路径含完整 needle」。dp 为空（拓扑意外、路径兜底不可见）时
        // 不给 FULL——此时子树路径真值未知，保守进 VARIANT 走校验，正确性优先。
        if !dp.is_empty() && memmem::find(dp, needle).is_some() {
            dir_cls[di] = CLS_FULL;
            continue;
        }
        if needles
            .iter()
            .any(|nd| memmem::find(dp, nd).is_some() || memmem::find(own, nd).is_some())
        {
            dir_cls[di] = CLS_VARIANT;
        }
    }

    // 第二遍：带记忆化的父链上溯（祖先链 verdict 向下继承）。
    // 进链节点自身均未命中（UNKNOWN 才入链），故链上共享同一 verdict——与布尔版
    // 相同的正确性论证，值域从 {0,1} 扩到 {NONE,VARIANT,FULL} 而已。
    //
    // ⚠️ 上溯规则必须与 [`IndexStore::resolve_dir_path_lower`] /
    // `build_dir_path_lower_owned` **逐字对齐**：**当前节点的 `parent_idx == 0` 即到头**
    // （把当前节点算进来，但**不**继续走到下标 0）。
    // 真实 v6 索引里 `dir[0]` 是自指的死节点（如 `$RmMetadata`，其 `parent_idx == 0`），
    // 而真正的卷根是那个**名字为空**的目录；空名拼路径时不贡献分隔符，
    // 所以卷根本身不参与匹配、`dir[0]` 更是永远访问不到。
    for di in (0..ndirs).rev() {
        if dir_cls[di] != CLS_UNKNOWN {
            continue;
        }
        // 收集自底向上的链，直到撞上已定论节点，或某节点的 parent_idx == 0。
        let mut chain: Vec<u32> = Vec::new();
        let mut cur = di as u32;
        let verdict = loop {
            match dir_cls.get(cur as usize).copied().unwrap_or(CLS_NONE) {
                CLS_FULL => break CLS_FULL,
                CLS_VARIANT => break CLS_VARIANT,
                CLS_NONE => break CLS_NONE,
                _ => {}
            }
            chain.push(cur);
            let p = store.dirs[cur as usize].parent_idx;
            // `parent_idx == 0` 是「到头」哨兵；同时防自环与越界。
            if p == 0 || p as usize >= ndirs || p == cur {
                break CLS_NONE;
            }
            cur = p;
        };
        for c in chain {
            dir_cls[c as usize] = verdict;
        }
    }

    // --- 名字侧候选源：trigram 变体并集（超集）---
    //
    // 全部变体 ≥3 字节且边车可用时，逐变体 lookup（AND 语义）后 OR 并集；
    // 任一环节不可用（边车缺失 / 变体过短 / 候选过密）→ None，回退全库扫描。
    let tri_union: Option<RoaringBitmap> = 'tri: {
        let Some(tri) = store.trigram.as_ref() else {
            break 'tri None;
        };
        if store.entries.is_empty() {
            break 'tri None;
        }
        let mut acc: Option<RoaringBitmap> = None;
        for v in &needles {
            if v.len() < 3 {
                break 'tri None;
            }
            let Some(bm) = tri.lookup_candidates(v) else {
                break 'tri None;
            };
            match &mut acc {
                Some(a) => *a |= &bm,
                None => acc = Some(bm),
            }
        }
        let acc = acc?;
        // 候选太密（如 "the" / "ing" 高频 trigram）：位图逐条 contains 的判定开销
        // 省不过几个 memmem，回退全库更直接。
        if acc.len() as usize > store.entries.len() / 3 {
            break 'tri None;
        }
        let mut acc = acc;
        acc |= &store.tri_pending;
        Some(acc)
    };

    // --- 条目侧一遍（并行）：名字分类 + 目录子树展开 ---
    //
    // 名字分类只在 tri_union 命中（或 tri 不可用=全库）时做小写化 + memmem；
    // 目录展开每条仅一次数组查表。命中即收（名字优先），重复靠最后去重。
    let n = store.entries.len();
    let (mut exact_v, mut check_v): (Vec<u32>, Vec<u32>) = (0..n as u32)
        .into_par_iter()
        .fold(
            || (Vec::new(), Vec::new()),
            |mut acc, idx| {
                let e = &store.entries[idx as usize];
                // 名字侧：tri 位图不含此条目 → 名字必不含任何变体，跳过小写化+比对。
                let in_tri = tri_union
                    .as_ref()
                    .map_or(true, |bm| bm.contains(idx));
                if in_tri {
                    let mut scratch2 = [0u8; 256];
                    let nl = store.name_lower_into(e, &mut scratch2);
                    // needles[0] 恒为完整 needle：完整命中 → 精确；仅变体命中 → 待校验。
                    if memmem::find(nl.as_ref(), needle).is_some() {
                        acc.0.push(idx);
                        return acc;
                    }
                    if needles
                        .iter()
                        .skip(1)
                        .any(|nd| memmem::find(nl.as_ref(), nd).is_some())
                    {
                        acc.1.push(idx);
                        return acc;
                    }
                }
                // 目录侧：祖先链判定。FULL → 精确；VARIANT → 待校验。
                //
                // ⚠️ 对**目录条目**也用 `e.dir_idx`（它指向父目录），**不要**换成
                // `dir_index[frn]`。因为 `path:` 的 haystack 定义是
                // `dir_path(e.dir_idx) + \ + name(e)`——目录条目也不例外
                // （见 `path_full_lower_with_table`）。`e.dir_idx` 恰好就是这条 haystack 的前半段，
                // 换成本目录自己的下标会改变语义、与朴素实现不一致。
                match dir_cls.get(e.dir_idx as usize).copied().unwrap_or(1) {
                    3 => acc.0.push(idx),
                    2 => acc.1.push(idx),
                    _ => {}
                }
                acc
            },
        )
        .reduce(
            || (Vec::new(), Vec::new()),
            |mut a, mut b| {
                if a.0.len() < b.0.len() {
                    std::mem::swap(&mut a.0, &mut b.0);
                }
                a.0.extend_from_slice(&b.0);
                if a.1.len() < b.1.len() {
                    std::mem::swap(&mut a.1, &mut b.1);
                }
                a.1.extend_from_slice(&b.1);
                a
            },
        );

    exact_v.sort_unstable();
    exact_v.dedup();
    check_v.sort_unstable();
    check_v.dedup();
    Some(PathCandidates {
        exact: exact_v.into_iter().collect(),
        check: check_v.into_iter().collect(),
    })
}

fn parent_path_haystack(store: &IndexStore, entry_idx: usize) -> String {
    let e = &store.entries[entry_idx];
    let dir = store.resolve_dir_path_lower(e.dir_idx);
    if let Some(vol) = store.volume_for_entry(entry_idx) {
        if vol.is_unix() {
            return path_key(&crate::index::compose_volume_dir_path(vol, dir.as_ref()));
        }
    }
    std::str::from_utf8(dir.as_ref())
        .map(|s| path_key(s))
        .unwrap_or_default()
}

/// `parent:` / `infolder:`：父目录路径与给定路径**精确一致**（忽略大小写、斜杠方向）。
fn match_parent_path_exact(store: &IndexStore, entry_idx: usize, parent_needle: &str) -> bool {
    parent_path_haystack(store, entry_idx) == path_key(parent_needle)
}

/// `parentcontains:`：仅在**父目录路径**（不含文件名）中做子串匹配（旧行为，非 Everything 默认）。
fn match_parent_path(store: &IndexStore, entry_idx: usize, parent_needle: &str) -> bool {
    let hay = parent_path_haystack(store, entry_idx);
    let needle = path_key(parent_needle);
    if needle.is_empty() {
        return true;
    }
    hay.contains(&needle)
}

/// 目录深度（按 `\` 分段，至少为 1）
fn path_depth(store: &IndexStore, entry_idx: usize) -> u32 {
    let b = dir_path_lower(store, entry_idx);
    if b.is_empty() {
        return 1;
    }
    b.iter().filter(|&&c| c == b'\\').count() as u32 + 1
}

/// 父目录 FRN -> 直接子项数量（文件与子目录）。
/// per-parent 计数并行（每线程局部表 + 合并；8.5M 下串行约 0.3–0.5s），
/// 之后 dir_idx→FRN 映射仍串行（dirs 量级小）。仅 child:/empty: 查询触发。
fn child_count_map(store: &IndexStore) -> HashMap<u64, u32> {
    let n = store.entries.len();
    let per_parent_idx: HashMap<u32, u32> = (0..n)
        .into_par_iter()
        .fold(HashMap::new, |mut m, i| {
            if let Some(e) = store.entries.get(i) {
                *m.entry(e.dir_idx).or_insert(0) += 1;
            }
            m
        })
        .reduce(HashMap::new, |mut a, mut b| {
            if a.len() < b.len() {
                std::mem::swap(&mut a, &mut b);
            }
            for (k, v) in b {
                *a.entry(k).or_insert(0) += v;
            }
            a
        });
    let mut out: HashMap<u64, u32> = HashMap::new();
    for (di, c) in per_parent_idx {
        if let Some(d) = store.dirs.get(di as usize) {
            out.insert(d.frn, c);
        }
    }
    out
}

fn name_match_all_terms(
    store: &IndexStore,
    q: &ParsedQuery,
    opt: &SearchOptions,
    mut candidates: Vec<u32>,
    terms: &[String],
) -> Result<Vec<u32>> {
    for term in terms {
        let nb = if q.case_sensitive {
            term.as_bytes().to_vec()
        } else {
            term.to_ascii_lowercase().into_bytes()
        };
        candidates = name_match_phase(store, q, opt, candidates, &nb)?;
        if candidates.is_empty() {
            break;
        }
    }
    Ok(candidates)
}

fn filter_dupe(store: &IndexStore, hits: &[u32], kind: &str) -> Result<Vec<u32>> {
    match kind {
        "size" | "sizedupe" | "1" | "" => {
            let mut m: HashMap<u64, Vec<u32>> = HashMap::new();
            for &i in hits {
                let sz = store.entries[i as usize].size;
                m.entry(sz).or_default().push(i);
            }
            let keep: HashSet<u32> = m
                .into_values()
                .filter(|v| v.len() > 1)
                .flat_map(|v| v.into_iter())
                .collect();
            Ok(hits
                .iter()
                .filter(|i| keep.contains(i))
                .copied()
                .collect())
        }
        _ => Ok(hits.to_vec()),
    }
}

fn filter_content(store: &IndexStore, hits: &[u32], q: &ParsedQuery) -> Result<Vec<u32>> {
    let needle_opt = q
        .content_substring
        .as_ref()
        .or(q.utf8content_substring.as_ref());
    let Some(ns) = needle_opt else {
        return Ok(hits.to_vec());
    };
    // 超过此大小的文件跳过内容搜索，避免内存爆炸和长时间阻塞
    const MAX_CONTENT_FILE_SIZE: u64 = 100 * 1024 * 1024; // 100 MB
    let low = ns.to_ascii_lowercase();
    let mut out = Vec::new();
    for &idx in hits {
        let p = store.entry_display_path(idx as usize)?;
        let path = std::path::Path::new(&p);
        // 先检查文件大小，超限直接跳过
        match std::fs::metadata(path) {
            Ok(meta) if meta.len() > MAX_CONTENT_FILE_SIZE => continue,
            Err(_) => continue,
            _ => {}
        }
        let data = std::fs::read(path).map_err(|e| crate::Error::Platform(e.to_string()))?;
        let text = String::from_utf8_lossy(&data);
        if text.to_ascii_lowercase().contains(&low) {
            out.push(idx);
        }
    }
    Ok(out)
}

fn name_match_phase(
    store: &IndexStore,
    q: &ParsedQuery,
    _opt: &SearchOptions,
    candidates: Vec<u32>,
    needle_bs: &[u8],
) -> Result<Vec<u32>> {
    let hits: Vec<u32> = if let Some(ref pat) = q.regex_pattern {
        let re = Regex::new(pat).map_err(|e| crate::Error::Query(e.to_string()))?;
        candidates
            .into_par_iter()
            .filter(|&idx| {
                let e = &store.entries[idx as usize];
                re.is_match(store.name_bytes(e))
            })
            .collect()
    } else if let Some(ref g) = q.glob_pattern {
        let pat = glob::Pattern::new(g).map_err(|e| crate::Error::Query(e.to_string()))?;
        candidates
            .into_par_iter()
            .filter(|&idx| {
                let e = &store.entries[idx as usize];
                std::str::from_utf8(store.name_bytes(e))
                    .map(|n| pat.matches_with(n, glob::MatchOptions::new()))
                    .unwrap_or(false)
            })
            .collect()
    } else if needle_bs.is_empty() {
        candidates
    } else {
        let finder = memmem::Finder::new(needle_bs);
        #[cfg(feature = "pinyin")]
        {
            let ascii_lower_only = needle_bs.iter().all(|b| b.is_ascii_lowercase());
            let use_pinyin = _opt.allow_pinyin
                && !q.no_pinyin
                && (q.pinyin_only || ascii_lower_only);
            if use_pinyin {
                let needle_str =
                    std::str::from_utf8(needle_bs).map_err(|e| crate::Error::Query(e.to_string()))?;
                let config = MatchConfig::builder()
                    .pinyin(PinyinMatchConfig::default())
                    .build();
                let ib = IbRegex::builder()
                    .ib(config)
                    .build(needle_str)
                    .map_err(|e| crate::Error::Query(format!("{e}")))?;

                candidates
                    .into_par_iter()
                    .filter(|&idx| {
                        let e = &store.entries[idx as usize];
                        let name = store.name_bytes(e);
                        if q.pinyin_only {
                            ib.find(name).is_some()
                        } else {
                            finder.find(name).is_some() || ib.find(name).is_some()
                        }
                    })
                    .collect()
            } else {
                candidates
                    .into_par_iter()
                    .filter(|&idx| {
                        let e = &store.entries[idx as usize];
                        let mut buf = [0u8; 256];
                        let lower_cow;
                        let nb: &[u8] = if q.case_sensitive {
                            store.name_bytes(e)
                        } else {
                            lower_cow = store.name_lower_into(e, &mut buf);
                            &*lower_cow
                        };
                        finder.find(nb).is_some()
                    })
                    .collect()
            }
        }
        #[cfg(not(feature = "pinyin"))]
        {
            candidates
                .into_par_iter()
                .filter(|&idx| {
                    let e = &store.entries[idx as usize];
                    let mut buf = [0u8; 256];
                    let lower_cow;
                    let nb: &[u8] = if q.case_sensitive {
                        store.name_bytes(e)
                    } else {
                        lower_cow = store.name_lower_into(e, &mut buf);
                        &*lower_cow
                    };
                    finder.find(nb).is_some()
                })
                .collect()
        }
    };
    Ok(hits)
}

fn apply_post_name_filters(
    store: &IndexStore,
    q: &ParsedQuery,
    opt: &SearchOptions,
    mut hits: Vec<u32>,
) -> Result<Vec<u32>> {
    #[cfg(feature = "pinyin")]
    let pin_start = if opt.allow_pinyin && !q.no_pinyin {
        q.starts_with
            .as_deref()
            .filter(|s| !s.is_empty())
            .map(build_pinyin_matcher)
            .transpose()?
    } else {
        None
    };
    #[cfg(feature = "pinyin")]
    let pin_end = if opt.allow_pinyin && !q.no_pinyin {
        q.ends_with
            .as_deref()
            .filter(|s| !s.is_empty())
            .map(build_pinyin_matcher)
            .transpose()?
    } else {
        None
    };
    #[cfg(not(feature = "pinyin"))]
    let _ = opt;

    if let Some(ref sw) = q.starts_with {
        let swb = sw.as_bytes();
        hits.retain(|&idx| {
            let n = store.name_bytes(&store.entries[idx as usize]);
            if q.case_sensitive {
                return n.starts_with(swb);
            }
            let lit = std::str::from_utf8(n)
                .map(|s| s.to_ascii_lowercase().starts_with(sw))
                .unwrap_or(false);
            if lit {
                return true;
            }
            #[cfg(feature = "pinyin")]
            if let Some(re) = pin_start.as_ref() {
                if let Ok(name) = std::str::from_utf8(n) {
                    if let Some(m) = re.find(name) {
                        return m.start() == 0;
                    }
                }
            }
            false
        });
    }
    if let Some(ref ew) = q.ends_with {
        hits.retain(|&idx| {
            let n = store.name_bytes(&store.entries[idx as usize]);
            if q.case_sensitive {
                return n.ends_with(ew.as_bytes());
            }
            let lit = std::str::from_utf8(n)
                .map(|s| s.to_ascii_lowercase().ends_with(ew.as_str()))
                .unwrap_or(false);
            if lit {
                return true;
            }
            #[cfg(feature = "pinyin")]
            if let Some(re) = pin_end.as_ref() {
                if let Ok(name) = std::str::from_utf8(n) {
                    if let Some(m) = re.find(name) {
                        return m.end() == name.len();
                    }
                }
            }
            false
        });
    }
    if q.len_min.is_some() || q.len_max.is_some() {
        hits.retain(|&idx| {
            let n = store.name_bytes(&store.entries[idx as usize]);
            let ok = std::str::from_utf8(n).map(|s| s.chars().count()).unwrap_or(0) as u32;
            let mut ok2 = true;
            if let Some(lo) = q.len_min {
                ok2 &= ok >= lo;
            }
            if let Some(hi) = q.len_max {
                ok2 &= ok <= hi;
            }
            ok2
        });
    }

    if q.whole_filename {
        let sub = q.substring.clone().unwrap_or_default();
        hits.retain(|&idx| {
            let n = store.name_bytes(&store.entries[idx as usize]);
            let need = needle_bytes_for_compare(q, &sub);
            n == need.as_slice()
        });
    } else if q.whole_word {
        let needle = q.substring.clone().unwrap_or_default();
        hits.retain(|&idx| {
            let n = store.name_bytes(&store.entries[idx as usize]);
            whole_word_match_bytes(n, &needle, q.case_sensitive)
        });
    }

    Ok(hits)
}

fn needle_bytes_for_compare(q: &ParsedQuery, s: &str) -> Vec<u8> {
    if q.case_sensitive {
        s.as_bytes().to_vec()
    } else {
        s.to_ascii_lowercase().into_bytes()
    }
}

fn whole_word_match_bytes(hay: &[u8], word: &str, case_sensitive: bool) -> bool {
    let w = if case_sensitive {
        word.as_bytes().to_vec()
    } else {
        word.to_ascii_lowercase().into_bytes()
    };
    if w.is_empty() {
        return true;
    }
    let h: Vec<u8> = if case_sensitive {
        hay.to_vec()
    } else {
        std::str::from_utf8(hay)
            .map(|s| s.to_ascii_lowercase().into_bytes())
            .unwrap_or_else(|_| hay.iter().map(|b| b.to_ascii_lowercase()).collect())
    };
    let finder = memmem::Finder::new(&w);
    let mut search_at = 0usize;
    while search_at <= h.len().saturating_sub(w.len()) {
        let rest = &h[search_at..];
        let Some(rel) = finder.find(rest) else {
            break;
        };
        let i = search_at + rel;
        let before_ok = i == 0 || !is_word_char(h[i - 1]);
        let after_idx = i + w.len();
        let after_ok = after_idx >= h.len() || !is_word_char(h[after_idx]);
        if before_ok && after_ok {
            return true;
        }
        search_at = i + 1;
    }
    false
}

fn is_word_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// 通用 top-k：v.len() > k 时 select_nth_unstable 把第 k-1 个分位点定位（O(N)），截断后再 sort_unstable（O(k log k)）。
/// 比 BinaryHeap<(Key, idx)> 的 O(N log k) 在小 k 上常数小得多，且无堆分配（cmp 直接借用 entries）。
#[inline]
fn select_top_k_then_sort<F>(v: &mut Vec<u32>, k: usize, mut cmp: F)
where
    F: FnMut(&u32, &u32) -> std::cmp::Ordering,
{
    if k == 0 {
        v.clear();
        return;
    }
    if v.len() > k {
        let pivot = k - 1;
        v.select_nth_unstable_by(pivot, &mut cmp);
        v.truncate(k);
    }
    v.sort_unstable_by(&mut cmp);
}

/// finalize 阶段一次性备好的高亮 needle（per-query 构建一次，per-hit 只读共享）。
///
/// 背景：`highlight_name_for_query` 曾在每个 hit 上重复做三件事——needle 小写分配
/// （`to_ascii_lowercase().into_bytes()`）、`memmem::Finder::new` 构建、正则查询下
/// `Regex::new` 编译。limit=5000 时就是 5000×N 次重复，占 build 阶段约一半。
/// 此结构把它们全部提到循环外；行为与之前逐 hit 现算完全一致：
/// - `terms` 与 `q.name_terms`（或退化 `[q.substring]`）**下标一一对齐**（含空串占位），
///   保证 `pin_res[i]` 的拼音 matcher 映射不变；
/// - 空 needle 跳过（旧 `needle_byte_ranges_for_name_match` 对空串返回空区间）；
/// - 正则编译失败时为 None → 无高亮（旧代码 `if let Ok(re)` 同语义；且搜索阶段
///   早已对非法正则报错返回，走到 finalize 的正则必合法）。
struct PreparedHighlight {
    terms: Vec<Option<HighlightTerm>>,
    regex: Option<Regex>,
}

struct HighlightTerm {
    needle: Vec<u8>,
    finder: memmem::Finder<'static>,
}

impl PreparedHighlight {
    fn build(q: &ParsedQuery) -> Self {
        let mut terms = Vec::new();
        if q.regex_pattern.is_none() && q.glob_pattern.is_none() {
            let mut push = |s: &str| {
                let needle = if q.case_sensitive {
                    s.as_bytes().to_vec()
                } else {
                    s.to_ascii_lowercase().into_bytes()
                };
                terms.push(if needle.is_empty() {
                    None
                } else {
                    Some(HighlightTerm {
                        finder: memmem::Finder::new(&needle).into_owned(),
                        needle,
                    })
                });
            };
            if !q.name_terms.is_empty() {
                for t in &q.name_terms {
                    push(t);
                }
            } else if let Some(ref s) = q.substring {
                push(s);
            }
        }
        let regex = q
            .regex_pattern
            .as_ref()
            .and_then(|pat| Regex::new(pat).ok());
        Self { terms, regex }
    }
}

/// 与 `name_match_phase` 使用同一套字面 / `ib_matcher` 拼音规则，生成文件名高亮区间（UTF-8 字节 → Unicode 字符下标）。
///
/// `pin_res` 与 `q.name_terms`（或 `[q.substring]`）一一对应：第 i 个 needle 对应 `pin_res[i]`。
/// caller（`finalize_hits` 上游）已预编译，因此本函数 per-hit 调用 0 次 build——这是 IbEverythingExt
/// 实现 ms 级响应的核心，与 fused_scan_pinyin 共享同一份 IbRegex 实例。
fn highlight_name_for_query(
    name: &str,
    name_bytes: &[u8],
    q: &ParsedQuery,
    opt: &SearchOptions,
    pin_res: &PinList<'_>,
    prep: &PreparedHighlight,
) -> Vec<[u32; 2]> {
    let mut byte_ranges: Vec<(usize, usize)> = Vec::new();
    if q.regex_pattern.is_some() {
        // 正则只编译一次（prep）；旧代码每 hit `Regex::new` 一次。
        if let Some(ref re) = prep.regex {
            if let Some(m) = re.find(name_bytes) {
                byte_ranges.push((m.start(), m.end()));
            }
        }
        return byte_ranges_to_char_ranges_merged(name, byte_ranges);
    }
    if q.glob_pattern.is_some() {
        return vec![];
    }
    // prep.terms 与 name_terms / [substring] 下标对齐，pin_res[i] 映射与旧代码一致。
    for (i, slot) in prep.terms.iter().enumerate() {
        let Some(term) = slot else {
            continue;
        };
        #[cfg(feature = "pinyin")]
        let pin_re: Option<&IbRegexUnit<'_>> = pin_res.get(i);
        #[cfg(not(feature = "pinyin"))]
        let pin_re: Option<&IbRegexUnit<'_>> = {
            let _ = pin_res;
            let _ = i;
            None
        };
        byte_ranges.extend(needle_byte_ranges_for_name_match(
            name, name_bytes, term, q, opt, pin_re,
        ));
    }
    byte_ranges_to_char_ranges_merged(name, byte_ranges)
}

fn byte_ranges_to_char_ranges_merged(name: &str, byte_ranges: Vec<(usize, usize)>) -> Vec<[u32; 2]> {
    let merged = merge_byte_ranges(byte_ranges);
    let mut ranges: Vec<(u32, u32)> = merged
        .into_iter()
        .filter_map(|(a, b)| byte_range_to_char_range_pair(name, a, b))
        .collect();
    merge_u32_ranges(&mut ranges)
}

fn byte_range_to_char_range_pair(name: &str, start_b: usize, end_b: usize) -> Option<(u32, u32)> {
    if start_b > end_b || end_b > name.len() {
        return None;
    }
    Some((
        name[..start_b].chars().count() as u32,
        name[..end_b].chars().count() as u32,
    ))
}

fn merge_u32_ranges(ranges: &mut Vec<(u32, u32)>) -> Vec<[u32; 2]> {
    if ranges.is_empty() {
        return vec![];
    }
    ranges.sort_by_key(|x| x.0);
    let mut out: Vec<[u32; 2]> = Vec::new();
    for &(s, e) in ranges.iter() {
        if let Some(prev) = out.last_mut() {
            if s <= prev[1] {
                prev[1] = prev[1].max(e);
            } else {
                out.push([s, e]);
            }
        } else {
            out.push([s, e]);
        }
    }
    out
}

fn merge_byte_ranges(mut v: Vec<(usize, usize)>) -> Vec<(usize, usize)> {
    if v.is_empty() {
        return vec![];
    }
    v.sort_by_key(|x| x.0);
    let mut out: Vec<(usize, usize)> = Vec::new();
    for (s, e) in v {
        if let Some(last) = out.last_mut() {
            if s <= last.1 {
                last.1 = last.1.max(e);
            } else {
                out.push((s, e));
            }
        } else {
            out.push((s, e));
        }
    }
    out
}

fn find_all_literal_substrings(haystack: &[u8], term: &HighlightTerm) -> Vec<(usize, usize)> {
    if term.needle.is_empty() {
        return vec![];
    }
    let mut out = Vec::new();
    let mut pos = 0usize;
    while pos < haystack.len() {
        let slice = &haystack[pos..];
        if let Some(off) = term.finder.find(slice) {
            let start = pos + off;
            let end = start + term.needle.len();
            out.push((start, end));
            pos = start + 1;
        } else {
            break;
        }
    }
    out
}

/// `pin_re`：如果非 None，调用方已为本 needle 预编译好 `lita::Regex`。
/// 复用之，避免 per-hit 重新 build（这是历史 466ms/500hit 的瓶颈根因）。
///
/// 注意：lita::Regex 没有 `find_iter`（文档明示 limitation），但 highlight 通常只需要第一个匹配区间；
/// 字面 substring 部分仍用 SIMD 的 `memmem::Finder` 找全部出现，与 fused_scan 命中位置语义一致。
fn needle_byte_ranges_for_name_match(
    name: &str,
    name_bytes: &[u8],
    term: &HighlightTerm,
    q: &ParsedQuery,
    #[allow(unused_variables)]
    opt: &SearchOptions,
    #[cfg_attr(not(feature = "pinyin"), allow(unused_variables))]
    pin_re: Option<&IbRegexUnit<'_>>,
) -> Vec<(usize, usize)> {
    if term.needle.is_empty() {
        return vec![];
    }
    if q.regex_pattern.is_some() || q.glob_pattern.is_some() {
        return vec![];
    }
    #[cfg(feature = "pinyin")]
    {
        // 复用调用方预编译的 IbRegex；per-hit 0 次 build。
        // pin_re=None 即 caller 决定本次查询不启用拼音匹配（mode=Off / no_pinyin / 等）。
        if let Some(re) = pin_re {
            // SAFETY：name_bytes 来自 store.name_bytes，建索引时已校验为合法 UTF-8。
            let name_str = unsafe { std::str::from_utf8_unchecked(name_bytes) };
            let mut ranges = Vec::new();
            // 1) 字面子串：在「混合 needle」（英文 + 拼音首字母）下确保字面命中也被高亮。
            //    fused_scan_pinyin 已经命中本条，所以一定有至少一个匹配区间（字面或拼音）。
            ranges.extend(find_all_literal_substrings(name_bytes, term));
            // 2) lita 的拼音/混合匹配区间：只有 1 个 leftmost match（lita 不暴露 find_iter）。
            //    对中文文件名「马春天」+ needle "mct"，lita.find 会返回整段 "马春天" 的字节区间。
            if let Some(m) = re.find(name_str) {
                if m.end() > m.start() {
                    ranges.push((m.start(), m.end()));
                }
            }
            return merge_byte_ranges(ranges);
        }
    }
    let mut ranges = Vec::new();
    if q.case_sensitive {
        ranges.extend(find_all_literal_substrings(name_bytes, term));
    } else {
        let h: Vec<u8> = name.to_ascii_lowercase().into_bytes();
        ranges.extend(find_all_literal_substrings(&h, term));
    }
    merge_byte_ranges(ranges)
}

/// 让函数签名在 cfg pinyin / no-pinyin 都能写 `Option<&IbRegexUnit<'_>>`。
/// pinyin feature 启用时 = 真正的 lita::Regex；关闭时 = 占位单元类型（永远拿不到 Some）。
#[cfg(feature = "pinyin")]
pub(crate) type IbRegexUnit<'a> = IbRegex<'a>;
#[cfg(not(feature = "pinyin"))]
pub(crate) struct IbRegexUnit<'a>(std::marker::PhantomData<&'a ()>);

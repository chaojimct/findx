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
}

impl Default for BackfillProgress {
    fn default() -> Self {
        Self {
            done: AtomicU64::new(0),
            total: AtomicU64::new(0),
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
}

impl SearchEngine {
    pub fn new(store: IndexStore) -> Self {
        let entry_count = store.entries.len();
        Self {
            store: RwLock::new(store),
            backfill: Arc::new(BackfillProgress::default()),
            metadata_overlay: Arc::new(MetaOverlay::new(entry_count)),
        }
    }

    /// overlay 已回填的条数（仅用于统计/进度，search 路径用不到）。
    pub fn metadata_overlay_len(&self) -> usize {
        self.metadata_overlay.filled_count()
    }

    /// 并行回填后批量写入 overlay。**完全无锁**。
    /// 入参 `(idx, size, mtime_filetime, ctime_filetime)`——为了兼容老调用方仍给 FILETIME，
    /// 这里转成 unix 秒存进紧凑 overlay。
    pub fn extend_metadata_overlay_batch(&self, items: &[(usize, u64, u64, u64)]) {
        for &(idx, size, m_ft, c_ft) in items {
            let mtime = crate::index::filetime_to_unix_secs(m_ft);
            let ctime = crate::index::filetime_to_unix_secs(c_ft);
            self.metadata_overlay.put(idx, size, mtime, ctime);
        }
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
        // 合并完不立刻 clear——并发 backfill 可能此刻还在 put 新条目，clear 会丢数据。
        // 留给上层（确认回填彻底完成后）显式 clear。
        Ok(n)
    }

    /// 显式清空 overlay。**只在 metadata_ready 翻 true 之后**或者明确确定回填线程已停时调用。
    pub fn clear_metadata_overlay(&self) {
        self.metadata_overlay.clear();
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

    fn search_inner(
        store: &IndexStore,
        q: &ParsedQuery,
        opt: &SearchOptions,
        overlay: &MetaOverlay,
    ) -> Result<(Vec<SearchHit>, u32)> {
        if q.atime_min.is_some() || q.atime_max.is_some() {
            return Err(crate::Error::Query(
                "当前索引未存储文件访问时间，无法使用 da: / dateaccessed:".into(),
            ));
        }

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

        if !q.or_branches.is_empty() {
            // OR 分支需各自 build pin_res：每个 branch 的 name_terms / substring 互相独立，
            // 共用顶层 needles 会让某一支拿不到自己的拼音 matcher（如 `beijing | english`
            // 中 english 分支需要 english 自己的 lita::Regex 才能命中纯英文文件名）。
            let stripped_top = Self::strip_or(q);
            #[cfg(feature = "pinyin")]
            let top_needles: Vec<String> = collect_pinyin_needles(&stripped_top, opt);
            #[cfg(feature = "pinyin")]
            let top_res: Vec<IbRegex<'_>> = top_needles
                .iter()
                .map(|s| build_pinyin_matcher(s.as_str()))
                .collect::<Result<Vec<_>>>()?;
            #[cfg(feature = "pinyin")]
            let top_ref: &PinList<'_> = top_res.as_slice();
            #[cfg(not(feature = "pinyin"))]
            let top_ref: &PinList<'_> = &[];

            let mut uni: HashSet<u32> = HashSet::new();
            uni.extend(Self::search_flat_indices(
                store,
                overlay,
                &stripped_top,
                opt,
                top_ref,
            )?);

            for br in &q.or_branches {
                let stripped = Self::strip_or(br);
                #[cfg(feature = "pinyin")]
                let br_needles: Vec<String> = collect_pinyin_needles(&stripped, opt);
                #[cfg(feature = "pinyin")]
                let br_res: Vec<IbRegex<'_>> = br_needles
                    .iter()
                    .map(|s| build_pinyin_matcher(s.as_str()))
                    .collect::<Result<Vec<_>>>()?;
                #[cfg(feature = "pinyin")]
                let br_ref: &PinList<'_> = br_res.as_slice();
                #[cfg(not(feature = "pinyin"))]
                let br_ref: &PinList<'_> = &[];
                uni.extend(Self::search_flat_indices(
                    store,
                    overlay,
                    &stripped,
                    opt,
                    br_ref,
                )?);
            }
            let mut hits: Vec<u32> = uni.into_iter().collect();
            let total = hits.len() as u32;
            // finalize 用顶层 q 的 pin_res 高亮即可：高亮失败只是不上色，不影响命中正确性。
            let out = Self::finalize_hits(store, &mut hits, q, opt, overlay, pin_res_ref)?;
            return Ok((out, total));
        }

        let t_flat = std::time::Instant::now();
        let mut hits = Self::search_flat_indices(store, overlay, q, opt, pin_res_ref)?;
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
        let _no_complex_for_log = no_complex_name;
        let mut hits = if !pin_res.is_empty() && no_ext_filter && no_complex_name {
            #[cfg(feature = "pinyin")]
            {
                fused_scan_pinyin(store, overlay, q, pin_res)
            }
            #[cfg(not(feature = "pinyin"))]
            {
                let _ = pin_res;
                fused_scan(store, overlay, q)
            }
        } else if no_ext_filter && no_complex_name && name_terms_compatible {
            fused_scan(store, overlay, q)
        } else {
            // === Slow / 复杂 path：保留原 retain 链 + name_match_phase（regex/glob/pinyin/name_terms 等）
            let mut cand = initial_candidates(store, q);

            cand.retain(|&idx| {
                let e = &store.entries[idx as usize];
                !store.deleted.contains(idx) && !e.is_deleted()
            });

            if q.only_files {
                cand.retain(|&idx| !store.entries[idx as usize].is_dir_entry());
            }
            if q.only_dirs {
                cand.retain(|&idx| store.entries[idx as usize].is_dir_entry());
            }

            // 快速首遍：文件 size 多为占位 0，仅在后端标记 metadata_ready 后才做大小过滤。
            // 修改/创建时间：USN 首遍与 MFT 扫描对多数条目已有秒级时间，应始终参与过滤（与 GUI dm: 一致）。
            if store.metadata_ready {
                if let Some(min) = q.size_min {
                    cand.retain(|&idx| store.entries[idx as usize].size >= min);
                }
                if let Some(max) = q.size_max {
                    cand.retain(|&idx| store.entries[idx as usize].size <= max);
                }
            }
            if let Some(min) = q.mtime_min.map(crate::index::filetime_to_unix_secs) {
                cand.retain(|&idx| {
                    let (_, mt, _) = entry_meta_for_filter(overlay, store, idx);
                    mt >= min
                });
            }
            if let Some(max) = q.mtime_max.map(crate::index::filetime_to_unix_secs) {
                cand.retain(|&idx| {
                    let (_, mt, _) = entry_meta_for_filter(overlay, store, idx);
                    mt <= max
                });
            }
            if let Some(min) = q.ctime_min.map(crate::index::filetime_to_unix_secs) {
                cand.retain(|&idx| {
                    let (_, _, ct) = entry_meta_for_filter(overlay, store, idx);
                    ct >= min
                });
            }
            if let Some(max) = q.ctime_max.map(crate::index::filetime_to_unix_secs) {
                cand.retain(|&idx| {
                    let (_, _, ct) = entry_meta_for_filter(overlay, store, idx);
                    ct <= max
                });
            }

            if q.attrib_must != 0 {
                let am = q.attrib_must;
                cand.retain(|&idx| {
                    let a = store.entries[idx as usize].attrs & 0xff;
                    (a & am) == am
                });
            }

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
                hits.retain(|&idx| {
                    let pb = path_full_lower(store, idx as usize, q.nowfn);
                    memmem::find(&pb, nb).is_some()
                });
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
            hits.retain(|&idx| {
                let nb = store.name_bytes(&store.entries[idx as usize]);
                let nb = if q.case_sensitive {
                    CowBytes::Borrowed(nb)
                } else {
                    let lo: Vec<u8> = std::str::from_utf8(nb)
                        .map(|s| s.to_ascii_lowercase().into_bytes())
                        .unwrap_or_else(|_| nb.iter().map(|b| b.to_ascii_lowercase()).collect());
                    CowBytes::Owned(lo)
                };
                finder.find(nb.as_ref()).is_none()
            });
        }

        hits = apply_post_name_filters(store, q, hits)?;

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
        let sort_by = effective_sort_field(store, q);
        let _dbg_fin = std::env::var("FINDX2_DEBUG_SEARCH").is_ok();
        let _t_sort = std::time::Instant::now();
        let _hits_total = hits.len();
        // 不在建索引时做三次全局排序；此处仅对命中集排序（与原先「全局序上扫描」等价）。
        // 命中集远大于 limit 时，用堆只保留前 limit 条，避免 O(n log n) 全量排序。
        let ordered: Vec<u32> = match sort_by {
            SortField::Size => {
                let v = hits.as_slice();
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
                    select_top_k_then_sort(&mut out, limit, cmp);
                    out
                }
            }
            SortField::Modified => {
                let v = hits.as_slice();
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
                    select_top_k_then_sort(&mut out, limit, cmp);
                    out
                }
            }
            SortField::Created => {
                let v = hits.as_slice();
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
                    select_top_k_then_sort(&mut out, limit, cmp);
                    out
                }
            }
            // Name 排序热路径：避免 to_string() 堆分配（之前对每条 hit 都构造 String 塞 BinaryHeap，
            // 1852 hit + limit 1000 在 release 下要 ~600ms，是 GUI 主要慢源）。
            // 改为直接用 names_buf 的借用切片做字节序比较，并用 select_nth_unstable_by 取前 k。
            SortField::Name => {
                let v = hits.as_slice();
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
                    select_top_k_then_sort(&mut out, limit, cmp);
                    out
                }
            }
            // Path 排序：entry_display_path 返回 owned String 没法借用，
            // 但通过 select_nth_unstable_by 至少把 N 全排序换成 k 全排序 + N 一次 partition。
            SortField::Path => {
                let v = hits.as_slice();
                if v.is_empty() {
                    vec![]
                } else {
                    let mut out = v.to_vec();
                    let desc = q.sort_desc;
                    let cmp = |a: &u32, b: &u32| -> std::cmp::Ordering {
                        let pa = store.entry_display_path(*a as usize).unwrap_or_default();
                        let pb = store.entry_display_path(*b as usize).unwrap_or_default();
                        if desc {
                            pb.cmp(&pa)
                        } else {
                            pa.cmp(&pb)
                        }
                    };
                    select_top_k_then_sort(&mut out, limit, cmp);
                    out
                }
            }
        };
        let _sort_us = _t_sort.elapsed().as_micros();
        let _t_build = std::time::Instant::now();
        let mut out = Vec::with_capacity(ordered.len());
        let mut _path_us: u128 = 0;
        let mut _hl_us: u128 = 0;
        let mut _meta_us: u128 = 0;
        for idx in ordered {
            let e = &store.entries[idx as usize];
            let name = store.name_str(e)?.to_string();
            let name_bytes = store.name_bytes(e);
            let _t = if _dbg_fin { Some(std::time::Instant::now()) } else { None };
            let name_highlight = highlight_name_for_query(&name, name_bytes, q, opt, pin_res);
            if let Some(t) = _t {
                _hl_us += t.elapsed().as_micros();
            }
            let _t = if _dbg_fin { Some(std::time::Instant::now()) } else { None };
            let (size, mtime, _) = effective_meta(overlay, store, idx);
            if let Some(t) = _t {
                _meta_us += t.elapsed().as_micros();
            }
            let _t = if _dbg_fin { Some(std::time::Instant::now()) } else { None };
            let path = store.entry_display_path(idx as usize)?;
            if let Some(t) = _t {
                _path_us += t.elapsed().as_micros();
            }
            out.push(SearchHit {
                entry_idx: idx,
                name,
                path,
                size,
                mtime,
                name_highlight,
            });
        }
        if _dbg_fin {
            eprintln!(
                "[finalize] sort_field={:?} hits_total={} returned={} sort={:.2}ms build={:.2}ms (path={:.2}ms hl={:.2}ms meta={:.2}ms)",
                sort_by,
                _hits_total,
                out.len(),
                _sort_us as f64 / 1000.0,
                _t_build.elapsed().as_micros() as f64 / 1000.0,
                _path_us as f64 / 1000.0,
                _hl_us as f64 / 1000.0,
                _meta_us as f64 / 1000.0,
            );
        }
        Ok(out)
    }

    pub fn index_store(&self) -> parking_lot::RwLockReadGuard<'_, IndexStore> {
        self.store.read()
    }

    pub fn index_store_mut(&self) -> parking_lot::RwLockWriteGuard<'_, IndexStore> {
        self.store.write()
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
    if !q.name_terms.is_empty() {
        q.name_terms
            .iter()
            .filter(|s| !s.is_empty())
            .cloned()
            .collect()
    } else if let Some(s) = q.substring.as_ref() {
        if s.is_empty() {
            Vec::new()
        } else {
            vec![s.clone()]
        }
    } else {
        Vec::new()
    }
}

/// 一次性编译拼音 matcher，下游 par_iter / highlight 全部复用。
///
/// 注意：lita::Regex 是 `Send + Sync + Clone`，文档建议「在每个线程上 clone 一份」以避免
/// 内部 cache pool 的 spin-lock 竞争（短 haystack + 大并发场景）。
/// 本项目目前先用共享引用，若后续 bench 看到锁争用再改成 per-thread clone。
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
    let result = (0u32..n)
        .into_par_iter()
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
#[cfg(feature = "pinyin")]
fn fused_scan_pinyin(
    store: &IndexStore,
    overlay: &MetaOverlay,
    q: &ParsedQuery,
    pin_res: &[IbRegex<'_>],
) -> Vec<u32> {
    debug_assert!(!pin_res.is_empty(), "fused_scan_pinyin 调用方必须保证 pin_res 非空");

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
    let n = store.entries.len() as u32;
    let any_size_filter = metadata_ready && (size_min.is_some() || size_max.is_some());
    let any_time_filter = mtime_min.is_some()
        || mtime_max.is_some()
        || ctime_min.is_some()
        || ctime_max.is_some();

    let _dbg = std::env::var("FINDX2_DEBUG_SEARCH").is_ok();
    let _t_par = std::time::Instant::now();
    let _n_threads = if _dbg { rayon::current_num_threads() } else { 0 };

    let result = (0u32..n)
        .into_par_iter()
        .fold(Vec::new, |mut acc, idx| {
            let e = unsafe { store.entries.get_unchecked(idx as usize) };
            if e.is_deleted() {
                return acc;
            }
            if check_deleted_bm && store.deleted.contains(idx) {
                return acc;
            }
            let is_dir = e.is_dir_entry();
            if only_files && is_dir {
                return acc;
            }
            if only_dirs && !is_dir {
                return acc;
            }
            if attrib_must != 0 {
                let a = e.attrs & 0xff;
                if (a & attrib_must) != attrib_must {
                    return acc;
                }
            }
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
            // 名字匹配（AND）：lita::Regex 接 &str；name_str 在 UTF-8 校验失败时会用 lossy。
            // 我们的索引 name 必为合法 UTF-8（建索引时强制），unsafe 走原始字节避免重复校验。
            let name_bytes = store.name_bytes(e);
            // SAFETY：建索引阶段已保证 entry.name 是合法 UTF-8（FRN→name 通过 OS API 取的 wide string 转过来）。
            let name_str = unsafe { std::str::from_utf8_unchecked(name_bytes) };
            for re in pin_res.iter() {
                if re.find(name_str).is_none() {
                    return acc;
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
            "[fused_scan_pinyin] entries={} hits={} threads={} took={:.2}ms needles={}",
            n,
            result.len(),
            _n_threads,
            _t_par.elapsed().as_micros() as f64 / 1000.0,
            pin_res.len()
        );
    }
    result
}

fn initial_candidates(store: &IndexStore, q: &ParsedQuery) -> Vec<u32> {
    let ext_src: Vec<String> = if !q.ext_list.is_empty() {
        q.ext_list.clone()
    } else if let Some(ref e) = q.ext {
        vec![e.clone()]
    } else {
        Vec::new()
    };

    if ext_src.is_empty() {
        return (0..store.entries.len() as u32).collect();
    }

    let mut acc: Option<RoaringBitmap> = None;
    for ext in &ext_src {
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
        .unwrap_or_else(|| Vec::new())
}

fn path_matches_drive_prefix(
    store: &IndexStore,
    entry_idx: usize,
    drive: Option<char>,
    path_prefix: Option<&str>,
) -> bool {
    let vol_letter = store
        .volumes
        .first()
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
        let stored = store.volumes.first().map(|v| v.volume_letter).unwrap_or(b'C');
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

/// `path:` — 全路径小写字节（卷符 + 目录 + 文件名）用于子串匹配；`nowfn` 为真时仅文件名小写
fn path_full_lower(store: &IndexStore, entry_idx: usize, nowfn: bool) -> Vec<u8> {
    let e = &store.entries[entry_idx];
    let name_s = store
        .name_str(e)
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_else(|_| {
            String::from_utf8_lossy(store.name_bytes(e))
                .to_ascii_lowercase()
        });
    if nowfn {
        return name_s.as_bytes().to_vec();
    }
    let letter = store
        .volumes
        .first()
        .map(|v| v.volume_letter as char)
        .unwrap_or('C')
        .to_ascii_lowercase() as u8;
    let dir = dir_path_lower(store, entry_idx);
    let mut full = Vec::with_capacity(4 + dir.len() + name_s.len());
    full.push(letter);
    full.push(b':');
    full.extend_from_slice(dir.as_ref());
    full.push(b'\\');
    full.extend_from_slice(name_s.as_bytes());
    full
}

/// `parent:` / `infolder:`：父目录路径与给定路径**精确一致**（忽略大小写、首尾 `\`），对齐 Everything。
fn match_parent_path_exact(store: &IndexStore, entry_idx: usize, parent_needle: &str) -> bool {
    let e = &store.entries[entry_idx];
    let dir_full = store.resolve_dir_path_lower(e.dir_idx);
    let path_s = std::str::from_utf8(dir_full.as_ref())
        .map(|s| s.trim_matches('\\').to_ascii_lowercase())
        .unwrap_or_default();
    let needle = parent_needle
        .trim_matches('\\')
        .trim()
        .to_ascii_lowercase();
    path_s == needle
}

/// `parentcontains:`：仅在**父目录路径**（不含文件名）中做子串匹配（旧行为，非 Everything 默认）。
fn match_parent_path(store: &IndexStore, entry_idx: usize, parent_needle: &str) -> bool {
    let e = &store.entries[entry_idx];
    let dir_full = store.resolve_dir_path_lower(e.dir_idx);
    let norm_needle = parent_needle.trim_matches('\\').to_ascii_lowercase();
    let needle_b = norm_needle.as_bytes();
    memmem::find(dir_full.as_ref(), needle_b).is_some()
}

/// 目录深度（按 `\` 分段，至少为 1）
fn path_depth(store: &IndexStore, entry_idx: usize) -> u32 {
    let b = dir_path_lower(store, entry_idx);
    if b.is_empty() {
        return 1;
    }
    b.iter().filter(|&&c| c == b'\\').count() as u32 + 1
}

/// 父目录 FRN -> 直接子项数量（文件与子目录）
fn child_count_map(store: &IndexStore) -> HashMap<u64, u32> {
    let mut per_parent_idx: HashMap<u32, u32> = HashMap::new();
    for i in 0..store.entries.len() {
        let di = store.entries[i].dir_idx;
        *per_parent_idx.entry(di).or_insert(0) += 1;
    }
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
    let low = ns.to_ascii_lowercase();
    let mut out = Vec::new();
    for &idx in hits {
        let p = store.entry_display_path(idx as usize)?;
        let data =
            std::fs::read(std::path::Path::new(&p)).map_err(|e| crate::Error::Platform(e.to_string()))?;
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
    mut hits: Vec<u32>,
) -> Result<Vec<u32>> {
    if let Some(ref sw) = q.starts_with {
        let swb = sw.as_bytes();
        hits.retain(|&idx| {
            let n = store.name_bytes(&store.entries[idx as usize]);
            if q.case_sensitive {
                n.starts_with(swb)
            } else {
                n.eq_ignore_ascii_case(swb)
                    || std::str::from_utf8(n)
                        .map(|s| s.to_ascii_lowercase().starts_with(sw))
                        .unwrap_or(false)
            }
        });
    }
    if let Some(ref ew) = q.ends_with {
        hits.retain(|&idx| {
            let n = store.name_bytes(&store.entries[idx as usize]);
            if q.case_sensitive {
                n.ends_with(ew.as_bytes())
            } else {
                std::str::from_utf8(n)
                    .map(|s| s.to_ascii_lowercase().ends_with(ew.as_str()))
                    .unwrap_or(false)
            }
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
) -> Vec<[u32; 2]> {
    let mut byte_ranges: Vec<(usize, usize)> = Vec::new();
    if let Some(ref pat) = q.regex_pattern {
        if let Ok(re) = Regex::new(pat) {
            if let Some(m) = re.find(name_bytes) {
                byte_ranges.push((m.start(), m.end()));
            }
        }
        return byte_ranges_to_char_ranges_merged(name, byte_ranges);
    }
    if q.glob_pattern.is_some() {
        return vec![];
    }
    if !q.name_terms.is_empty() {
        for (i, term) in q.name_terms.iter().enumerate() {
            let nb = if q.case_sensitive {
                term.as_bytes().to_vec()
            } else {
                term.to_ascii_lowercase().into_bytes()
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
                name, name_bytes, &nb, q, opt, pin_re,
            ));
        }
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
        #[cfg(feature = "pinyin")]
        let pin_re: Option<&IbRegexUnit<'_>> = pin_res.first();
        #[cfg(not(feature = "pinyin"))]
        let pin_re: Option<&IbRegexUnit<'_>> = {
            let _ = pin_res;
            None
        };
        byte_ranges.extend(needle_byte_ranges_for_name_match(
            name,
            name_bytes,
            &needle_bs,
            q,
            opt,
            pin_re,
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

fn find_all_literal_substrings(haystack: &[u8], needle: &[u8]) -> Vec<(usize, usize)> {
    if needle.is_empty() {
        return vec![];
    }
    let finder = memmem::Finder::new(needle);
    let mut out = Vec::new();
    let mut pos = 0usize;
    while pos < haystack.len() {
        let slice = &haystack[pos..];
        if let Some(off) = finder.find(slice) {
            let start = pos + off;
            let end = start + needle.len();
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
    needle_bs: &[u8],
    q: &ParsedQuery,
    #[allow(unused_variables)]
    opt: &SearchOptions,
    #[cfg_attr(not(feature = "pinyin"), allow(unused_variables))]
    pin_re: Option<&IbRegexUnit<'_>>,
) -> Vec<(usize, usize)> {
    if needle_bs.is_empty() {
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
            ranges.extend(find_all_literal_substrings(name_bytes, needle_bs));
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
        ranges.extend(find_all_literal_substrings(name_bytes, needle_bs));
    } else {
        let h: Vec<u8> = name.to_ascii_lowercase().into_bytes();
        ranges.extend(find_all_literal_substrings(&h, needle_bs));
    }
    merge_byte_ranges(ranges)
}

/// 让函数签名在 cfg pinyin / no-pinyin 都能写 `Option<&IbRegexUnit<'_>>`。
/// pinyin feature 启用时 = 真正的 lita::Regex；关闭时 = 占位单元类型（永远拿不到 Some）。
#[cfg(feature = "pinyin")]
pub(crate) type IbRegexUnit<'a> = IbRegex<'a>;
#[cfg(not(feature = "pinyin"))]
pub(crate) struct IbRegexUnit<'a>(std::marker::PhantomData<&'a ()>);

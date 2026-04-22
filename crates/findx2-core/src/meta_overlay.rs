//! 紧凑回填元数据 overlay：取代原先的 `DashMap<u32, (u64, u64, u64)>`。
//!
//! ## 为什么需要这个？
//!
//! 后台回填线程把"扫描出来的 size/mtime/ctime"写到这里；search 路径读这里——
//! 全程**无锁**，回填永不和 search 抢 IndexStore 的 `RwLock`。
//!
//! ## 为什么不直接用 DashMap？
//!
//! - 内存：DashMap key + value + bucket overhead ≈ 50B/条；8.5M 文件回填高峰 ~430MB。
//! - 速度：DashMap.get 要 hash + cas + bucket walk，约几十 ns；
//!   平铺 `Vec<AtomicU64>` 直接下标访问，~3 ns。
//! - 释放：DashMap 析构按 bucket 释放，几百 MB 时长尾几百 ms。
//! - 用不上 DashMap 的能力：我们的 key 是连续的 entry idx（0..entry_count），
//!   完全可以平铺数组；hashmap 是杀鸡用牛刀。
//!
//! ## 内存占用
//!
//! 每条 entry 固定 16 字节（一个 size u64 + 一个 mtime/ctime 打包 u64）。
//! 8.5M entry → **~136MB 常驻**。这是固定的，不会随回填进度涨跌。
//!
//! ## 编码
//!
//! - `sizes[i]`：u64，原值字节数；`u64::MAX` 表示"未回填/无效"（合法 size 不会到 EB 级）。
//! - `mctime[i]`：u64，高 32 位 = mtime 秒（unix epoch），低 32 位 = ctime 秒（unix epoch）。
//!   两个时间戳 0 同时表示"未填"，与 IndexStore::Entry.mtime/ctime 的"0=未知"语义对齐。
//!
//! ## 与 IndexStore 的关系
//!
//! - search 优先看 overlay 的 size：若 ≠ MAX，整条 (size, mtime, ctime) 都用 overlay 的；
//!   否则 fallback 到 IndexStore::Entry 的字段（mft 首遍写入的 USN TimeStamp）。
//! - 持久化时（`save_index_bin` 之前）需要把 overlay flush 进 IndexStore.entries
//!   再落盘——这步是合并到主索引的唯一时机。

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

/// 表示"槽位未回填"。合法文件 size 永远到不了这个数。
const SIZE_UNSET: u64 = u64::MAX;

pub struct MetaOverlay {
    /// 与 IndexStore.entries 一一对应，长度 = entry_count。
    /// **注意**：这里的索引是 entry idx（usize），不是 FRN。
    sizes: Vec<AtomicU64>,
    /// 高 32 位 mtime / 低 32 位 ctime，单位都是 unix 秒。
    mctime: Vec<AtomicU64>,
    /// 已回填条目计数（只增不减；search 用不到，主要是给统计/进度用）。
    filled: AtomicUsize,
}

impl MetaOverlay {
    /// 给定 entry 总数，按 SIZE_UNSET 初始化。
    /// 8.5M 条目大约耗时 50-100ms，发生在 service 启动一次性，可忽略。
    pub fn new(entry_count: usize) -> Self {
        let mut sizes = Vec::with_capacity(entry_count);
        let mut mctime = Vec::with_capacity(entry_count);
        for _ in 0..entry_count {
            sizes.push(AtomicU64::new(SIZE_UNSET));
            mctime.push(AtomicU64::new(0));
        }
        Self {
            sizes,
            mctime,
            filled: AtomicUsize::new(0),
        }
    }

    /// 容量；不会变（除非 IndexStore 整体重建，那种场景下 SearchEngine 会换 overlay 实例）。
    pub fn capacity(&self) -> usize {
        self.sizes.len()
    }

    /// 已回填条目数；用于回填进度统计 / overlay flush 是否值得。
    pub fn filled_count(&self) -> usize {
        self.filled.load(Ordering::Relaxed)
    }

    /// 写入一条回填结果。`mtime`/`ctime` 单位 unix 秒；与 IndexStore::Entry 字段对齐。
    /// **`size == u64::MAX` 是非法值**，本函数会跳过；正常文件 size 不可能到这个值。
    #[inline]
    pub fn put(&self, idx: usize, size: u64, mtime: u32, ctime: u32) {
        if idx >= self.sizes.len() || size == SIZE_UNSET {
            return;
        }
        let packed = ((mtime as u64) << 32) | (ctime as u64);
        // 只在第一次填充时计数（避免重复回填导致 filled 虚高）。
        let prev = self.sizes[idx].swap(size, Ordering::Relaxed);
        self.mctime[idx].store(packed, Ordering::Relaxed);
        if prev == SIZE_UNSET {
            self.filled.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// 查询：返回 `Some((size, mtime_secs, ctime_secs))` 表示已回填；`None` 表示槽位空。
    #[inline]
    pub fn get(&self, idx: usize) -> Option<(u64, u32, u32)> {
        if idx >= self.sizes.len() {
            return None;
        }
        let size = self.sizes[idx].load(Ordering::Relaxed);
        if size == SIZE_UNSET {
            return None;
        }
        let packed = self.mctime[idx].load(Ordering::Relaxed);
        Some((size, (packed >> 32) as u32, (packed & 0xFFFF_FFFF) as u32))
    }

    /// 清空所有槽位（持久化合并完后或重建索引时调用）。
    /// 不释放底层 Vec 容量——overlay 长度跟随 entry_count，不会变。
    pub fn clear(&self) {
        for s in &self.sizes {
            s.store(SIZE_UNSET, Ordering::Relaxed);
        }
        for m in &self.mctime {
            m.store(0, Ordering::Relaxed);
        }
        self.filled.store(0, Ordering::Relaxed);
    }

    /// 遍历所有"已回填"槽位的快照。**不持锁**——并发 put 可能让快照漏掉刚写进来的，
    /// 但回填阶段无关紧要：下一轮 flush 还会再扫一遍。返回 `(idx, size, mtime, ctime)`。
    /// 用于持久化时把 overlay 倒进主索引。
    pub fn snapshot(&self) -> Vec<(usize, u64, u32, u32)> {
        let mut out = Vec::with_capacity(self.filled_count());
        for i in 0..self.sizes.len() {
            let size = self.sizes[i].load(Ordering::Relaxed);
            if size == SIZE_UNSET {
                continue;
            }
            let packed = self.mctime[i].load(Ordering::Relaxed);
            out.push((i, size, (packed >> 32) as u32, (packed & 0xFFFF_FFFF) as u32));
        }
        out
    }
}

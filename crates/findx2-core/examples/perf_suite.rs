//! 全套性能基线：`cargo run --release -p findx2-core --example perf_suite -- <index.bin>`
//!
//! 输出一份可直接贴进 README/报告的实测表，覆盖：
//!   1. 文件体积与加载耗时（冷加载，首次 `load_index_bin`）
//!   2. 索引构成（条目/目录/名字/墓碑区各占多少、interning 效果、字节/条目）
//!   3. 常驻内存（Windows 用 `GetProcessMemoryInfo` 读 WorkingSet / PrivateUsage）
//!   4. 查询延迟矩阵：子串 / ext / 拼音 / 组合 / 排序 / path / folder，各跑 N 轮取中位数
//!
//! 与 `real_index_bench` 的区别：那个只看「加载 + 3 个查询」，
//! 这个跑**多轮取中位数**并带 RSS，适合做「改动前 vs 改动后」的对照基线。

use std::path::PathBuf;
use std::time::Instant;

use findx2_core::search::PinyinMatchMode;
use findx2_core::{
    load_index_bin, ParsedQuery, QueryParser, SearchEngine, SearchOptions, SortField,
};

/// 当前进程常驻内存（MB）：Windows 读 WorkingSet，其他平台读 /proc 或返回 0。
fn rss_mb() -> f64 {
    #[cfg(windows)]
    {
        // 用 PowerShell 太重；直接读 K32 的 GetProcessMemoryInfo。
        #[repr(C)]
        struct ProcessMemoryCounters {
            cb: u32,
            page_fault_count: u32,
            peak_working_set_size: usize,
            working_set_size: usize,
            quota_peak_paged_pool_usage: usize,
            quota_paged_pool_usage: usize,
            quota_peak_non_paged_pool_usage: usize,
            quota_non_paged_pool_usage: usize,
            pagefile_usage: usize,
            peak_pagefile_usage: usize,
        }
        extern "system" {
            fn GetCurrentProcess() -> isize;
            fn K32GetProcessMemoryInfo(
                process: isize,
                counters: *mut ProcessMemoryCounters,
                cb: u32,
            ) -> i32;
        }
        unsafe {
            let mut pmc: ProcessMemoryCounters = std::mem::zeroed();
            pmc.cb = std::mem::size_of::<ProcessMemoryCounters>() as u32;
            if K32GetProcessMemoryInfo(GetCurrentProcess(), &mut pmc, pmc.cb) != 0 {
                return pmc.working_set_size as f64 / (1024.0 * 1024.0);
            }
        }
        0.0
    }
    #[cfg(not(windows))]
    {
        if let Ok(s) = std::fs::read_to_string("/proc/self/status") {
            for line in s.lines() {
                if let Some(rest) = line.strip_prefix("VmRSS:") {
                    if let Some(kb) = rest.split_whitespace().next() {
                        return kb.parse::<f64>().unwrap_or(0.0) / 1024.0;
                    }
                }
            }
        }
        0.0
    }
}

/// 跑 N 轮取中位数（毫秒），返回 (中位数, 最小, 命中数)。
fn bench<F: FnMut() -> usize>(rounds: usize, mut f: F) -> (f64, f64, usize) {
    let mut times = Vec::with_capacity(rounds);
    let mut hits = 0;
    for _ in 0..rounds {
        let t = Instant::now();
        hits = f();
        times.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    times.sort_by(|a, b| a.partial_cmp(b).unwrap());
    (times[times.len() / 2], times[0], hits)
}

fn main() {
    let path: PathBuf = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("index.bin"));

    println!("======== FindX 性能基线 ========");
    println!("索引: {}", path.display());

    let file_bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    println!("文件大小: {:.2} GB ({file_bytes} B)", file_bytes as f64 / 1073741824.0);

    let rss_before = rss_mb();
    println!("加载前进程 RSS: {rss_before:.1} MB");

    // ── 1. 冷加载 ──
    let t0 = Instant::now();
    let store = load_index_bin(&path).expect("load_index_bin");
    let load_ms = t0.elapsed().as_secs_f64() * 1000.0;
    let rss_after = rss_mb();
    println!();
    println!("── 加载 ──");
    println!("load_index_bin : {load_ms:.1} ms  ({:.2} s)", load_ms / 1000.0);
    println!("加载后进程 RSS : {rss_after:.1} MB");
    println!("加载净增 RSS   : {:.1} MB", rss_after - rss_before);

    // ── 2. 索引构成 ──
    let n_entries = store.entries.len();
    let n_dirs = store.dirs.len();
    let n_names = store.names_buf.len();
    let n_tomb = store.deleted.len() as usize;
    let live = n_entries.saturating_sub(n_tomb);
    println!();
    println!("── 索引构成 ──");
    println!("条目数         : {n_entries}");
    println!("目录数         : {n_dirs}");
    println!("墓碑数         : {n_tomb} ({:.2}%)", store.tombstone_ratio() * 100.0);
    println!("存活条目       : {live}");
    println!("条目区         : {:.2} GB ({} × 32B)", n_entries as f64 * 32.0 / 1073741824.0, n_entries);
    println!("目录区         : {:.2} GB ({} × 24B)", n_dirs as f64 * 24.0 / 1073741824.0, n_dirs);
    println!(
        "名字区         : {:.2} GB ({} B, {:.2} B/条)",
        n_names as f64 / 1073741824.0,
        n_names,
        if n_entries > 0 { n_names as f64 / n_entries as f64 } else { 0.0 }
    );
    println!("磁盘字节/条目  : {:.1}", if n_entries > 0 { file_bytes as f64 / n_entries as f64 } else { 0.0 });

    // ── 3. 查询延迟矩阵 ──
    let engine = SearchEngine::new(store);
    let rounds: usize = std::env::var("ROUNDS").ok().and_then(|s| s.parse().ok()).unwrap_or(5);

    let opt_no_py = SearchOptions::default(); // allow_pinyin 默认 false
    let mut opt_py = SearchOptions::default();
    opt_py.allow_pinyin = true;
    opt_py.pinyin_match_mode = PinyinMatchMode::Auto;

    println!();
    println!("── 查询延迟（{} 轮取中位数）──", rounds);
    println!("{:<34} {:>10} {:>10} {:>10}", "查询", "中位数", "最小", "命中");

    let cases: &[(&str, &str, bool)] = &[
        ("子串: readme", "readme", false),
        ("子串: config", "config", false),
        ("ext:txt", "ext:txt", false),
        ("ext:pdf", "ext:pdf", false),
        ("ext:txt + 子串 readme", "ext:txt readme", false),
        ("folder: tmp", "folder:tmp", false),
        ("path: users", "path:users", false),
        ("startwith: test", "startwith:test", false),
        ("endwith: .log", "endwith:.log", false),
        ("拼音: jpg (全拼触发)", "jpg", true),
    ];

    for (label, qstr, use_py) in cases {
        let pq: ParsedQuery = match QueryParser::parse(qstr) {
            Ok(q) => q,
            Err(e) => {
                println!("{label:<34} parse 失败: {e}");
                continue;
            }
        };
        let opt = if *use_py { &opt_py } else { &opt_no_py };
        let (med, min, hits) = bench(rounds, || {
            engine.search(&pq, opt).map(|(v, _)| v.len()).unwrap_or(0)
        });
        println!("{label:<34} {med:>8.1}ms {min:>8.1}ms {hits:>10}");
    }

    // ── 4. 排序查询 ──
    println!();
    println!("── 排序查询 ──");
    let mut pq_sort = QueryParser::parse("ext:txt").unwrap();
    pq_sort.sort_by = SortField::Size;
    pq_sort.sort_desc = true;
    pq_sort.limit = 100;
    let (med, min, hits) = bench(rounds, || {
        engine.search(&pq_sort, &opt_no_py).map(|(v, _)| v.len()).unwrap_or(0)
    });
    println!("{:<34} {:>10} {:>10} {:>10}", "ext:txt sort:size desc 100", format!("{med:.1}ms"), format!("{min:.1}ms"), hits);

    let rss_final = rss_mb();
    println!();
    println!("── 汇总 ──");
    println!("加载耗时       : {load_ms:.1} ms");
    println!("常驻内存 RSS   : {rss_final:.1} MB");
    println!("文件大小       : {:.2} GB", file_bytes as f64 / 1073741824.0);
    println!("存活条目       : {live}");
    println!("================================");
}

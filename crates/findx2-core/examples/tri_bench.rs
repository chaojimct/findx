//! trigram 剪枝 / mmap 加载 / 拼音剪枝基准：同一份 index.bin 上跑「无边车 vs 有边车」查询对比。
//!
//! ```text
//! cargo run --release -p findx2-core --example tri_bench -- <index.bin>
//! ```

use std::path::Path;
use std::time::Instant;

use findx2_core::{build_trigram_sidecar, load_index_bin, QueryParser, SearchEngine, SearchOptions};

const QUERIES: &[&str] = &[
    "config",     // 中频词
    "readme",     // 中频词
    ".gitignore", // 高频（扩展名命中极多文件）
    "kernel32",   // 低频词
    "template",   // 中频
];

const PREFIX_QUERIES: &[&str] = &[
    "startwith:config",   // 前缀中频
    "startwith:kernel32", // 前缀低频
    "startwith:readme",   // 前缀中频
];

/// 拼音 Auto 场景：全拼命中中文名走 `trigram ∪ cjk_names` 剪枝（主战场），
/// 纯 ASCII 字面走 trigram 主导；两条路都在 fused_scan_pinyin 里验证。
const PINYIN_QUERIES: &[&str] = &[
    "config",            // 纯 ASCII 字面对照
    "jisuanqi",          // 计算器（全拼）
    "weixin",            // 微信
    "startwith:config",  // 前缀 + 拼音路径
    "startwith:jisuanqi", // 前缀全拼
];

fn bench(engine: &SearchEngine, query: &str) -> (f64, u32, usize) {
    let pq = QueryParser::parse(query).unwrap();
    let opts = SearchOptions::default();
    let _ = engine.search(&pq, &opts).unwrap(); // 预热（页缓存 / 线程池）
    let mut samples = Vec::with_capacity(5);
    let mut total = 0;
    let mut hits = 0;
    for _ in 0..5 {
        let t = Instant::now();
        let (h, tot) = engine.search(&pq, &opts).unwrap();
        samples.push(t.elapsed().as_micros() as f64);
        total = tot;
        hits = h.len();
    }
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let median = samples[samples.len() / 2] / 1000.0;
    (median, total, hits)
}

/// 拼音 Auto 基准（median ms + hits；hits 用于边车前后一致性对照）。
fn bench_pinyin(engine: &SearchEngine, query: &str) -> (f64, usize) {
    let pq = QueryParser::parse(query).unwrap();
    let opts = SearchOptions {
        allow_pinyin: true,
        ..Default::default()
    };
    let _ = engine.search(&pq, &opts).unwrap(); // 预热
    let mut samples = Vec::with_capacity(5);
    let mut hits = 0;
    for _ in 0..5 {
        let t = Instant::now();
        let (h, _) = engine.search(&pq, &opts).unwrap();
        hits = h.len();
        samples.push(t.elapsed().as_micros() as f64);
    }
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
    (samples[samples.len() / 2] / 1000.0, hits)
}

fn main() {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "index.bin".into());
    let path = Path::new(&path);

    // 上次跑留下的边车会让「基线」也带剪枝；先删掉保证从全表扫描起测。
    let _ = std::fs::remove_file(findx2_core::tri_sidecar_path(path));

    println!("=== 基线：无 trigram 边车（全表 SIMD 扫描）===");
    let t = Instant::now();
    let store = load_index_bin(path).unwrap();
    println!(
        "加载：{:.1} ms（{} 条目）",
        t.elapsed().as_secs_f64() * 1000.0,
        store.entry_count()
    );
    let engine = SearchEngine::new(store);
    let mut baseline = Vec::new();
    for q in QUERIES.iter().chain(PREFIX_QUERIES.iter()) {
        let (ms, total, hits) = bench(&engine, q);
        baseline.push(ms);
        println!("  query={q:<22} median={ms:8.2} ms  hits={hits} total={total}");
    }
    drop(engine);

    // GUI 默认路径：allow_pinyin=true + Auto —— fused_scan_pinyin 全表正则扫描。
    println!("\n=== 拼音基线：Auto 无边车（全表 lita 正则）===");
    let engine = {
        let store = load_index_bin(path).unwrap();
        SearchEngine::new(store)
    };
    let mut pinyin_baseline = Vec::new();
    for q in PINYIN_QUERIES.iter() {
        let (ms, hits) = bench_pinyin(&engine, q);
        pinyin_baseline.push((ms, hits));
        println!("  query={q:<22} median={ms:8.2} ms  hits={hits}");
    }
    drop(engine);

    println!("\n=== 构建边车（一次性成本）===");
    let t = Instant::now();
    let store = load_index_bin(path).unwrap();
    build_trigram_sidecar(&store, path).unwrap();
    println!("构建耗时：{:.2} s", t.elapsed().as_secs_f64());
    drop(store);

    println!("\n=== 优化后：mmap 挂载 trigram 边车===");
    let t = Instant::now();
    let store = load_index_bin(path).unwrap();
    println!(
        "加载（含边车）：{:.1} ms（{} 条目，trigram {}）",
        t.elapsed().as_secs_f64() * 1000.0,
        store.entry_count(),
        if store.trigram.is_some() { "on" } else { "off" }
    );
    let engine = SearchEngine::new(store);
    println!("query                    baseline      trigram     speedup");
    for (i, q) in QUERIES.iter().chain(PREFIX_QUERIES.iter()).enumerate() {
        let (ms, total, hits) = bench(&engine, q);
        let base = baseline[i];
        let speedup = if ms > 0.001 { base / ms } else { f64::INFINITY };
        println!(
            "{q:<24} {base:8.2} ms {ms:8.2} ms  {speedup:7.1}x  hits={hits} total={total}"
        );
    }

    println!("\n=== 拼音优化后：Auto + 边车剪枝（trigram ∪ cjk_names）===");
    println!("query                    baseline    pruned     speedup   hits 一致");
    for (i, q) in PINYIN_QUERIES.iter().enumerate() {
        let (base, hits_base) = pinyin_baseline[i];
        let (ms, hits) = bench_pinyin(&engine, q);
        let speedup = if ms > 0.001 { base / ms } else { f64::INFINITY };
        let ok = if hits_base == hits { "yes" } else { "NO!" };
        println!(
            "{q:<24} {base:8.2} ms {ms:8.2} ms  {speedup:7.1}x  {ok} ({hits})"
        );
    }
}

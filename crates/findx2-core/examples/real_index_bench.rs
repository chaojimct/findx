//! 真实 index.bin 诊断 + 测速：`cargo run --release -p findx2-core --example real_index_bench -- path\to\index.bin`
//!
//! 除了测速，还会打印**索引构成**：条目/目录/名字区各占多少、墓碑（已删除但保留的）占比、
//! 名字区是否被 interning 压过。用来回答「这个库怎么这么大」这类容量问题。

use std::path::PathBuf;
use std::time::Instant;

use findx2_core::{
    load_index_bin, ParsedQuery, QueryParser, SearchEngine, SearchOptions, SortField,
};

fn main() {
    let path: PathBuf = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("index.bin"));

    let file_bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    eprintln!("加载: {}", path.display());
    eprintln!(
        "  文件大小: {:.2} GB",
        file_bytes as f64 / (1024.0 * 1024.0 * 1024.0)
    );
    let t0 = Instant::now();
    let store = load_index_bin(&path).expect("load_index_bin");
    let load_ms = t0.elapsed().as_secs_f64() * 1000.0;
    eprintln!(
        "  load_index_bin: {load_ms:.1} ms | 条目 {}",
        store.entry_count()
    );

    // ── 索引构成诊断 ──────────────────────────────────────────────
    let n_entries = store.entries.len();
    let n_dirs = store.dirs.len();
    let n_names = store.names_buf.len();
    let n_tomb = store.deleted.len() as usize;
    let sec_entries = n_entries as f64 * 32.0;
    let sec_dirs = n_dirs as f64 * 24.0;
    eprintln!("  ── 构成 ──");
    eprintln!(
        "  条目区 {:>6.2} GB  ({} 条 × 32B)",
        sec_entries / 1073741824.0,
        n_entries
    );
    eprintln!(
        "  目录区 {:>6.2} GB  ({} 个 × 24B)",
        sec_dirs / 1073741824.0,
        n_dirs
    );
    eprintln!(
        "  名字区 {:>6.2} GB  ({} 字节, 平均 {:.2} B/条)",
        n_names as f64 / 1073741824.0,
        n_names,
        if n_entries > 0 {
            n_names as f64 / n_entries as f64
        } else {
            0.0
        }
    );
    eprintln!(
        "  墓碑   {} 条 ({:.2}% 条目是已删除的残留)",
        n_tomb,
        store.tombstone_ratio() * 100.0
    );
    eprintln!(
        "  磁盘占用/条目: {:.1} 字节",
        if n_entries > 0 {
            file_bytes as f64 / n_entries as f64
        } else {
            0.0
        }
    );
    let live = n_entries.saturating_sub(n_tomb);
    eprintln!("  存活条目: {live}（墓碑压缩后可回收 {:.2} GB）",
        n_tomb as f64 * 32.0 / 1073741824.0);

    let engine = SearchEngine::new(store);
    let opt = SearchOptions::default();

    let cases: &[(&str, &str)] = &[
        ("ext:txt", "ext:txt"),
        ("子串（示例）", "readme"),
        ("ext:txt + 子串", "ext:txt readme"),
    ];

    for (label, qstr) in cases {
        let pq: ParsedQuery = QueryParser::parse(qstr).unwrap();
        let t1 = Instant::now();
        let n = engine.search(&pq, &opt).map(|(v, _t)| v.len()).unwrap_or(0);
        let ms = t1.elapsed().as_secs_f64() * 1000.0;
        eprintln!("  [{label}] {qstr:?} -> {n} 条, search {ms:.1} ms");
    }

    let mut pq_sort = QueryParser::parse("ext:txt").unwrap();
    pq_sort.sort_by = SortField::Size;
    pq_sort.sort_desc = true;
    pq_sort.limit = 100;
    let t2 = Instant::now();
    let n = engine
        .search(&pq_sort, &opt)
        .map(|(v, _t)| v.len())
        .unwrap_or(0);
    let ms2 = t2.elapsed().as_secs_f64() * 1000.0;
    eprintln!("  [ext:txt sort:size desc 100] -> {n} 条, search {ms2:.1} ms");
}

//! 搜索性能自测：合成索引 + 若干查询耗时（运行：`cargo bench -p findx2-core --bench search_perf`）
//! 可选参数：文件条数，默认 200000。

use std::time::Instant;

use findx2_core::{
    IndexBuilder, ParsedQuery, QueryParser, RawEntry, SearchEngine, SearchOptions, SortField,
};

fn synthetic_entries(n_files: usize) -> (Vec<RawEntry>, Vec<RawEntry>) {
    let root_frn = 100u64;
    let dirs = vec![RawEntry {
        file_id: root_frn,
        file_id_128: None,
        parent_id: 0,
        name: "synthetic_root".into(),
        size: 0,
        mtime: 0,
        ctime: 0,
        attrs: 0x10,
        is_dir: true,
    }];
    let mut files = Vec::with_capacity(n_files);
    for i in 0..n_files {
        // 仅约 1/5000 条名含 rare_marker，便于测「高选择性」关键字
        let name = if i % 5000 == 0 {
            format!("rare_marker_doc_{i:08}.txt")
        } else {
            format!("noise_{i:08}.txt")
        };
        files.push(RawEntry {
            file_id: 10_000 + i as u64,
            file_id_128: None,
            parent_id: root_frn,
            name,
            size: (i % 4096) as u64,
            mtime: i as u64,
            ctime: 0,
            attrs: 0,
            is_dir: false,
        });
    }
    (files, dirs)
}

fn time_ms(f: impl FnOnce() -> usize) -> (f64, usize) {
    let t0 = Instant::now();
    let n = f();
    (t0.elapsed().as_secs_f64() * 1000.0, n)
}

fn main() {
    let n_files: usize = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(200_000);

    eprintln!("合成索引：约 {n_files} 个文件 + 1 个目录 …");
    let (files, dirs) = synthetic_entries(n_files);
    let t_build = Instant::now();
    let store = IndexBuilder::new(b'C', 1, 1, 1)
        .build_from_raw(files, dirs, true)
        .unwrap();
    eprintln!(
        "  build_from_raw: {:.1} ms",
        t_build.elapsed().as_secs_f64() * 1000.0
    );

    let engine = SearchEngine::new(store);
    let opt = SearchOptions::default();

    let cases: &[(&str, &str)] = &[
        ("ext:txt（扩展名桶缩小候选）", "ext:txt"),
        ("子串 rare_marker（高选择性）", "rare_marker"),
        ("子串 noise（几乎全表扫名）", "noise"),
        ("ext:txt + 子串（组合）", "ext:txt noise"),
    ];

    for (label, qstr) in cases {
        let pq: ParsedQuery = QueryParser::parse(qstr).unwrap();
        let (ms, nhits) = time_ms(|| {
            engine
                .search(&pq, &opt)
                .map(|(v, _t)| v.len())
                .unwrap_or(0)
        });
        eprintln!("  [{label}] {qstr:?}  -> {nhits} 条, {ms:.1} ms");
    }

    // 排序：对较大命中集 sort（模拟用户按大小排序）
    let pq_sort: ParsedQuery = {
        let mut q = QueryParser::parse("ext:txt").unwrap();
        q.sort_by = SortField::Size;
        q.sort_desc = true;
        q.limit = 100;
        q
    };
    let (ms, nhits) = time_ms(|| {
        engine
            .search(&pq_sort, &opt)
            .map(|(v, _t)| v.len())
            .unwrap_or(0)
    });
    eprintln!(
        "  [按大小降序取 100 条] ext:txt sort:size desc -> {nhits} 条, {ms:.1} ms"
    );
}

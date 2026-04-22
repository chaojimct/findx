//! 拼音搜索耗时：仓库 `files_for_test`（若有）+ 合成中文文件名大索引。
//! 运行：`cargo bench -p findx2-core --features pinyin --bench pinyin_perf`
//! 可选参数：合成文件条数，默认 100000。

use std::path::Path;
use std::time::Instant;

use findx2_core::{
    IndexBuilder, ParsedQuery, QueryParser, RawEntry, SearchEngine, SearchOptions,
};

fn synthetic_chinese_entries(n_files: usize) -> (Vec<RawEntry>, Vec<RawEntry>) {
    let root_frn = 100u64;
    let dirs = vec![RawEntry {
        file_id: root_frn,
        file_id_128: None,
        parent_id: 0,
        name: "pinyin_bench_root".into(),
        size: 0,
        mtime: 0,
        ctime: 0,
        attrs: 0x10,
        is_dir: true,
    }];
    let mut files = Vec::with_capacity(n_files);
    for i in 0..n_files {
        let name = if i % 10_000 == 0 {
            format!("北京_{i:08}.txt")
        } else if i % 5000 == 0 {
            format!("上海_{i:08}.txt")
        } else {
            format!("中文噪声_{i:08}.txt")
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

fn time_search(engine: &SearchEngine, pq: &ParsedQuery, allow_pinyin: bool) -> (f64, usize) {
    let t0 = Instant::now();
    let n = engine
        .search(
            pq,
            &SearchOptions {
                allow_pinyin,
                ..Default::default()
            },
        )
        .map(|(v, _)| v.len())
        .unwrap_or(0);
    let ms = t0.elapsed().as_secs_f64() * 1000.0;
    (ms, n)
}

fn main() {
    let n_files: usize = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(100_000);

    eprintln!(
        "—— 合成索引：约 {n_files} 个中文名文件（每 10000 条「北京_*」、每 5000 条「上海_*」、其余「中文噪声_*」）——"
    );
    let t_build = Instant::now();
    let (files, dirs) = synthetic_chinese_entries(n_files);
    let store = IndexBuilder::new(b'C', 1, 1, 1)
        .build_from_raw(files, dirs, true)
        .unwrap();
    eprintln!(
        "  build_from_raw: {:.1} ms",
        t_build.elapsed().as_secs_f64() * 1000.0
    );

    let engine = SearchEngine::new(store);

    let cases: &[(&str, &str, bool)] = &[
        ("全拼 beijing", "beijing", true),
        ("首字母 bj", "bj", true),
        ("广匹配 zhongwen", "zhongwen", true),
        ("首字母 sh", "sh", true),
        ("关拼音 beijing（几乎无字面命中）", "beijing", false),
    ];

    for (label, qstr, py) in cases {
        let pq = QueryParser::parse(qstr).unwrap();
        let (ms, n) = time_search(&engine, &pq, *py);
        eprintln!("  [{label}] {qstr:?} pinyin={py} -> {n} 条, {ms:.2} ms");
    }

    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../files_for_test");
    if fixture.is_dir() {
        eprintln!("—— 仓库 files_for_test（小集，毫秒级仅作对照）——");
        const ROOT_FRN: u64 = 100;
        let dirs = vec![RawEntry {
            file_id: ROOT_FRN,
            file_id_128: None,
            parent_id: 0,
            name: "fixture_root".into(),
            size: 0,
            mtime: 0,
            ctime: 0,
            attrs: 0x10,
            is_dir: true,
        }];
        let mut files: Vec<RawEntry> = Vec::new();
        let mut id: u64 = 10_000;
        for e in std::fs::read_dir(&fixture).unwrap() {
            let e = e.unwrap();
            if !e.metadata().unwrap().is_file() {
                continue;
            }
            let name = e.file_name().to_string_lossy().into_owned();
            files.push(RawEntry {
                file_id: id,
                file_id_128: None,
                parent_id: ROOT_FRN,
                name,
                size: 1,
                mtime: 1,
                ctime: 1,
                attrs: 0,
                is_dir: false,
            });
            id += 1;
        }
        if !files.is_empty() {
            let t0 = Instant::now();
            let store = IndexBuilder::new(b'C', 1, 1, 1)
                .build_from_raw(files, dirs, true)
                .unwrap();
            let eng = SearchEngine::new(store);
            eprintln!(
                "  fixture build: {:.2} ms, {} 个文件",
                t0.elapsed().as_secs_f64() * 1000.0,
                eng.index_store().entry_count()
            );
            for (label, qstr) in [
                ("fixture beijing", "beijing"),
                ("fixture shanghai lujiazui", "shanghai lujiazui"),
            ] {
                let pq = QueryParser::parse(qstr).unwrap();
                let (ms, n) = time_search(&eng, &pq, true);
                eprintln!("  [{label}] {qstr:?} -> {n} 条, {ms:.3} ms");
            }
        }
    }
}

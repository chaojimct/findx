//! M0 bench：`<walk-dir>` 全量抽取建索引 → 体积比/速度/查询延迟。
//! 用法：`cargo run -p findx2-content --example m0_bench -- <walk-dir> <index-dir>`

use std::time::Instant;

use findx2_content::{extract_text, open_or_create, should_extract, DocMeta};

fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for ent in rd.flatten() {
        let p = ent.path();
        if p.is_dir() {
            walk(&p, out);
        } else {
            out.push(p);
        }
    }
}

fn dir_size(dir: &std::path::Path) -> u64 {
    let mut total = 0u64;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for ent in rd.flatten() {
            let p = ent.path();
            if p.is_dir() {
                stack.push(p);
            } else if let Ok(m) = ent.metadata() {
                total += m.len();
            }
        }
    }
    total
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 3 {
        eprintln!("用法：m0_bench <walk-dir> <index-dir>");
        std::process::exit(2);
    }
    let walk_dir = std::path::Path::new(&args[1]);
    let idx_dir = std::path::Path::new(&args[2]);

    let mut files = Vec::new();
    walk(walk_dir, &mut files);
    println!("[bench] 遍历文件总数: {}", files.len());

    let t0 = Instant::now();
    let idx = open_or_create(idx_dir).unwrap();
    let mut extracted = 0u64;
    let mut skipped = 0u64;
    let mut failed = 0u64;
    let mut bytes = 0u64;
    for (i, p) in files.iter().enumerate() {
        if should_extract(p).is_none() {
            skipped += 1;
            continue;
        }
        match extract_text(p) {
            Ok(t) => {
                bytes += t.raw_bytes;
                if let Err(e) = idx.index_doc(
                    &DocMeta {
                        frn: i as u64,
                        volume_id: "bench".into(),
                        path: p.display().to_string(),
                        mtime: 0,
                        size: t.raw_bytes,
                    },
                    &t.text,
                ) {
                    eprintln!("[bench] 入库失败 {}: {e}", p.display());
                    failed += 1;
                } else {
                    extracted += 1;
                }
            }
            Err(_) => failed += 1,
        }
    }
    idx.commit().unwrap();
    let build_s = t0.elapsed().as_secs_f64();
    let idx_bytes = dir_size(idx_dir);
    println!(
        "[bench] 抽取 {extracted} / 跳过 {skipped} / 失败 {failed}，原文 {:.2} MB，耗时 {build_s:.1}s（{:.2} MB/s）",
        bytes as f64 / 1048576.0,
        if build_s > 0.0 {
            bytes as f64 / 1048576.0 / build_s
        } else {
            0.0
        }
    );
    println!(
        "[bench] 索引 {} docs / {:.2} MB，体积比 {:.1}%",
        idx.num_docs(),
        idx_bytes as f64 / 1048576.0,
        if bytes == 0 {
            0.0
        } else {
            idx_bytes as f64 * 100.0 / bytes as f64
        }
    );

    for q in ["fn", "全文", "error", "索引", "struct"] {
        // 预热：排除 jieba/mmap 首次开销。
        let _ = idx.search_ids(q, 10);
        let mut best_ids = u128::MAX;
        let mut best_full = u128::MAX;
        let mut n = 0;
        for _ in 0..20 {
            let t = Instant::now();
            n = idx.search_ids(q, 10).map(|h| h.len()).unwrap_or(0);
            best_ids = best_ids.min(t.elapsed().as_micros());
            let t = Instant::now();
            let _ = idx.search(q, 10);
            best_full = best_full.min(t.elapsed().as_micros());
        }
        println!("[bench] 查询 {q:?}: {n} hits，ids {best_ids}μs / 含snippet {best_full}μs（20 次最佳）");
    }
}

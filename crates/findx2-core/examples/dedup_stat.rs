//! 构建期名字去重（interning）的真实数据验证工具。
//!
//! MFT 枚举需要管理员权限，本 example 改走普通 `std::fs` 递归枚举
//! （用户目录与 C:\Windows 均可读），把真实文件名喂给同一条
//! `build_from_raw` 管线，对比去重前后的 `names_buf` 体积：
//!
//! ```text
//! cargo run -p findx2-core --release --example dedup_stat -- C:\Users\<you> C:\Windows
//! ```

use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use findx2_core::index::IndexBuilder;
use findx2_core::platform::RawEntry;

fn raw(id: u64, parent: u64, name: &str, is_dir: bool, size: u64) -> RawEntry {
    RawEntry {
        file_id: id,
        file_id_128: None,
        parent_id: parent,
        name: name.to_string(),
        size,
        mtime: 0,
        ctime: 0,
        attrs: if is_dir { 0x10 } else { 0 },
        is_dir,
    }
}

fn display_name(p: &Path) -> String {
    p.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.to_string_lossy().into_owned())
}

fn main() {
    let args: Vec<PathBuf> = std::env::args().skip(1).map(PathBuf::from).collect();
    if args.is_empty() {
        eprintln!("用法: dedup_stat <目录> [目录2 ...]");
        std::process::exit(2);
    }

    let mut dirs: Vec<RawEntry> = Vec::new();
    let mut files: Vec<RawEntry> = Vec::new();
    let mut raw_name_bytes: usize = 0; // 去重前：Σ(len+1)
    let mut next_id: u64 = 2; // 0 保留给「根的父」
    let mut skipped = 0usize;

    let mut queue: VecDeque<(PathBuf, u64)> = VecDeque::new();
    for root in &args {
        let id = next_id;
        next_id += 1;
        let name = display_name(root);
        raw_name_bytes += name.len() + 1;
        dirs.push(raw(id, 0, &name, true, 0));
        queue.push_back((root.clone(), id));
    }

    let t0 = std::time::Instant::now();
    while let Some((path, parent_id)) = queue.pop_front() {
        let rd = match std::fs::read_dir(&path) {
            Ok(rd) => rd,
            Err(_) => {
                skipped += 1;
                continue;
            }
        };
        for entry in rd.flatten() {
            let Ok(ft) = entry.file_type() else { continue };
            let name = entry.file_name().to_string_lossy().into_owned();
            let id = next_id;
            next_id += 1;
            raw_name_bytes += name.len() + 1;
            if ft.is_dir() {
                dirs.push(raw(id, parent_id, &name, true, 0));
                queue.push_back((entry.path(), id));
            } else {
                let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                files.push(raw(id, parent_id, &name, false, size));
            }
        }
    }
    let scan_secs = t0.elapsed().as_secs_f64();
    let total = dirs.len() + files.len();
    eprintln!(
        "枚举完成：{} 目录 + {} 文件 = {} 名字（{:.1}s，跳过 {} 个不可读目录）",
        dirs.len(),
        files.len(),
        total,
        scan_secs,
        skipped
    );

    let store = IndexBuilder::new(b'C', 1, 1, 1)
        .build_from_raw(files, dirs, true)
        .expect("build_from_raw");

    let deduped = store.names_buf.len();
    let saved = raw_name_bytes.saturating_sub(deduped);
    println!(
        "名字写入 {:>10} 次 | 去重前 {:>9} B | 去重后 {:>9} B | 省 {:>6.1} MB ({:.1}%)",
        total,
        raw_name_bytes,
        deduped,
        saved as f64 / 1048576.0,
        saved as f64 * 100.0 / raw_name_bytes.max(1) as f64
    );
    println!(
        "entries {:>9} | names_buf 占比 {:.1} B/条目（去重前 {:.1} B/条目）",
        store.entries.len(),
        deduped as f64 / store.entries.len().max(1) as f64,
        raw_name_bytes as f64 / store.entries.len().max(1) as f64
    );
}

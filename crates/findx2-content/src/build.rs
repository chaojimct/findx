//! 全量构建：文件名索引 → 内容倒排（CLI `content-index` 与未来 GUI 共用）。
//!
//! 单线程 M1（IO 主导，瓶颈在抽取；rayon 化是 M2 优化项）。
//! 分批 commit：bound writer 内存，崩溃最多丢一批（下次重建/增量补回）。

use std::path::Path;

use findx2_core::{volume_key_for, IndexStore};

use crate::{extract_text, open_or_create, should_extract, DocMeta, Error, Result};

#[derive(Debug, Default, Clone)]
pub struct BuildStats {
    pub scanned: u64,
    pub extracted: u64,
    pub skipped: u64,
    pub failed: u64,
    pub bytes: u64,
    pub docs: u64,
}

/// `cdir` 不存在则创建；`rebuild=true` 且存在则先删。
/// `progress` 每 ~5 秒回调一次（CLI 打 eprintln，GUI 可接进度条）。
pub fn build_from_store(
    store: &IndexStore,
    cdir: &Path,
    rebuild: bool,
    mut progress: impl FnMut(&BuildStats),
) -> Result<BuildStats> {
    if rebuild && cdir.exists() {
        std::fs::remove_dir_all(cdir).map_err(|e| Error::Io(cdir.display().to_string(), e))?;
    }
    // 卷前缀（小写）→ 归一化卷键：条目按最长前缀归属卷。
    let vols: Vec<(String, String)> = store
        .volumes
        .iter()
        .enumerate()
        .map(|(i, v)| {
            (
                v.root_prefix.to_lowercase(),
                volume_key_for(&v.volume_id, i),
            )
        })
        .collect();
    let cidx = open_or_create(cdir)?;
    let mut st = BuildStats::default();
    let mut since_commit = 0u64;
    let mut last_log = std::time::Instant::now();
    for idx in 0..store.entries.len() {
        let e = &store.entries[idx];
        if e.is_dir_entry() || e.is_deleted() {
            continue;
        }
        st.scanned += 1;
        let p = match store.entry_display_path(idx) {
            Ok(p) => p,
            Err(_) => {
                st.failed += 1;
                continue;
            }
        };
        let pl = p.to_lowercase();
        let mut slot = 0usize;
        let mut best = 0usize;
        for (i, (pre, _)) in vols.iter().enumerate() {
            if !pre.is_empty() && pl.starts_with(pre) && pre.len() >= best {
                best = pre.len();
                slot = i;
            }
        }
        if slot >= vols.len() {
            st.failed += 1;
            continue;
        }
        let path = Path::new(&p);
        if should_extract(path).is_none() {
            st.skipped += 1;
            continue;
        }
        let frn = store.frns.get(idx).copied().unwrap_or(0);
        match extract_text(path) {
            Ok(t) => {
                let ok = cidx
                    .index_doc(
                        &DocMeta {
                            frn,
                            volume_id: vols[slot].1.clone(),
                            path: p,
                            mtime: e.mtime as u64,
                            size: e.size,
                        },
                        &t.text,
                    )
                    .is_ok();
                if !ok {
                    st.failed += 1;
                    continue;
                }
                st.extracted += 1;
                st.bytes += t.raw_bytes;
                since_commit += 1;
                if since_commit >= 20_000 {
                    cidx.commit()?;
                    since_commit = 0;
                }
            }
            Err(_) => st.failed += 1,
        }
        if last_log.elapsed().as_secs() >= 5 {
            progress(&st);
            last_log = std::time::Instant::now();
        }
    }
    cidx.commit()?;
    st.docs = cidx.num_docs();
    progress(&st);
    Ok(st)
}

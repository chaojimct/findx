//! Bounded indexed-usage page to GUI render model. No filesystem scan or allocated-size inference.
use crate::disk_usage::{self, Scan, Limits};
use std::{path::PathBuf, time::Duration};
pub fn scan_from_page(page: &findx2_ipc::UsagePageDto, requested: PathBuf) -> Scan {
    let degraded=page.watch_error.is_some() || page.metadata_error.is_some();
    let convert =
        |n: &findx2_ipc::UsageItemDto, path: PathBuf, parent: Option<usize>| {
            disk_usage::Node {
                path,
                parent,
                children: vec![],
                entries: n.files + n.directories,
                files: n.files,
                directories: n.directories,
                category: 0,
                own_allocated: 0,
                directory: n.directory,
                logical: n.logical,
                mtime: 0,
                allocated: 0,
                hardlink_duplicate: false,
                uncertain: n.unknown > 0,
            }
        };
    let mut nodes = vec![convert(&page.root, requested.clone(), None)];
    for child in &page.children {
        nodes.push(convert(child, requested.join(&child.name), Some(0)));
    }
    nodes[0].children = (1..nodes.len()).collect();
    let scan = Scan {
        logical_generation: Some((page.epoch, page.generation)),
        nodes,
        errors: 0,
        skipped_links: 0,
        hardlinks: 0,
        unknown: page.root.unknown,
        partial: page.root.unknown > 0 || page.next.is_some() || degraded,
        cancelled: false,
        elapsed: Duration::ZERO,
        entry_limit_reached: false,
        time_limit_reached: false,
        memory_limit_reached: false,
        alloc_failed: false,
        limits: Limits::default(),
        volume: None,
    };
    scan
}

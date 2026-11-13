//! Explicit fixture backend. This module never probes a volume, journal or real index.
use findx2_core::{ChangeEvent, IndexBuilder, RawEntry, SearchEngine};
use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};
pub fn create_index(root: &Path, index: &Path) -> anyhow::Result<()> {
    if index.exists() {
        anyhow::bail!("fixture index already exists; refusing overwrite");
    }
    let canonical = root.canonicalize()?;
    let root = PathBuf::from(
        canonical
            .to_string_lossy()
            .trim_start_matches(r"\\?\")
            .to_owned(),
    );
    if root.parent().is_none() || !root.is_dir() {
        anyhow::bail!("fixture must be explicit non-volume directory");
    }
    let mut files = Vec::new();
    let mut dirs = Vec::new();
    let mut paths = vec![(root.clone(), 0u64)];
    let mut id = 1;
    let mut manifest = Vec::new();
    while let Some((path, parent)) = paths.pop() {
        let md = std::fs::symlink_metadata(&path)?;
        if md.file_type().is_symlink() {
            continue;
        }
        let frn = id;
        id += 1;
        let is_dir = md.is_dir();
        let raw = RawEntry {
            file_id: frn,
            file_id_128: None,
            parent_id: parent,
            name: path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            size: if is_dir { 0 } else { md.len() },
            mtime: 0,
            ctime: 0,
            attrs: if is_dir { 0x10 } else { 0 },
            is_dir,
        };
        manifest.push(serde_json::json!({"id":frn,"parent":parent,"path":path.to_string_lossy(),"directory":is_dir,"size":raw.size}));
        if is_dir {
            dirs.push(raw);
            let mut children = std::fs::read_dir(&path)?.collect::<Result<Vec<_>, _>>()?;
            children.sort_by_key(|e| e.file_name());
            for child in children.into_iter().rev() {
                paths.push((child.path(), frn));
            }
        } else {
            files.push(raw);
        }
    }
    let mut store = IndexBuilder::new(b'F', 0xF197, 0, 0).build_from_raw(files, dirs, true)?;
    store.volumes[0].root_prefix = format!("{}\\", root.display());
    store.volumes[0].volume_id = "fixture-synthetic-not-a-live-volume".into();
    if let Some(parent) = index.parent() {
        std::fs::create_dir_all(parent)?;
    }
    findx2_core::save_index_bin(index, &store)?;
    std::fs::write(
        index.with_extension("fixture-manifest.json"),
        serde_json::to_vec_pretty(&manifest)?,
    )?;
    Ok(())
}
struct Command {
    op: String,
    file_id: u64,
    parent_id: u64,
    name: String,
    size: u64,
    directory: bool,
}
pub fn spawn_events(engine: Arc<SearchEngine>, script: PathBuf, stop: Arc<AtomicBool>) {
    std::thread::spawn(move || {
        let mut consumed = 0;
        while !stop.load(Ordering::Acquire) {
            if let Ok(text) = std::fs::read_to_string(&script) {
                for line in text.lines().skip(consumed) {
                    consumed += 1;
                    let result = (|| -> anyhow::Result<()> {
                        let v: serde_json::Value = serde_json::from_str(line)?;
                        let c = Command {
                            op: v["op"].as_str().unwrap_or_default().into(),
                            file_id: v["file_id"].as_u64().unwrap_or_default(),
                            parent_id: v["parent_id"].as_u64().unwrap_or_default(),
                            name: v["name"].as_str().unwrap_or_default().into(),
                            size: v["size"].as_u64().unwrap_or_default(),
                            directory: v["directory"].as_bool().unwrap_or(false),
                        };
                        let ev = match c.op.as_str() {
                            "create" => ChangeEvent::Create {
                                entry: RawEntry {
                                    file_id: c.file_id,
                                    file_id_128: None,
                                    parent_id: c.parent_id,
                                    name: c.name,
                                    size: c.size,
                                    mtime: 0,
                                    ctime: 0,
                                    attrs: if c.directory { 0x10 } else { 0 },
                                    is_dir: c.directory,
                                },
                            },
                            "pending" => ChangeEvent::CreatePending {
                                file_id: c.file_id, file_id_128: None, parent_id: c.parent_id,
                                name: c.name, attrs: if c.directory {0x10} else {0}, is_dir:c.directory,
                            },
                            "delete" => ChangeEvent::Delete { file_id: c.file_id },
                            "rename" => ChangeEvent::Rename {
                                file_id: c.file_id,
                                new_parent_id: c.parent_id,
                                new_name: c.name,
                            },
                            "size" => ChangeEvent::DataOrMeta {
                                file_id: c.file_id,
                                size: Some(c.size),
                                mtime: None,
                                ctime: None,
                                create_if_missing: None,
                            },
                            _ => anyhow::bail!("unknown fixture command"),
                        };
                        engine
                            .apply_volume_change(b'F', &ev)
                            .map_err(anyhow::Error::msg)
                    })();
                    if let Err(e) = result {
                        tracing::error!("fixture event rejected: {e}");
                    } else {
                        tracing::info!("fixture event committed");
                    }
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    });
}

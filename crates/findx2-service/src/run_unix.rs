//! Unix 前台：加载索引、Unix socket、平台增量监听、周期性落盘。

use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use findx2_core::{load_index_bin, save_index_bin, ChangeEvent, SearchEngine, WatchCursor};
use tracing::{error, info, warn};

use crate::ipc_dispatch::EngineSlot;
use crate::watch_health::set_watch_error_id;

pub(crate) fn run_foreground(
    index: PathBuf,
    volume: String,
    pipe_name: String,
    save_interval_secs: u64,
    extra_excluded: Vec<String>,
) -> anyhow::Result<()> {
    let socket = findx2_ipc::unix_socket_path(&pipe_name);
    info!("Unix 服务：index={} socket={}", index.display(), socket.display());

    let slot: EngineSlot = Arc::new(RwLock::new(None));
    let slot_ipc = slot.clone();
    let sock2 = socket.clone();
    std::thread::Builder::new()
        .name("findx2-unix-ipc".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("tokio");
            if let Err(e) = rt.block_on(crate::unix_server::unix_accept_loop(sock2, slot_ipc)) {
                error!("Unix socket 退出: {e:#}");
            }
        })?;

    let mut store = if index.exists() {
        load_index_bin(&index)?
    } else {
        info!("index.bin 不存在，开始全量建库…");
        build_unix_index(&index, &volume, &extra_excluded)?
    };
    if !extra_excluded.is_empty() {
        store.excluded_dirs = extra_excluded.clone();
        store.mark_excluded_entries(&extra_excluded);
    }

    let engine = Arc::new(SearchEngine::new(store));
    {
        let mut g = slot.write().map_err(|e| anyhow::anyhow!("{e}"))?;
        *g = Some(engine.clone());
    }

    let volume = normalize_unix_volume(&volume);
    let roots: Vec<String> = {
        let g = engine.index_store();
        let from_index: Vec<String> = g
            .volumes
            .iter()
            .map(|v| watch_root_for_volume(&v.root_prefix, &volume))
            .collect();
        if from_index.is_empty() {
            vec![watch_root_for_volume("", &volume)]
        } else {
            from_index
        }
    };
    info!("Unix 监听根路径：{:?}", roots);

    let (tx, rx) = mpsc::channel::<ChangeEvent>();
    let watch_roots = roots.clone();
    let watch_cursor = {
        let g = engine.index_store();
        g.volumes
            .first()
            .map(|v| WatchCursor {
                watch_gen: v.usn_journal_id,
                watch_cursor: v.last_usn,
            })
            .unwrap_or_default()
    };
    std::thread::Builder::new()
        .name("findx2-unix-watch".into())
        .spawn(move || {
            if let Err(e) = platform_watch(&watch_roots, watch_cursor, tx) {
                warn!("增量监听结束: {e}");
                set_watch_error_id("unix", Some(e.to_string()));
            }
        })?;

    let save_every = Duration::from_secs(save_interval_secs.max(5));
    let mut last_save = Instant::now();
    loop {
        match rx.recv_timeout(Duration::from_secs(1)) {
            Ok(ev) => {
                if matches!(&ev, ChangeEvent::CreatePending { .. } | ChangeEvent::Create { .. } | ChangeEvent::Delete { .. } | ChangeEvent::Rename { .. } | ChangeEvent::DataOrMeta { .. })
                {
                    let mut g = engine.index_store_mut();
                    if let Err(e) = g.apply_change_event(&ev) {
                        warn!("应用增量失败: {e}");
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                error!("监听通道断开，尝试重建卷…");
                set_watch_error_id("unix", Some("增量监听中断，正在重建".into()));
                match rebuild_unix(&engine, &index, &roots) {
                    Ok(()) => set_watch_error_id("unix", None),
                    Err(e) => {
                        set_watch_error_id("unix", Some(e.to_string()));
                        std::thread::sleep(Duration::from_secs(5));
                    }
                }
            }
        }
        if last_save.elapsed() >= save_every {
            persist(&engine, &index)?;
            last_save = Instant::now();
        }
    }
}

fn persist(engine: &SearchEngine, path: &PathBuf) -> anyhow::Result<()> {
    let store = engine.index_store();
    save_index_bin(path, &store)?;
    Ok(())
}

/// clap 在 Unix 上仍默认 `--volume C:`；把它当成「未指定」，走平台默认扫描根。
pub(crate) fn normalize_unix_volume(volume: &str) -> String {
    let v = volume.trim();
    if v.is_empty() || v.eq_ignore_ascii_case("C:") || v.eq_ignore_ascii_case(r"C:\") {
        String::new()
    } else {
        v.to_string()
    }
}

fn default_unix_watch_root() -> String {
    #[cfg(target_os = "macos")]
    {
        findx2_macos::default_scan_root()
    }
    #[cfg(target_os = "linux")]
    {
        findx2_linux::default_scan_root()
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        "/".into()
    }
}

/// Data 卷在索引里的 `root_prefix` 是 `/`（给用户看的路径），监听必须落到真实扫描根，
/// 绝不能把 Windows 默认盘符 `C:` 交给 FSEvents。
fn watch_root_for_volume(root_prefix: &str, volume: &str) -> String {
    let prefix = root_prefix.trim();
    if prefix.len() > 1 {
        return prefix.to_string();
    }
    let vol = normalize_unix_volume(volume);
    if !vol.is_empty() {
        return vol;
    }
    default_unix_watch_root()
}

fn build_unix_index(
    output: &PathBuf,
    volume: &str,
    extra_excluded: &[String],
) -> anyhow::Result<findx2_core::IndexStore> {
    let roots = if normalize_unix_volume(volume).is_empty() {
        Vec::new()
    } else {
        volume
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    };
    let exclude = extra_excluded.to_vec();
    #[cfg(target_os = "macos")]
    {
        return findx2_macos::build_full_disk_index(output, roots, exclude)
            .map_err(|e| anyhow::anyhow!("{e}"));
    }
    #[cfg(target_os = "linux")]
    {
        return findx2_linux::build_full_disk_index(output, roots, exclude)
            .map_err(|e| anyhow::anyhow!("{e}"));
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (output, roots, exclude);
        anyhow::bail!("当前 Unix 平台未实现建库");
    }
}

fn platform_watch(
    roots: &[String],
    cursor: WatchCursor,
    tx: mpsc::Sender<ChangeEvent>,
) -> findx2_core::Result<WatchCursor> {
    #[cfg(target_os = "macos")]
    {
        let root = roots
            .first()
            .cloned()
            .unwrap_or_else(default_unix_watch_root);
        return findx2_macos::watch_loop(&root, cursor, tx);
    }
    #[cfg(target_os = "linux")]
    {
        return findx2_linux::watch_loop(roots, cursor, tx);
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (roots, cursor, tx);
        Err(findx2_core::Error::Platform("无 Unix 监听后端".into()))
    }
}

fn rebuild_unix(engine: &Arc<SearchEngine>, index: &PathBuf, roots: &[String]) -> anyhow::Result<()> {
    info!("Unix 卷重建：{:?}", roots);
    let fresh = build_unix_index(index, &roots.join(","), &[])?;
    {
        let mut g = engine.index_store_mut();
        *g = fresh;
    }
    persist(engine, index)?;
    Ok(())
}

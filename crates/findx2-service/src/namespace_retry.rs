//! Retry preparation only: no mutation/cursor acknowledgement until complete.
//! The caller owns ONE unchanged batch; a bounded channel backpressures readers.
pub(crate) fn prepare_with_retry<T>(
    mut prepare: impl FnMut() -> Result<T, String>,
    retryable: impl Fn(&str) -> bool,
    mut wait: impl FnMut(u32) -> bool,
) -> Result<Option<T>, String> {
    let mut attempt = 0u32;
    loop {
        match prepare() {
            Ok(value) => return Ok(Some(value)),
            Err(error) if retryable(&error) => {
                attempt = attempt.saturating_add(1);
                if !wait(attempt) {
                    return Ok(None);
                }
            }
            Err(error) => return Err(error),
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn old_full_frn_delete_preserves_reused_ordinal_identity() {
        use findx2_core::{ChangeEvent, IndexBuilder, RawEntry, SearchEngine};
        let old = (7u64 << 48) | 42;
        let new = (8u64 << 48) | 42;
        let entry = |id, name: &str, is_dir, parent| RawEntry {
            file_id: id,
            file_id_128: None,
            parent_id: parent,
            name: name.into(),
            size: 10,
            mtime: 1,
            ctime: 1,
            attrs: if is_dir { 16 } else { 0 },
            is_dir,
        };
        let store = IndexBuilder::new(b'C', 1, 7, 100)
            .build_from_raw(
                vec![
                    entry(old, "old.txt", false, 100),
                    entry(new, "new.txt", false, 100),
                ],
                vec![entry(100, "root", true, 0)],
                true,
            )
            .unwrap();
        let engine = SearchEngine::new(store);
        engine
            .apply_volume_batch(b'C', &[ChangeEvent::Delete { file_id: old }], 7, 200)
            .unwrap();
        let g = engine.index_store();
        assert!(g.entries[g.entry_on_volume(b'C', old).unwrap() as usize].is_deleted());
        assert!(!g.entries[g.entry_on_volume(b'C', new).unwrap() as usize].is_deleted());
        assert_eq!(g.volumes[0].last_usn, 200);
    }
    #[test]
    fn identical_batch_retried_then_published_once() {
        let original = vec![7u64, 9, 11];
        let mut seen = Vec::new();
        let mut sleeps = 0;
        let prepared = prepare_with_retry(
            || {
                seen.push(original.clone());
                if seen.len() < 3 {
                    Err("pending".into())
                } else {
                    Ok(original.clone())
                }
            },
            |e| e == "pending",
            |_| {
                sleeps += 1;
                true
            },
        )
        .unwrap()
        .unwrap();
        assert_eq!(seen, vec![original.clone(); 3]);
        assert_eq!(prepared, original);
        assert_eq!(sleeps, 2);
    }
    #[test]
    fn shutdown_retains_unprocessed_batch() {
        let mut attempts = 0;
        let r = prepare_with_retry::<Vec<u64>>(
            || {
                attempts += 1;
                Err("pending".into())
            },
            |_| true,
            |_| false,
        )
        .unwrap();
        assert!(r.is_none());
        assert_eq!(attempts, 1);
    }
    #[test]
    fn arbitrary_denial_not_retried() {
        let mut attempts = 0;
        let r = prepare_with_retry::<()>(
            || {
                attempts += 1;
                Err("access denied".into())
            },
            |e| e == "pending",
            |_| panic!("must not retry"),
        );
        assert!(r.is_err());
        assert_eq!(attempts, 1);
    }
}

#[cfg(all(test, windows))]
mod native_owned_retry {
    use super::prepare_with_retry;
    use std::{
        fs::{File, OpenOptions},
        mem::size_of,
        os::windows::{
            fs::OpenOptionsExt,
            io::{AsRawHandle, FromRawHandle},
        },
        time::Duration,
    };
    use windows::Win32::{Foundation::HANDLE, Storage::FileSystem::*};
    fn h(file: &File) -> HANDLE {
        HANDLE(file.as_raw_handle() as _)
    }
    fn identity(file: &File) -> u64 {
        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        unsafe { GetFileInformationByHandle(h(file), &mut info) }.unwrap();
        (info.nFileIndexHigh as u64) << 32 | info.nFileIndexLow as u64
    }
    fn disposition(file: &File, delete: bool) {
        let d = FILE_DISPOSITION_INFO { DeleteFile: delete };
        unsafe {
            SetFileInformationByHandle(
                h(file),
                FileDispositionInfo,
                (&d as *const FILE_DISPOSITION_INFO).cast(),
                size_of::<FILE_DISPOSITION_INFO>() as u32,
            )
        }
        .unwrap();
    }
    #[test]
    fn real_ntfs_delete_pending_recovers_same_identity_without_fencing_or_deletion() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "findx-t176-owned-retry-{}-{stamp}",
            std::process::id()
        ));
        std::fs::create_dir(&root).unwrap();
        let path = root.join("owned-only.txt");
        std::fs::write(&path, b"owned retry sentinel").unwrap();
        let hint = OpenOptions::new()
            .read(true)
            .share_mode(7)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS.0)
            .open(&root)
            .unwrap();
        let retained = OpenOptions::new()
            .read(true)
            .share_mode(7)
            .open(&path)
            .unwrap();
        let id = identity(&retained);
        let delete_handle = OpenOptions::new()
            .access_mode(0x10000 | 0x80)
            .share_mode(7)
            .open(&path)
            .unwrap();
        disposition(&delete_handle, true);
        let (tx, rx) = std::sync::mpsc::sync_channel::<()>(1);
        let cancel = std::thread::spawn(move || {
            rx.recv().unwrap();
            std::thread::sleep(Duration::from_millis(100));
            disposition(&delete_handle, false);
        });
        let mut tx = Some(tx);
        let mut failures = 0;
        let result = prepare_with_retry(
            || match findx2_windows::open_file_by_id_diagnostic(h(&hint), id) {
                Ok(raw) => {
                    let file = unsafe { File::from_raw_handle(raw.0 as _) };
                    assert_eq!(identity(&file), id);
                    Ok(id)
                }
                Err(error) => {
                    assert!(
                        error.is_transient_delete_pending(),
                        "unexpected native status: {error:?}"
                    );
                    failures += 1;
                    Err(
                        "bound unlink identity temporarily delete-pending: owned native fixture"
                            .into(),
                    )
                }
            },
            findx2_windows::is_retryable_namespace_error,
            |attempt| {
                if let Some(tx) = tx.take() {
                    tx.send(()).unwrap();
                }
                if attempt >= 100 {
                    return false;
                }
                std::thread::sleep(Duration::from_millis(20));
                true
            },
        )
        .unwrap();
        if let Some(tx) = tx.take() {
            let _ = tx.send(());
        }
        cancel.join().unwrap();
        assert_eq!(result, Some(id));
        assert!(failures > 0);
        assert_eq!(std::fs::read(&path).unwrap(), b"owned retry sentinel");
        println!(
            "T176_REAL_NTFS_RETRY path={} retries={} same_identity={} content_preserved=true",
            root.display(),
            failures,
            id
        );
        drop(retained);
        drop(hint);
        std::fs::remove_file(path).unwrap();
        std::fs::remove_dir(root).unwrap();
    }
}

//! Isolated transactional publish faults. No live watcher, official install, or user paths.
use findx2_core::{
    clear_persist_fault, generation_dir, generation_disk_bytes, read_published,
    set_persist_fault, v6_load, v6_save_full, v6_try_commit, CommitBatch, CommitOutcome, IndexBuilder,
    PersistFault, RawEntry, V6Layout,
};
use std::path::PathBuf;

fn tmp(label: &str) -> (PathBuf, V6Layout) {
    let dir = std::env::temp_dir().join(format!(
        "findx-persist-tx-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("synthetic.bin");
    (dir, V6Layout::for_index(&path))
}
fn sample() -> findx2_core::IndexStore {
    let raw = |id, parent, name: &str, dir| RawEntry {
        file_id: id, file_id_128: None, parent_id: parent, name: name.into(),
        size: if dir { 0 } else { 10 }, mtime: 1, ctime: 1, attrs: if dir { 0x10 } else { 0 }, is_dir: dir,
    };
    IndexBuilder::new(b'C', 1, 7, 10)
        .build_from_raw(vec![raw(2, 1, "public-file.txt", false)], vec![raw(1, 0, "root", true)], true)
        .unwrap()
}
fn batch_of(store: &mut findx2_core::IndexStore) -> CommitBatch {
    CommitBatch {
        mutations: store.take_mutations(),
        entry_count: store.entries.len(),
        dir_count: store.dirs.len(),
        names_len: store.names_buf.len(),
        dir_paths_len: store.dir_paths_buf.len(),
        metadata_ready: store.metadata_ready,
        volumes: store.volumes.clone(),
        tri_pending: store.tri_pending.clone(),
        tri_snap: 0,
        has_trigram: false,
        wal_seq: 0,
    }
}
fn patch_size(store: &mut findx2_core::IndexStore, size: u64) {
    store.set_track_mutations(true);
    store.apply_change_event(&findx2_core::ChangeEvent::DataOrMeta {
        file_id: 2, size: Some(size), mtime: None, ctime: None, create_if_missing: None,
    }).unwrap();
}

fn assert_old(layout: &V6Layout, size: u64, usn: u64) {
    let loaded = v6_load(layout).unwrap();
    assert_eq!(loaded.entries[loaded.entry_on_volume(b'C', 2).unwrap() as usize].size, size);
    assert_eq!(loaded.volumes[0].last_usn, usn);
}

#[test]
fn before_publish_faults_restore_complete_old_state() {
    let (dir, layout) = tmp("before");
    let store = sample();
    v6_save_full(&layout, &store).unwrap();
    let first = read_published(&layout).unwrap().unwrap().manifest.generation;
    for fault in [
        PersistFault::BeforePublish,
        PersistFault::AfterDataSync,
        PersistFault::RenameFail,
        PersistFault::ShortWrite { file: "entries.dat".into(), max_bytes: 8 },
        PersistFault::DiskFull { max_bytes: 32 },
        PersistFault::AccessDenied { file: "entries.dat".into() },
    ] {
        let mut working = v6_load(&layout).unwrap();
        patch_size(&mut working, 99);
        working.volumes[0].last_usn = 20;
        set_persist_fault(fault.clone());
        let batch = batch_of(&mut working);
        assert!(v6_try_commit(&layout, &batch).is_err(), "fault {fault:?} must fail");
        clear_persist_fault();
        assert_old(&layout, 10, 10);
        assert_eq!(read_published(&layout).unwrap().unwrap().manifest.generation, first);
    }
    let mut store = v6_load(&layout).unwrap();
    patch_size(&mut store, 99);
    store.volumes[0].last_usn = 20;
    let batch = batch_of(&mut store);
    assert!(matches!(v6_try_commit(&layout, &batch).unwrap(), CommitOutcome::Delta { .. }));
    assert_old(&layout, 99, 20);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn after_publish_load_is_complete_new_state_and_restart_is_idempotent() {
    let (dir, layout) = tmp("after");
    let mut store = sample();
    v6_save_full(&layout, &store).unwrap();
    patch_size(&mut store, 77);
    store.volumes[0].last_usn = 11;
    let batch = batch_of(&mut store);
    let CommitOutcome::Delta { generation, .. } = v6_try_commit(&layout, &batch).unwrap() else { panic!("delta") };
    assert_old(&layout, 77, 11);
    assert_eq!(v6_load(&layout).unwrap().entries[0].size, v6_load(&layout).unwrap().entries[0].size);
    // empty retry
    store.set_track_mutations(true);
    let empty = batch_of(&mut store);
    assert!(matches!(v6_try_commit(&layout, &empty).unwrap(), CommitOutcome::Delta { applied: 0, generation: g, .. } if g == generation));
    assert_old(&layout, 77, 11);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn missing_sidecar_and_bad_crc_after_publish_do_not_mix_legacy_bytes() {
    let (dir, layout) = tmp("crc");
    let store = sample();
    v6_save_full(&layout, &store).unwrap();
    std::fs::write(layout.dir.join("entries.dat"), [1u8; 8]).unwrap();
    let ptr = read_published(&layout).unwrap().unwrap();
    let live = generation_dir(&layout, ptr.manifest.generation);
    let pending = live.join("tri.pending");
    std::fs::write(&pending, b"not-a-sidecar").unwrap();
    // pending not in published file list unless has_trigram; add a listed file miss:
    let names = live.join("names.dat");
    let backup = std::fs::read(&names).unwrap();
    std::fs::remove_file(&names).unwrap();
    assert!(v6_load(&layout).is_err());
    std::fs::write(&names, backup).unwrap();
    let mut bytes = std::fs::read(live.join("entries.dat")).unwrap();
    bytes[0] ^= 0xFF;
    std::fs::write(live.join("entries.dat"), bytes).unwrap();
    assert!(v6_load(&layout).is_err());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn old_windows_mapping_keeps_previous_generation_bytes() {
    let (dir, layout) = tmp("map");
    let mut store = sample();
    let g1 = v6_save_full(&layout, &store).unwrap();
    let prev = generation_dir(&layout, g1).join("entries.dat");
    let kept = std::fs::read(&prev).unwrap();
    // Hold a shared mapping / open handle across the next publish, as an old reader would.
    let handle = std::fs::File::open(&prev).unwrap();
    patch_size(&mut store, 55);
    let batch = batch_of(&mut store);
    assert!(matches!(v6_try_commit(&layout, &batch).unwrap(), CommitOutcome::Delta { .. }));
    assert_eq!(std::fs::read(&prev).unwrap(), kept, "previous generation bytes must stay intact");
    let mut on_disk = vec![0u8; kept.len()];
    use std::io::{Read, Seek, SeekFrom};
    let mut h = handle;
    h.seek(SeekFrom::Start(0)).unwrap();
    h.read_exact(&mut on_disk).unwrap();
    assert_eq!(on_disk, kept);
    assert_old(&layout, 55, 10);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn generations_are_bounded_and_legacy_files_are_not_reclaimed() {
    let (dir, layout) = tmp("gc");
    let mut store = sample();
    v6_save_full(&layout, &store).unwrap();
    std::fs::write(layout.dir.join("legacy-keep.dat"), b"pre-migration-copy").unwrap();
    let mut last = 0u64;
    for size in 11..16 {
        patch_size(&mut store, size);
        let batch = batch_of(&mut store);
        let CommitOutcome::Delta { generation, .. } = v6_try_commit(&layout, &batch).unwrap() else { panic!() };
        last = generation;
    }
    let gens: Vec<u64> = std::fs::read_dir(layout.dir.join("gen")).unwrap().filter_map(|e| e.ok()?.file_name().to_str()?.parse().ok()).collect();
    assert!(gens.iter().all(|g| *g >= last - 1), "only current and previous generations kept: {gens:?}");
    assert!(gens.len() <= 2);
    assert_eq!(std::fs::read(layout.dir.join("legacy-keep.dat")).unwrap(), b"pre-migration-copy");
    let _cost = generation_disk_bytes(&layout);
    println!("generation_disk_bytes={}", _cost);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn isolated_child_killed_before_publish_restarts_from_old_generation() {
    let (dir, layout) = tmp("kill");
    let store = sample();
    v6_save_full(&layout, &store).unwrap();
    let barrier = dir.join("barrier.ready");
    let child_store = dir.join("child-flag.txt");
    std::fs::write(&child_store, b"ok").unwrap();
    let exe = std::env::current_exe().unwrap();
    let mut cmd = std::process::Command::new(exe);
    cmd.env("FINDX_TX_CHILD", "1")
        .env("FINDX_TX_DIR", layout.index_bin.as_os_str())
        .env("FINDX_PERSIST_FAULT", format!("barrier={}", barrier.display()))
        .args(["--ignored", "--exact", "isolated_child_worker", "--nocapture"]);
    let mut child = cmd.spawn().unwrap();
    let started = std::time::Instant::now();
    while !barrier.is_file() {
        if started.elapsed() > std::time::Duration::from_secs(30) {
            let _ = child.kill();
            panic!("isolated child did not reach the pre-publish barrier");
        }
        if let Some(status) = child.try_wait().unwrap() {
            panic!("isolated child exited before barrier: {status}");
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    child.kill().unwrap();
    child.wait().unwrap();
    assert_old(&layout, 10, 10);
    let mut store = v6_load(&layout).unwrap();
    patch_size(&mut store, 123);
    store.volumes[0].last_usn = 30;
    let batch = batch_of(&mut store);
    assert!(matches!(v6_try_commit(&layout, &batch).unwrap(), CommitOutcome::Delta { .. }));
    assert_old(&layout, 123, 30);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
#[ignore]
fn isolated_child_worker() {
    if std::env::var("FINDX_TX_CHILD").ok().as_deref() != Some("1") { return; }
    let path = PathBuf::from(std::env::var_os("FINDX_TX_DIR").unwrap());
    let layout = V6Layout::for_index(&path);
    let mut store = v6_load(&layout).unwrap();
    patch_size(&mut store, 99);
    store.volumes[0].last_usn = 20;
    let batch = batch_of(&mut store);
    let _ = v6_try_commit(&layout, &batch);
}

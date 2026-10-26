use findx2_core::index::{IndexBuilder, IndexStore};
use findx2_core::platform::{ChangeEvent, RawEntry};
use findx2_core::{
    load_store_auto, v6_load, v6_save_full, v6_try_commit, CommitBatch, CommitOutcome, V6Layout,
};

fn sample_store() -> IndexStore {
    let dirs = vec![
        RawEntry {
            file_id: 100,
            file_id_128: None,
            parent_id: 0,
            name: "Users".into(),
            size: 0,
            mtime: 0,
            ctime: 0,
            attrs: 0x10,
            is_dir: true,
        },
        RawEntry {
            file_id: 101,
            file_id_128: None,
            parent_id: 100,
            name: "Alice".into(),
            size: 0,
            mtime: 0,
            ctime: 0,
            attrs: 0x10,
            is_dir: true,
        },
    ];
    let files = vec![
        RawEntry {
            file_id: 1,
            file_id_128: None,
            parent_id: 101,
            name: "note.txt".into(),
            size: 10,
            mtime: 1,
            ctime: 1,
            attrs: 0,
            is_dir: false,
        },
        RawEntry {
            file_id: 2,
            file_id_128: None,
            parent_id: 101,
            name: "pic.png".into(),
            size: 20,
            mtime: 2,
            ctime: 2,
            attrs: 0,
            is_dir: false,
        },
    ];
    let b = IndexBuilder::new(b'C', 1, 2, 3);
    b.build_from_raw(files, dirs, true).unwrap()
}

fn tmp_index(name: &str) -> (PathBuf, V6Layout) {
    let mut p = std::env::temp_dir();
    p.push(format!("findx2_v6_{name}.bin"));
    let layout = V6Layout::for_index(&p);
    let _ = std::fs::remove_dir_all(&layout.dir);
    (p, layout)
}

use std::path::PathBuf;

fn batch_of(store: &mut IndexStore) -> CommitBatch {
    let (tri_snap, has_trigram) = match store.trigram.as_ref() {
        Some(t) => (t.snapshot_entry_count(), true),
        None => (0, false),
    };
    CommitBatch {
        mutations: store.take_mutations(),
        entry_count: store.entries.len(),
        dir_count: store.dirs.len(),
        names_len: store.names_buf.len(),
        dir_paths_len: store.dir_paths_buf.len(),
        metadata_ready: store.metadata_ready,
        volumes: store.volumes.clone(),
        tri_pending: store.tri_pending.clone(),
        tri_snap,
        has_trigram,
    }
}

#[test]
fn v6_full_roundtrip_preserves_derived_state() {
    let (_p, layout) = tmp_index("full");
    let store = sample_store();
    v6_save_full(&layout, &store).unwrap();
    let loaded = v6_load(&layout).unwrap();
    assert_eq!(loaded.entry_count(), store.entry_count());
    assert_eq!(loaded.dirs.len(), store.dirs.len());
    assert_eq!(loaded.frns, store.frns);
    assert_eq!(loaded.names_buf, store.names_buf);
    assert_eq!(loaded.entries, store.entries);
    assert_eq!(loaded.deleted.len(), store.deleted.len());
    for b in 0..256 {
        assert_eq!(
            loaded.ext_filter[b].as_ref().map(|x| x.len()),
            store.ext_filter[b].as_ref().map(|x| x.len()),
            "ext bucket {b} mismatch"
        );
    }
    let _ = std::fs::remove_dir_all(&layout.dir);
}

#[test]
fn v6_delta_commit_meta_patch_and_reload() {
    let (_p, layout) = tmp_index("meta");
    let mut store = sample_store();
    v6_save_full(&layout, &store).unwrap();

    store.set_track_mutations(true);
    // 模拟日常 mtime 抖动：改第一个文件
    store
        .apply_change_event(&ChangeEvent::DataOrMeta {
            file_id: 1,
            size: Some(11),
            mtime: Some(1_000_000_000),
            ctime: None,
        })
        .unwrap();
    let batch = batch_of(&mut store);
    assert!(!batch.mutations.is_empty());
    match v6_try_commit(&layout, &batch).unwrap() {
        CommitOutcome::Delta { applied, generation, .. } => {
            assert_eq!(applied, batch.mutations.len());
            assert!(generation >= 2);
        }
        CommitOutcome::NeedFull { reason } => panic!("unexpected NeedFull: {reason}"),
    }
    let loaded = v6_load(&layout).unwrap();
    assert_eq!(loaded.entries[store.frn_to_entry.get(&1).copied().unwrap() as usize].size, 11);
    let _ = std::fs::remove_dir_all(&layout.dir);
}

#[test]
fn v6_delta_commit_structural_changes_and_delete() {
    let (_p, layout) = tmp_index("struct");
    let mut store = sample_store();
    v6_save_full(&layout, &store).unwrap();
    store.set_track_mutations(true);

    // 新增文件 + 新增目录 + 改名 + 删除
    store
        .apply_change_event(&ChangeEvent::Create {
            entry: RawEntry {
                file_id: 3,
                file_id_128: None,
                parent_id: 101,
                name: "new.md".into(),
                size: 30,
                mtime: 3,
                ctime: 3,
                attrs: 0,
                is_dir: false,
            },
        })
        .unwrap();
    store
        .apply_change_event(&ChangeEvent::Create {
            entry: RawEntry {
                file_id: 102,
                file_id_128: None,
                parent_id: 100,
                name: "Bob".into(),
                size: 0,
                mtime: 0,
                ctime: 0,
                attrs: 0x10,
                is_dir: true,
            },
        })
        .unwrap();
    store
        .apply_change_event(&ChangeEvent::Rename {
            file_id: 2,
            new_parent_id: 101,
            new_name: "pic2.png".into(),
        })
        .unwrap();
    store
        .apply_change_event(&ChangeEvent::Delete { file_id: 1 })
        .unwrap();

    let batch = batch_of(&mut store);
    match v6_try_commit(&layout, &batch).unwrap() {
        CommitOutcome::Delta { .. } => {}
        CommitOutcome::NeedFull { reason } => panic!("unexpected NeedFull: {reason}"),
    }
    let loaded = v6_load(&layout).unwrap();
    assert_eq!(loaded.entry_count(), store.entry_count());
    assert_eq!(loaded.entries, store.entries);
    assert_eq!(loaded.frns, store.frns);
    assert_eq!(loaded.names_buf, store.names_buf);
    assert_eq!(loaded.deleted.len(), store.deleted.len());
    assert_eq!(loaded.dirs.len(), store.dirs.len());
    // 删除的条目在重建后仍是墓碑
    let del_idx = loaded.frn_to_entry.get(&1).copied().unwrap() as usize;
    assert!(loaded.entries[del_idx].is_deleted());
    // 改名后 ext 桶一致
    for b in 0..256 {
        assert_eq!(
            loaded.ext_filter[b].as_ref().map(|x| x.len()),
            store.ext_filter[b].as_ref().map(|x| x.len()),
            "ext bucket {b} mismatch"
        );
    }
    let _ = std::fs::remove_dir_all(&layout.dir);
}

#[test]
fn v6_empty_commit_is_noop_and_crash_tail_tolerated() {
    let (_p, layout) = tmp_index("noop");
    let mut store = sample_store();
    v6_save_full(&layout, &store).unwrap();
    store.set_track_mutations(true);
    let batch = batch_of(&mut store);
    match v6_try_commit(&layout, &batch).unwrap() {
        CommitOutcome::Delta { applied, bytes, .. } => {
            assert_eq!(applied, 0);
            assert_eq!(bytes, 0);
        }
        CommitOutcome::NeedFull { reason } => panic!("unexpected NeedFull: {reason}"),
    }
    // 模拟崩溃 torn tail：entries.dat 尾部追加垃圾，未更新 manifest
    {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(layout.dir.join("entries.dat"))
            .unwrap();
        f.write_all(&[0xFFu8; 128]).unwrap();
    }
    // load 只读 manifest 前缀，容忍 tail
    let loaded = v6_load(&layout).unwrap();
    assert_eq!(loaded.entry_count(), store.entry_count());
    // 下一次 commit 会把 tail 截掉
    store
        .apply_change_event(&ChangeEvent::DataOrMeta {
            file_id: 2,
            size: Some(21),
            mtime: None,
            ctime: None,
        })
        .unwrap();
    let batch = batch_of(&mut store);
    match v6_try_commit(&layout, &batch).unwrap() {
        CommitOutcome::Delta { .. } => {}
        CommitOutcome::NeedFull { reason } => panic!("unexpected NeedFull: {reason}"),
    }
    let meta_len = std::fs::metadata(layout.dir.join("entries.dat")).unwrap().len();
    assert_eq!(meta_len, (store.entries.len() * 32) as u64);
    let _ = std::fs::remove_dir_all(&layout.dir);
}

#[test]
fn v6_corrupt_manifest_falls_back_to_v5_and_commit_heals() {
    use findx2_core::{v6_save_full, CommitBatch};
    let (p, layout) = tmp_index("heal");
    let store = sample_store();
    findx2_core::save_index_bin(&p, &store).unwrap();
    // 写一个 v6 骨架然后破坏 manifest
    v6_save_full(&layout, &store).unwrap();
    std::fs::write(layout.dir.join("manifest.json"), b"{broken json").unwrap();
    // load 回退 v5
    let loaded = load_store_auto(&p).unwrap();
    assert_eq!(loaded.entry_count(), store.entry_count());
    // commit 不 Err，而是 NeedFull → 全量自愈
    let mut store2 = loaded;
    store2.set_track_mutations(true);
    store2
        .apply_change_event(&ChangeEvent::DataOrMeta {
            file_id: 1,
            size: Some(77),
            mtime: None,
            ctime: None,
        })
        .unwrap();
    let batch = CommitBatch {
        mutations: store2.take_mutations(),
        entry_count: store2.entries.len(),
        dir_count: store2.dirs.len(),
        names_len: store2.names_buf.len(),
        dir_paths_len: store2.dir_paths_buf.len(),
        metadata_ready: store2.metadata_ready,
        volumes: store2.volumes.clone(),
        tri_pending: store2.tri_pending.clone(),
        tri_snap: 0,
        has_trigram: false,
    };
    match v6_try_commit(&layout, &batch).unwrap() {
        CommitOutcome::NeedFull { .. } => {}
        CommitOutcome::Delta { .. } => panic!("corrupt manifest 应该降级全量"),
    }
    v6_save_full(&layout, &store2).unwrap();
    let healed = v6_load(&layout).unwrap();
    assert_eq!(healed.entries, store2.entries);
    let _ = std::fs::remove_file(&p);
    let _ = std::fs::remove_dir_all(&layout.dir);
}

#[test]
fn v6_full_resync_falls_back_to_rewrite() {
    let (_p, layout) = tmp_index("resync");
    let mut store = sample_store();
    v6_save_full(&layout, &store).unwrap();
    store.set_track_mutations(true);
    store
        .apply_change_event(&ChangeEvent::DataOrMeta {
            file_id: 1,
            size: Some(99),
            mtime: None,
            ctime: None,
        })
        .unwrap();
    store.mark_full_resync();
    let batch = batch_of(&mut store);
    match v6_try_commit(&layout, &batch).unwrap() {
        CommitOutcome::NeedFull { .. } => {}
        CommitOutcome::Delta { .. } => panic!("expected NeedFull"),
    }
    // 降级：全量重写后数据一致
    v6_save_full(&layout, &store).unwrap();
    let loaded = v6_load(&layout).unwrap();
    assert_eq!(loaded.entries, store.entries);
    let _ = std::fs::remove_dir_all(&layout.dir);
}

#[test]
fn v6_auto_prefers_v6_over_v5() {    let (p, layout) = tmp_index("auto");
    let store = sample_store();
    // 先写 v5
    findx2_core::save_index_bin(&p, &store).unwrap();
    let v5 = load_store_auto(&p).unwrap();
    assert_eq!(v5.entry_count(), store.entry_count());
    // 再写 v6（改一个值以区分）
    let mut store2 = sample_store();
    store2.entries[0].size = 424242;
    v6_save_full(&layout, &store2).unwrap();
    let auto_loaded = load_store_auto(&p).unwrap();
    assert_eq!(auto_loaded.entries[0].size, 424242);
    let _ = std::fs::remove_file(&p);
    let _ = std::fs::remove_dir_all(&layout.dir);
}

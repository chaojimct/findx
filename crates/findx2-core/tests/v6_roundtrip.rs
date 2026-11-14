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
            create_if_missing: None,
        })
        .unwrap();
    let batch = batch_of(&mut store);
    assert!(!batch.mutations.is_empty());
    match v6_try_commit(&layout, &batch).unwrap() {
        CommitOutcome::Delta {
            applied,
            generation,
            ..
        } => {
            assert_eq!(applied, batch.mutations.len());
            assert!(generation >= 2);
        }
        CommitOutcome::NeedFull { reason } => panic!("unexpected NeedFull: {reason}"),
    }
    let loaded = v6_load(&layout).unwrap();
    assert_eq!(
        loaded.entries[store.frn_to_entry.get(&1).copied().unwrap() as usize].size,
        11
    );
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
fn v6_empty_commit_is_noop_and_uncommitted_junk_is_ignored() {
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
    // Uncommitted junk in a future generation directory must not be loaded.
    let junk = findx2_core::generation_dir(&layout, 999);
    std::fs::create_dir_all(&junk).unwrap();
    std::fs::write(junk.join("entries.dat"), [0xFFu8; 128]).unwrap();
    let loaded = v6_load(&layout).unwrap();
    assert_eq!(loaded.entry_count(), store.entry_count());
    store
        .apply_change_event(&ChangeEvent::DataOrMeta {
            file_id: 2,
            size: Some(21),
            mtime: None,
            ctime: None,
            create_if_missing: None,
        })
        .unwrap();
    let batch = batch_of(&mut store);
    match v6_try_commit(&layout, &batch).unwrap() {
        CommitOutcome::Delta { generation, .. } => {
            let live = findx2_core::generation_dir(&layout, generation).join("entries.dat");
            assert_eq!(std::fs::metadata(live).unwrap().len(), (store.entries.len() * 32) as u64);
        }
        CommitOutcome::NeedFull { reason } => panic!("unexpected NeedFull: {reason}"),
    }
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
    std::fs::write(findx2_core::published_path(&layout), b"{broken json").unwrap();
    // load 回退 v5；损坏的 published 指针不得与段文件混装
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
            create_if_missing: None,
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
            create_if_missing: None,
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
fn v6_auto_prefers_v6_over_v5() {
    let (p, layout) = tmp_index("auto");
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

#[test]
fn pending_namespace_manifest_crash_retains_replay_watermark() {
    use findx2_core::SearchEngine;
    let nonce=std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let dir=std::env::temp_dir().join(format!("findx-t109-crash-{}-{nonce}",std::process::id()));
    std::fs::create_dir(&dir).unwrap();
    let layout=V6Layout::for_index(&dir.join("synthetic.bin"));
    let raw=|id,parent,is_dir| RawEntry{file_id:id,file_id_128:None,parent_id:parent,name:"synthetic".into(),size:10,mtime:0,ctime:0,attrs:if is_dir{0x10}else{0},is_dir};
    let store=IndexBuilder::new(b'F',1,7,10).build_from_raw(vec![],vec![raw(1,0,true)],true).unwrap();
    let e=SearchEngine::new(store);e.enable_usage(1024*1024).unwrap();
    let pending=[ChangeEvent::Create{entry:raw(50,99,false)}];
    e.apply_volume_batch(b'F',&pending,7,20).unwrap();
    assert_eq!(e.index_store().volumes[0].last_usn,10);
    v6_save_full(&layout,&e.index_store()).unwrap();
    let restart=SearchEngine::new(v6_load(&layout).unwrap());
    restart.enable_usage(1024*1024).unwrap();
    restart.index_store_mut().set_track_mutations(true);
    restart.apply_volume_batch(b'F',&pending,7,20).unwrap();
    let parent=[ChangeEvent::Create{entry:raw(99,1,true)}];
    restart.apply_volume_batch(b'F',&parent,7,21).unwrap();
    assert_eq!(restart.index_store().volumes[0].last_usn,21);
    let batch=batch_of(&mut restart.index_store_mut());
    findx2_core::set_persist_fault(findx2_core::PersistFault::BeforePublish);
    assert!(v6_try_commit(&layout,&batch).is_err());
    findx2_core::clear_persist_fault();
    let loaded=v6_load(&layout).unwrap();
    assert_eq!(loaded.volumes[0].last_usn,10,"uncommitted generation must not advance restart cursor");
    let recovered=SearchEngine::new(loaded);recovered.enable_usage(1024*1024).unwrap();
    recovered.apply_volume_batch(b'F',&pending,7,20).unwrap();
    recovered.apply_volume_batch(b'F',&parent,7,21).unwrap();
    let page=recovered.usage_page("F:\\",None,None,4096).unwrap();
    assert_eq!(page.root.logical,10);
    assert_eq!(page.root.files,1);
    v6_save_full(&layout,&recovered.index_store()).unwrap();
    assert_eq!(v6_load(&layout).unwrap().volumes[0].last_usn,21);
    std::fs::remove_dir_all(dir).unwrap();
}

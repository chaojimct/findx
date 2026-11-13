//! Public synthetic namespace only; no live watcher, installed service or user paths.
use findx2_core::{IndexBuilder,RawEntry,SearchEngine};
fn engine()->SearchEngine {
    let raw=|id,dir|RawEntry{file_id:id,file_id_128:None,parent_id:if dir{0}else{1},name:if dir{"fixture-root".into()}else{"public-file.txt".into()},size:if dir{0}else{10},mtime:0,ctime:0,attrs:if dir{0x10}else{0},is_dir:dir};
    let mut store=IndexBuilder::new(b'F',1,7,100).build_from_raw(vec![raw(10,false)],vec![raw(1,true)],false).unwrap();
    store.set_track_mutations(true);
    let engine=SearchEngine::new(store);engine.enable_usage(1024*1024).unwrap();engine
}
fn check(engine:&SearchEngine,size:u64) {
    assert!(!engine.metadata_ready(),"coverage must remain false");
    let store=engine.index_store();
    let i=store.entry_on_volume(b'F',10).unwrap();
    assert_eq!(store.entries[i as usize].size,size,"accepted metadata must join the namespace checkpoint boundary");
    drop(store);
    let batch=engine.take_persistence_batch().expect("partial metadata must not remain outside the persistence journal");
    assert!(!batch.metadata_ready);assert_eq!(batch.volumes[0].last_usn,100);
    assert!(!batch.mutations.is_empty());
    assert!(engine.take_persistence_batch().is_none());
}
#[test]fn partial_ticket_is_checkpointed_without_ready_or_overlay_drain(){let e=engine();let t=e.metadata_ticket_by_id(b'F',10).unwrap();assert!(e.commit_metadata(t,456,0,0).unwrap());check(&e,456);}
#[test]fn partial_batch_is_checkpointed_without_ready_or_overlay_drain(){let e=engine();let i=e.index_store().entry_on_volume(b'F',10).unwrap() as usize;e.extend_metadata_overlay_batch(&[(i,567,0,0)]);check(&e,567);}
#[test]fn partial_observation_is_checkpointed_without_ready_or_overlay_drain(){let e=engine();let stamp=e.begin_metadata_observation(e.metadata_epoch()).unwrap();assert_eq!(e.commit_metadata_observation(b'F',stamp,&[(10,678,0,0)],4096).unwrap(),1);check(&e,678);}

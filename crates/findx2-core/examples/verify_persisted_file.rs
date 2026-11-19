//! Read-only offline verifier. Never starts an engine, service, watch or journal replay.
use std::path::Path;
fn main()->Result<(),Box<dyn std::error::Error>>{
 let args:Vec<_>=std::env::args().collect();
 if args.len()==3 && args[1]=="--inspect-index" {
  println!("{}",inspect_index(Path::new(&args[2]))?);
  return Ok(());
 }

 if args.len()!=5 && args.len()!=6{return Err("usage: verify_persisted_file INDEX EXPECTED_FULL_PATH EXPECTED_BYTES EXPECTED_GENERATION [MUST_BE_ABSENT_FULL_PATH]".into());}
 let layout=findx2_core::V6Layout::for_index(Path::new(&args[1]));
 let before=findx2_core::persist_tx::read_published(&layout)?.ok_or("no actual published generation")?;
 let generation:u64=args[4].parse()?;
 if before.manifest.generation!=generation{return Err("unexpected persisted generation before offline read".into());}
 let store=findx2_core::v6_load(&layout)?;
 let bytes:u64=args[3].parse()?;let mut matches=Vec::new();
 for (i,entry) in store.entries.iter().enumerate(){
  if entry.is_dir_entry()||entry.is_deleted(){continue;}
  if args.len()==6 && store.entry_display_path(i)?.eq_ignore_ascii_case(&args[5]){return Err("post-seal mutation contaminated sealed disk generation".into());}
  if store.entry_display_path(i)?.eq_ignore_ascii_case(&args[2]){matches.push(entry.size);}
 }
 if matches!=vec![bytes]{return Err(format!("offline disk snapshot did not contain exact expected path/size: {matches:?}").into());}
 let after=findx2_core::persist_tx::read_published(&layout)?.ok_or("published generation disappeared")?;
 if after.manifest.generation!=generation{return Err("persisted generation changed during offline read".into());}
 println!("{}",serde_json::json!({"passed":true,"offline":true,"watch":false,"replay":false,"path":args[2],"bytes":bytes,"generation":generation,"metadata_ready":store.metadata_ready,"absent_after_seal":args.get(5)}));
 Ok(())
}

fn inspect_index(path:&Path)->Result<serde_json::Value,Box<dyn std::error::Error>>{
 let layout=findx2_core::V6Layout::for_index(path);
 let segmented=layout.manifest_exists();
 let store=if segmented{findx2_core::v6_load(&layout)?}else{findx2_core::load_index_bin(path)?};
 let volumes:Vec<_>=store.volumes.iter().map(|v|serde_json::json!({"letter":(v.volume_letter as char).to_string(),"root_prefix":v.root_prefix,"serial":v.volume_serial,"journal_id":v.usn_journal_id,"last_usn":v.last_usn})).collect();
 Ok(serde_json::json!({"source":if segmented{"segmented-v6"}else{"legacy-anchor"},"offline":true,"watch":false,"replay":false,"entries":store.entry_count(),"metadata_ready":store.metadata_ready,"volumes":volumes}))
}
#[cfg(test)]mod tests{
 use super::*;
 #[test]fn initial_index_inspection_reads_actual_disk_metadata_and_volume_without_engine_or_replay(){
  use findx2_core::{IndexBuilder,RawEntry};
  let root=std::env::temp_dir().join(format!("findx-offline-native-provision-{}-{}",std::process::id(),std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));std::fs::create_dir(&root).unwrap();let path=root.join("index.bin");
  let raw=|id,dir|RawEntry{file_id:id,file_id_128:None,parent_id:if dir{0}else{1},name:if dir{"root".into()}else{"initial.txt".into()},size:18,mtime:0,ctime:0,attrs:if dir{0x10}else{0},is_dir:dir};
  let store=IndexBuilder::new(b'F',123,456,789).build_from_raw(vec![raw(2,false)],vec![raw(1,true)],false).unwrap();findx2_core::save_index_bin(&path,&store).unwrap();
  let v=inspect_index(&path).unwrap();assert_eq!(v["metadata_ready"],false);assert_eq!(v["volumes"][0]["letter"],"F");assert_eq!(v["volumes"][0]["serial"],123);assert_eq!(v["volumes"][0]["journal_id"],456);assert_eq!(v["volumes"][0]["last_usn"],789);assert_eq!(v["watch"],false);assert_eq!(v["replay"],false);assert!(v["entries"].as_u64().unwrap()>0);std::fs::remove_dir_all(root).unwrap();
 }
 #[test]fn inspection_prefers_actual_segmented_generation_over_stale_anchor_and_never_falls_back_on_corruption(){
  use findx2_core::{IndexBuilder,RawEntry};let root=std::env::temp_dir().join(format!("findx-inspect-latest-{}-{}",std::process::id(),std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));std::fs::create_dir(&root).unwrap();let path=root.join("index.bin");
  let d=RawEntry{file_id:1,file_id_128:None,parent_id:0,name:"".into(),size:0,mtime:0,ctime:0,attrs:16,is_dir:true};let mut s=IndexBuilder::new(b'F',123,456,789).build_from_raw(vec![],vec![d],true).unwrap();findx2_core::save_index_bin(&path,&s).unwrap();let anchor=std::fs::read(&path).unwrap();s.volumes[0].usn_journal_id=999;s.volumes[0].last_usn=1234;let layout=findx2_core::V6Layout::for_index(&path);findx2_core::v6_save_full(&layout,&s).unwrap();let v=inspect_index(&path).unwrap();assert_eq!(v["source"],"segmented-v6");assert_eq!(v["volumes"][0]["journal_id"],999);assert_eq!(v["volumes"][0]["last_usn"],1234);assert_eq!(std::fs::read(&path).unwrap(),anchor);
  std::fs::remove_dir_all(&layout.dir).unwrap();std::fs::create_dir(&layout.dir).unwrap();std::fs::write(layout.dir.join("manifest.json"),b"broken").unwrap();assert!(layout.manifest_exists());assert!(inspect_index(&path).is_err());std::fs::remove_dir_all(root).unwrap();
 }

}

//! Read-only offline verifier. Never starts an engine, service, watch or journal replay.
use std::path::Path;
fn main()->Result<(),Box<dyn std::error::Error>>{
 let args:Vec<_>=std::env::args().collect();
 if args.len()!=5{return Err("usage: verify_persisted_file INDEX EXPECTED_FULL_PATH EXPECTED_BYTES EXPECTED_GENERATION".into());}
 let layout=findx2_core::V6Layout::for_index(Path::new(&args[1]));
 let before=findx2_core::persist_tx::read_published(&layout)?.ok_or("no actual published generation")?;
 let generation:u64=args[4].parse()?;
 if before.manifest.generation!=generation{return Err("unexpected persisted generation before offline read".into());}
 let store=findx2_core::v6_load(&layout)?;
 let bytes:u64=args[3].parse()?;let mut matches=Vec::new();
 for (i,entry) in store.entries.iter().enumerate(){
  if entry.is_dir_entry()||entry.is_deleted(){continue;}
  if store.entry_display_path(i)?.eq_ignore_ascii_case(&args[2]){matches.push(entry.size);}
 }
 if matches!=vec![bytes]{return Err(format!("offline disk snapshot did not contain exact expected path/size: {matches:?}").into());}
 let after=findx2_core::persist_tx::read_published(&layout)?.ok_or("published generation disappeared")?;
 if after.manifest.generation!=generation{return Err("persisted generation changed during offline read".into());}
 println!("{}",serde_json::json!({"passed":true,"offline":true,"watch":false,"replay":false,"path":args[2],"bytes":bytes,"generation":generation,"metadata_ready":store.metadata_ready}));
 Ok(())
}

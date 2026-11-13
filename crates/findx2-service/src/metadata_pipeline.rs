//! Shared bounded fallback scheduler. It scans each index slot once and admits only
//! one byte-charged work window. No corpus-sized pending/results/identity maps.
use findx2_core::{SearchEngine,usage::MetadataWorkItem};
use std::sync::{Arc,atomic::{AtomicBool,Ordering}};
#[derive(Clone,Copy,Debug)]
pub(crate) struct Limits {pub active_bytes:usize,pub directory_bytes:usize,pub workers:usize}
impl Default for Limits{fn default()->Self{Self{active_bytes:2*1024*1024,directory_bytes:64*1024,workers:2}}}
#[derive(Default,Debug)]
pub(crate) struct Report{pub batches:usize,pub attempted:usize,pub committed:usize,pub missing:usize,pub peak_items:usize,pub peak_ticket_capacity:usize,pub peak_ticket_bytes:usize,pub max_scan_index:usize}
struct Lease{engine:Arc<SearchEngine>,items:Vec<MetadataWorkItem>}
impl Drop for Lease{fn drop(&mut self){for i in &self.items{self.engine.release_metadata_ticket(i.ticket);}}}
struct CapacityGuard(Arc<SearchEngine>);
impl Drop for CapacityGuard{fn drop(&mut self){self.0.reclaim_metadata_ticket_capacity();}}
pub(crate) fn ticket_pass<F>(engine:&Arc<SearchEngine>,epoch:u64,cancel:&AtomicBool,limits:Limits,mut fetch:F)->Result<Report,String>
where F:FnMut(u8,&[MetadataWorkItem])->Result<Vec<(usize,u64,u64,u64)>,String>{
    if limits.active_bytes<256||limits.workers==0{return Err("metadata work byte/concurrency budget cannot admit work".into());}
    let _capacity=CapacityGuard(engine.clone());let end=engine.index_store().entry_count();let mut next=0;let mut report=Report::default();
    while next<end {
        if cancel.load(Ordering::Acquire){return Err(format!("metadata pass cancelled after {} committed; coverage remains incomplete",report.committed));}
        let batch=engine.metadata_work_batch(epoch,next,end,limits.active_bytes)?;next=batch.next_index;report.max_scan_index=next;
        let lease=Lease{engine:engine.clone(),items:batch.items};
        if lease.items.is_empty(){std::thread::yield_now();continue;}
        report.batches+=1;report.attempted+=lease.items.len();report.peak_items=report.peak_items.max(lease.items.len());
        let (_,capacity,bytes)=engine.metadata_ticket_storage();report.peak_ticket_capacity=report.peak_ticket_capacity.max(capacity);report.peak_ticket_bytes=report.peak_ticket_bytes.max(bytes);
        // A volume-sized map is unnecessary: at most 256 letter slots, each with
        // a reusable bounded window. Windows currently has far fewer volumes.
        let mut seen=[false;256];for item in &lease.items{seen[item.volume as usize]=true;}
        let mut group=Vec::new();group.try_reserve_exact(lease.items.len()).map_err(|_|"metadata volume window allocation failed")?;
        let mut got=0;
        for (volume,present) in seen.into_iter().enumerate(){
            if !present{continue;}
            if cancel.load(Ordering::Acquire){return Err("metadata pass cancelled; tickets released".into());}
            group.clear();group.extend(lease.items.iter().filter(|i|i.volume as usize==volume).copied());
            let updates=fetch(volume as u8,&group)?;
            if updates.len()>group.len(){return Err("metadata producer exceeded admitted result window".into());}
            for (local,size,mtime,ctime) in updates{
                let item=group.get(local).ok_or("metadata producer returned invalid local identity")?;
                if engine.commit_metadata(item.ticket,size,mtime,ctime)?{report.committed+=1;engine.add_backfill_done(1);got+=1;}
            }
        }
        report.missing+=lease.items.len().saturating_sub(got);
        drop(lease);std::thread::yield_now();
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
 use super::*;
 use findx2_core::{IndexBuilder,RawEntry};
 fn engine(n:usize,budget:usize)->Arc<SearchEngine>{
  let raw=|id,dir|RawEntry{file_id:id,file_id_128:None,parent_id:if dir{0}else{1},name:"synthetic".into(),size:0,mtime:0,ctime:0,attrs:if dir{0x10}else{0},is_dir:dir};
  let mut g=IndexBuilder::new(b'F',1,7,10).build_from_raw(vec![raw(10,false)],vec![raw(1,true)],false).unwrap();
  let file=g.entries.iter().find(|e|!e.is_dir_entry()).copied().unwrap();
  g.entries.reserve(n);g.frns.reserve(n);
  for i in 0..n{g.entries.push(file);g.frns.push(100+i as u64);}
  g.set_track_mutations(true);
  let e=Arc::new(SearchEngine::new(g));e.enable_usage(budget).unwrap();e
 }
 fn memory()->(usize,usize){
  #[cfg(windows)]{
   #[repr(C)] struct M{cb:u32,faults:u32,peak_ws:usize,ws:usize,peak_paged:usize,paged:usize,peak_nonpaged:usize,nonpaged:usize,pf:usize,peak_pf:usize,private:usize}
   #[link(name="psapi")] extern "system" {fn GetProcessMemoryInfo(h:isize,m:*mut M,n:u32)->i32;}
   unsafe{let mut m:M=std::mem::zeroed();m.cb=std::mem::size_of::<M>() as u32;if GetProcessMemoryInfo(-1,&mut m,m.cb)!=0{return(m.ws,m.private);}}
  }(0,0)
 }
 fn memory_probe(n:usize){
  let e=engine(n,64*1024*1024);let epoch=e.metadata_epoch();let baseline=memory();let done=Arc::new(AtomicBool::new(false));let signal=done.clone();
  let sampler=std::thread::spawn(move||{let mut peak=baseline;while !signal.load(Ordering::Relaxed){let m=memory();peak.0=peak.0.max(m.0);peak.1=peak.1.max(m.1);std::thread::sleep(std::time::Duration::from_millis(1));}peak});
  let limits=Limits{active_bytes:1024*1024,..Limits::default()};
  let start=std::time::Instant::now();
  let report=ticket_pass(&e,epoch,&AtomicBool::new(false),limits,|_,items|Ok((0..items.len()).map(|i|(i,1,0,0)).collect())).unwrap();
  let after=memory();done.store(true,Ordering::Relaxed);let peak=sampler.join().unwrap();let elapsed=start.elapsed().as_millis();
  assert_eq!(report.committed,n+1);assert!(report.peak_items<=4096);assert!(report.peak_ticket_bytes<1024*1024);assert_eq!(e.metadata_ticket_storage().0,0);assert!(e.metadata_ticket_storage().2<=64*1024);
  assert_eq!(e.metadata_coverage(epoch).unwrap().unknown,0);
  assert!(!e.index_store().has_pending_mutations(),"backfill must not retain N per-file mutation records");
  let line=format!("bounded_fake_backfill files={} batches={} attempted={} committed={} peak_items={} peak_ticket_cap={} peak_ticket_bytes={} retained_ticket_bytes={} active_charge_budget={} d_ws={} d_private={} sampled_peak_d_ws={} sampled_peak_d_private={} elapsed_ms={elapsed}",n+1,report.batches,report.attempted,report.committed,report.peak_items,report.peak_ticket_capacity,report.peak_ticket_bytes,e.metadata_ticket_storage().2,limits.active_bytes,after.0 as i64-baseline.0 as i64,after.1 as i64-baseline.1 as i64,peak.0.saturating_sub(baseline.0),peak.1.saturating_sub(baseline.1));
  println!("{line}");
  let evidence=std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/backfill-t109-memory.txt");
  use std::io::Write;let mut file=std::fs::OpenOptions::new().create(true).append(true).open(evidence).unwrap();writeln!(file,"{line}").unwrap();
  assert!(e.complete_metadata_if_covered(epoch).unwrap());assert!(e.metadata_ready());
  assert!(e.index_store_mut().take_mutations().len()<=2,"bounded flush uses full-resync marker, not N patches");
 }
 #[test] fn bounded_virtual_memory_small(){memory_probe(65_536);}
 #[test] fn bounded_virtual_memory_large(){memory_probe(1_048_576);}
 #[test] fn errors_cancel_epoch_and_low_resources_release_tickets(){
  let e=engine(1000,4*1024*1024);let epoch=e.metadata_epoch();let limits=Limits{active_bytes:4096,..Limits::default()};
  assert!(!e.complete_metadata_if_covered(epoch).unwrap());
  let mut calls=0;
  let error=ticket_pass(&e,epoch,&AtomicBool::new(false),limits,|_,items|{calls+=1;if calls==2{return Err("fake IO failure".into());}Ok((0..items.len()).map(|i|(i,1,0,0)).collect())}).unwrap_err();
  assert!(error.contains("fake IO"));assert_eq!(e.metadata_ticket_storage().0,0);assert!(!e.metadata_ready());
  let cancel=AtomicBool::new(false);
  assert!(ticket_pass(&e,epoch,&cancel,limits,|_,_|{cancel.store(true,Ordering::Release);Ok(vec![])}).unwrap_err().contains("cancelled"));
  assert_eq!(e.metadata_ticket_storage().0,0);
  assert!(ticket_pass(&e,epoch,&AtomicBool::new(false),Limits{active_bytes:1,..limits},|_,_|Ok(vec![])).unwrap_err().contains("budget"));
  let clone=e.clone();assert!(ticket_pass(&e,epoch,&AtomicBool::new(false),limits,move|_,items|{clone.note_external_mutation();clone.enable_usage(4*1024*1024).unwrap();Ok((0..items.len()).map(|i|(i,1,0,0)).collect())}).unwrap_err().contains("epoch"));
  assert_eq!(e.metadata_ticket_storage().0,0);
 }
 #[test] fn stale_directory_observation_cannot_overwrite_namespace(){
  let e=engine(0,1024*1024);let epoch=e.metadata_epoch();let stamp=e.begin_metadata_observation(epoch).unwrap();
  e.apply_volume_change(b'F',&findx2_core::ChangeEvent::DataOrMeta{file_id:10,size:Some(99),mtime:None,ctime:None,create_if_missing:None}).unwrap();
  assert_eq!(e.commit_metadata_observation(b'F',stamp,&[(10,1,0,0)],4096).unwrap(),0);
  assert_eq!(e.usage_page("F:\\",None,None,4096).unwrap().root.logical,99);
 }
}

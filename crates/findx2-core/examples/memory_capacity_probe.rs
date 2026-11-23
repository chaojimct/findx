//! Read-only v6 capacity analysis. No watch, journal access, backfill or persistence.
use findx2_core::{v6_load,V6Layout,IndexStore,SearchEngine};
use serde_json::{json,Value};
use std::{path::PathBuf,mem::size_of};
fn vector<T>(v:&Vec<T>)->Value{json!({"len":v.len(),"capacity":v.capacity(),"element_bytes":size_of::<T>(),"len_bytes":v.len()*size_of::<T>(),"capacity_bytes":v.capacity()*size_of::<T>()})}
fn stats(s:&IndexStore)->Value{json!({"entries":vector(&s.entries),"frns":vector(&s.frns),"names":vector(&s.names_buf),"dirs":vector(&s.dirs),"ranges":vector(&s.dir_path_ranges),"dirpaths":vector(&s.dir_paths_buf),"entry_identity":s.frn_to_entry.memory_capacity_stats(),"directory_identity":s.dir_index.memory_capacity_stats()})}
#[cfg(windows)]fn process_memory()->Value{
 #[repr(C)]struct Counters{cb:u32,faults:u32,peak_ws:usize,ws:usize,peak_paged:usize,paged:usize,peak_nonpaged:usize,nonpaged:usize,pagefile:usize,peak_pagefile:usize,private:usize}
 #[link(name="kernel32")]extern "system"{fn GetCurrentProcess()->*mut std::ffi::c_void;}
 #[link(name="psapi")]extern "system"{fn GetProcessMemoryInfo(h:*mut std::ffi::c_void,p:*mut Counters,n:u32)->i32;}
 let mut c: Counters=unsafe{std::mem::zeroed()};c.cb=size_of::<Counters>() as u32;
 let ok=unsafe{GetProcessMemoryInfo(GetCurrentProcess(),&mut c,size_of::<Counters>() as u32)};
 json!({"ok":ok!=0,"private_bytes":c.private,"working_set_bytes":c.ws,"peak_working_set_bytes":c.peak_ws})
}
#[cfg(not(windows))]fn process_memory()->Value{json!({"not_measured":true})}
fn emit(stage:&str,s:&IndexStore,usage:Option<Value>){println!("{}",json!({"stage":stage,"store":stats(s),"usage":usage,"process":process_memory()}));}
fn main(){
 let mut args=std::env::args().skip(1);let path=PathBuf::from(args.next().expect("explicit authorized test index required"));
 let additional:usize=args.next().unwrap_or_else(||"0".into()).parse().expect("synthetic additional entries");
 assert!(additional<=100000,"bounded capacity simulation only");
 let s=v6_load(&V6Layout::for_index(&path)).expect("read-only v6 load; no fallback save");emit("loaded_no_usage_no_overlay",&s,None);
 let engine=SearchEngine::new(s);
 {let s=engine.index_store();emit("engine_overlay_allocated_no_usage",&s,None);}
 engine.enable_usage(256*1024*1024).expect("same usage budget as diagnostic service");
 let usage=engine.usage_memory_capacity_stats();{let s=engine.index_store();emit("usage_enabled_no_watch_backfill",&s,Some(usage));}
 if additional>0{
  let mut s=engine.index_store_mut();
  // Same-count append/reserve experiment ONLY. Not journal replay, and no alias/history claim.
  let exemplar=s.entries[0];let n=s.entries.len();let d=s.dirs[0].clone();let r=s.dir_path_ranges[0];let name_extra=s.names_buf.len()*additional/n.max(1);
  for _ in 0..additional{s.entries.push(exemplar);s.frns.push(0);}
  let extra_dirs=s.dirs.len()*additional/n.max(1);for _ in 0..extra_dirs{s.dirs.push(d.clone());s.dir_path_ranges.push(r);}
  let target_names=s.names_buf.len()+name_extra;s.names_buf.resize(target_names,0);
  emit("synthetic_count_only_append_not_replay",&s,None);
 }

}

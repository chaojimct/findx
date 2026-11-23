//! Independent OS capacity, never derived from indexed directory logical totals.
//! One outstanding blocking OS query at most; slow/offline shares never block UI
//! and never create an unbounded set of timeout workers. No worker is killed.
use crate::disk_usage::VolumeSpace;
use std::{collections::VecDeque,path::{Path,PathBuf},time::{Duration,Instant}};
#[derive(Clone,Debug)] pub struct Query{pub path:PathBuf,serial:u64}
struct Entry{path:PathBuf,value:Result<VolumeSpace,String>,checked:Instant}
#[derive(Default)] pub struct State{entries:VecDeque<Entry>,active:Option<(Query,Instant)>,serial:u64}
impl State{
 pub fn begin(&mut self,path:&Path)->Option<Query>{
  if self.active.is_some()||self.entries.iter().any(|e|e.path==path&&e.checked.elapsed()<Duration::from_secs(30)){return None}
  self.serial=self.serial.wrapping_add(1);let q=Query{path:path.into(),serial:self.serial};self.active=Some((q.clone(),Instant::now()));Some(q)
 }
 pub fn finish(&mut self,query:Query,value:Result<VolumeSpace,String>){
  if !self.active.as_ref().is_some_and(|(q,_)|q.serial==query.serial&&q.path==query.path){return}
  self.active=None;self.entries.retain(|e|e.path!=query.path);while self.entries.len()>=8{self.entries.pop_front();}
  self.entries.push_back(Entry{path:query.path,value,checked:Instant::now()});
 }
 pub fn get(&self,path:&Path)->Option<&Result<VolumeSpace,String>>{self.entries.iter().find(|e|e.path==path).map(|e|&e.value)}
 pub fn age(&self,path:&Path)->Option<Duration>{self.entries.iter().find(|e|e.path==path).map(|e|e.checked.elapsed())}
 pub fn slow(&self,path:&Path)->bool{self.active.as_ref().is_some_and(|(q,t)|q.path==path&&t.elapsed()>Duration::from_secs(2))}
 pub fn refresh(&mut self){self.entries.clear();}
}
pub fn query(path:&Path)->Result<VolumeSpace,String>{
 if !path.is_absolute()||path.as_os_str().as_encoded_bytes().contains(&0){return Err("卷容量需要有效绝对路径，不能按当前工作目录猜测卷".into())}
 let v=platform(path)?;
 if v.total==0||v.free>v.total||v.available>v.total{return Err("系统未提供有效卷容量".into())}Ok(v)
}
#[cfg(windows)] fn platform(path:&Path)->Result<VolumeSpace,String>{crate::disk_usage::volume_space(path)}
#[cfg(unix)] fn platform(path:&Path)->Result<VolumeSpace,String>{
 use std::{ffi::CString,os::unix::{ffi::OsStrExt,fs::MetadataExt}};
 let canonical=path.canonicalize().map_err(|e|format!("文件系统不可访问：{e}"))?;
 let mut root=canonical.clone();let dev=root.metadata().map_err(|e|e.to_string())?.dev();
 for _ in 0..256 {let Some(parent)=root.parent()else{break};match parent.metadata(){Ok(m)if m.dev()==dev=>root=parent.into(),_=>break}}
 let input=CString::new(canonical.as_os_str().as_bytes()).map_err(|_|"路径含无效字符")?;
 let mut info=std::mem::MaybeUninit::<libc::statvfs>::uninit();
 if unsafe{libc::statvfs(input.as_ptr(),info.as_mut_ptr())}!=0{return Err(std::io::Error::last_os_error().to_string())}
 let info=unsafe{info.assume_init()};let block=if info.f_frsize>0{info.f_frsize}else{info.f_bsize}as u128;
 let bytes=|blocks:u128|->Result<u64,String>{u64::try_from(blocks.checked_mul(block).ok_or("卷容量溢出")?).map_err(|_|"卷容量溢出".into())};
 Ok(VolumeSpace{root,total:bytes(info.f_blocks as u128)?,free:bytes(info.f_bfree as u128)?,available:bytes(info.f_bavail as u128)?})
}
#[cfg(not(any(unix,windows)))]fn platform(_:&Path)->Result<VolumeSpace,String>{Err("此系统不支持卷容量读取".into())}
#[cfg(test)]mod tests{
 use super::*;
 #[test]fn only_one_slow_query_and_stale_completion_cannot_overwrite_another_path(){let mut s=State::default();let a=s.begin(Path::new("A")).unwrap();assert!(s.begin(Path::new("B")).is_none());s.finish(a,Err("offline".into()));let b=s.begin(Path::new("B")).unwrap();s.finish(b,Err("denied".into()));assert_eq!(s.get(Path::new("A")).unwrap().as_ref().unwrap_err(),"offline");assert_eq!(s.get(Path::new("B")).unwrap().as_ref().unwrap_err(),"denied");assert!(s.begin(Path::new("B")).is_none());}
 #[test]fn cache_is_bounded_and_unknown_is_not_zero(){let mut s=State::default();for i in 0..20{let p=PathBuf::from(i.to_string());let q=s.begin(&p).unwrap();s.finish(q,Err("unavailable".into()));}assert_eq!(s.entries.len(),8);assert!(s.get(Path::new("0")).is_none());assert!(s.get(Path::new("19")).unwrap().is_err());}
 #[test]fn actual_system_capacity_respects_physical_not_quota_used_invariant(){let p=std::env::temp_dir();let v=query(&p).unwrap();assert!(v.total>0);assert!(v.free<=v.total);assert_eq!(v.used(),v.total-v.free);}
 #[test]fn relative_or_unresolved_volume_never_falls_back_to_the_process_working_drive(){assert!(query(Path::new("__findx_no_such_volume__/missing")).is_err());}
}

#[cfg(test)]mod stale_completion_tests{
 use super::*;
 #[test]fn old_duplicate_completion_cannot_clear_new_query_or_publish_into_its_scope(){
  let mut s=State::default();let a=s.begin(Path::new("A")).unwrap();s.finish(a.clone(),Err("old".into()));let b=s.begin(Path::new("B")).unwrap();s.finish(a,Err("stale".into()));assert!(s.get(Path::new("B")).is_none());assert!(s.begin(Path::new("C")).is_none());s.finish(b,Err("new".into()));assert_eq!(s.get(Path::new("A")).unwrap().as_ref().unwrap_err(),"old");assert_eq!(s.get(Path::new("B")).unwrap().as_ref().unwrap_err(),"new");
 }
}

#[cfg(all(test,windows))]mod windows_path_tests{
 #[test]fn absent_directory_on_known_volume_still_queries_that_real_volume(){let root=std::env::temp_dir();let base=super::query(&root).unwrap();let missing=super::query(&root.join("findx-owned-absent-capacity-probe").join("missing")).unwrap();assert_eq!(base.root,missing.root);assert_eq!(base.total,missing.total);}
}

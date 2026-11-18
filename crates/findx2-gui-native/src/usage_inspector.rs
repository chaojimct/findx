//! Coherent bounded Inspector snapshots; selection never changes navigation state.
use crate::disk_usage::{Node,Scan};
use std::{collections::VecDeque,path::PathBuf,sync::{Arc,atomic::{AtomicBool,Ordering}}};
pub type Source=(usize,usize);
pub type ActionKey=(usize,usize,u64);
#[derive(Clone)]pub struct Snapshot{pub scan:Arc<Scan>,pub root:usize,pub selected:usize,pub source:Source,pub serial:u64,pub issue:Option<String>,pub partial:bool}
impl Snapshot{
 pub fn node(&self)->&Node{&self.scan.nodes[self.selected]}
 pub fn key(&self)->ActionKey{(self.source.0,self.source.1,self.serial)}
}
#[derive(Clone)]pub struct Request{pub key:ActionKey,pub path:PathBuf,pub cancel:Arc<AtomicBool>}
#[derive(Default)]pub struct State{pub displayed:Option<Arc<Snapshot>>,pub pending:Option<Request>,serial:u64,cache:VecDeque<Arc<Snapshot>>}
impl State{
 pub fn cancel(&mut self){if let Some(p)=self.pending.take(){p.cancel.store(true,Ordering::Release);}self.serial=self.serial.wrapping_add(1);}
 pub fn begin(&mut self,mut seed:Snapshot)->Option<Request>{
  if self.pending.as_ref().is_some_and(|p|p.path==seed.node().path&&(p.key.0,p.key.1)==seed.source){return None}
  if self.displayed.as_ref().is_some_and(|p|p.source==seed.source&&p.node().path==seed.node().path&&p.issue.is_none()){
   if self.pending.is_some(){self.cancel();let mut same=(**self.displayed.as_ref().unwrap()).clone();same.serial=self.serial;self.displayed=Some(Arc::new(same));}return None
  }
  self.cancel();seed.serial=self.serial;
  if self.displayed.as_ref().is_some_and(|s|s.source!=seed.source){self.displayed=None;self.cache.clear();}
  if let Some(i)=self.cache.iter().position(|s|s.source==seed.source&&s.node().path==seed.node().path){let old=self.cache.remove(i).unwrap();let mut fresh=(*old).clone();fresh.serial=self.serial;let fresh=Arc::new(fresh);self.displayed=Some(fresh.clone());self.cache.push_back(fresh);return None}
  let req=Request{key:seed.key(),path:seed.node().path.clone(),cancel:Arc::new(AtomicBool::new(false))};
  if self.displayed.is_none(){self.displayed=Some(Arc::new(seed));} // initial source: one coherent loading snapshot
  self.pending=Some(req.clone());Some(req)
 }
 pub fn current(&self,r:&Request)->bool{!r.cancel.load(Ordering::Acquire)&&self.pending.as_ref().is_some_and(|p|p.key==r.key&&p.path==r.path)}
 pub fn finish(&mut self,r:&Request,mut snapshot:Snapshot)->bool{
  if !self.current(r){return false}snapshot.serial=r.key.2;self.pending=None;
  let s=Arc::new(snapshot);self.displayed=Some(s.clone());
  if s.issue.is_none(){self.cache.retain(|old|old.node().path!=s.node().path);self.cache.push_back(s);while self.cache.len()>8||self.cache.iter().map(|s|s.scan.nodes.len()).sum::<usize>()>4096{self.cache.pop_front();}}
  true
 }
 pub fn accepts(&self,key:ActionKey,source:Source)->bool{self.displayed.as_ref().is_some_and(|s|s.key()==key&&s.source==source)}
}
/// Only three header nodes plus bounded direct children are copied, never the
/// complete geometry tree. nodes[0] retains the original QUALITY scope.
pub fn seed(scan:&Scan,index:usize,root:usize,source:Source,indexed:bool)->Snapshot{
 let mut nodes=vec![scan.nodes[0].clone(),scan.nodes[root].clone(),scan.nodes[index].clone()];for n in &mut nodes{n.children.clear();n.parent=None;}
 for &i in &scan.nodes[index].children{if nodes.len()>=2051{break}let mut n=scan.nodes[i].clone();n.children.clear();n.parent=Some(2);let at=nodes.len();nodes.push(n);nodes[2].children.push(at);}
 let s=Scan{logical_generation:scan.logical_generation,nodes,errors:scan.errors,skipped_links:scan.skipped_links,hardlinks:scan.hardlinks,unknown:scan.unknown,partial:scan.partial,cancelled:scan.cancelled,elapsed:scan.elapsed,entry_limit_reached:scan.entry_limit_reached,time_limit_reached:scan.time_limit_reached,memory_limit_reached:scan.memory_limit_reached,alloc_failed:scan.alloc_failed,limits:scan.limits.clone(),volume:scan.volume.clone()};
 Snapshot{scan:Arc::new(s),root:if root==index{2}else{1},selected:2,source,serial:0,issue:if indexed&&scan.nodes[index].directory{Some("正在读取子项".into())}else{None},partial:false}
}
pub fn loaded(mut seed:Snapshot,list:crate::indexed_siblings::Listing)->Snapshot{
 let scan=Arc::make_mut(&mut seed.scan);scan.nodes.truncate(3);let n=&mut scan.nodes[2];n.children.clear();n.logical=list.root.logical;n.files=list.root.files;n.directories=list.root.directories;n.entries=list.root.files.saturating_add(list.root.directories);n.uncertain=list.root.unknown>0;
 let parent=n.path.clone();for item in list.children.iter(){let mut n=scan.nodes[2].clone();n.path=parent.join(&item.name);n.parent=Some(2);n.children.clear();n.logical=item.logical;n.files=item.files;n.directories=item.directories;n.entries=item.files.saturating_add(item.directories);n.directory=item.directory;n.mtime=0;n.hardlink_duplicate=false;n.uncertain=item.unknown>0;let at=scan.nodes.len();scan.nodes.push(n);scan.nodes[2].children.push(at);}
 seed.issue=None;seed.partial=list.items.partial;seed
}
/// Native mouse-down is routed once; matching up/out or activation loss releases it.
pub fn side_down(held:&mut [bool;2],forward:bool)->bool{let i=usize::from(forward);if held[i]{false}else{held[i]=true;true}}
#[cfg(test)]mod tests{
 use super::*;use findx2_ipc::{UsageItemDto as I,UsagePageDto as P};
 fn sample(name:&str)->Snapshot{let i=|id,name:&str|I{id,name:name.into(),directory:true,logical:100,files:8,directories:2,unknown:4};let p=P{epoch:7,generation:11,root:i(1,"root"),children:vec![i(2,name)],next:None,resident_bytes:0,state:"fixture".into(),watch_error:None,metadata_error:None};let s=crate::indexed_usage::scan_from_page(&p,"C".into());seed(&s,1,0,(9,0),true)}
 #[test]fn superseded_selection_never_mixes_header_and_children(){let mut s=State::default();let a=s.begin(sample("A")).unwrap();let b=s.begin(sample("B")).unwrap();assert_eq!(s.displayed.as_ref().unwrap().node().path,PathBuf::from("C").join("A"));assert!(!s.finish(&a,sample("A")));let mut done=sample("B");done.issue=None;assert!(s.finish(&b,done));assert_eq!(s.displayed.unwrap().node().path,PathBuf::from("C").join("B"));}
 #[test]fn cache_reuse_has_fresh_action_key_and_scope_quality_is_not_selected(){let mut s=State::default();let a=s.begin(sample("A")).unwrap();let mut done=sample("A");done.issue=None;s.finish(&a,done);let old=s.displayed.as_ref().unwrap().key();s.begin(sample("B"));assert!(s.begin(sample("A")).is_none());assert_ne!(old,s.displayed.as_ref().unwrap().key());assert_eq!(s.displayed.as_ref().unwrap().scan.nodes[0].path,PathBuf::from("C"));assert_eq!(s.displayed.as_ref().unwrap().node().mtime,0);}

 #[test]fn scope_replacement_and_error_never_show_old_children_as_new_or_zero(){let mut s=State::default();let a=s.begin(sample("A")).unwrap();let mut b=sample("B");b.source=(10,0);let req=s.begin(b.clone()).unwrap();assert!(!s.finish(&a,sample("A")));b.issue=Some("子项读取失败".into());b.partial=true;assert!(s.finish(&req,b));let shown=s.displayed.unwrap();assert_eq!(shown.source,(10,0));assert!(shown.partial);assert!(shown.issue.is_some());assert_eq!(shown.node().files,8);assert_ne!(shown.node().files,0);}
 #[test]fn native_side_down_deduplicates_then_release_or_blur_allows_next_press(){let mut held=[false;2];assert!(side_down(&mut held,false));assert!(!side_down(&mut held,false));assert!(side_down(&mut held,true));assert!(!side_down(&mut held,true));held[0]=false;assert!(side_down(&mut held,false));held=[false;2];assert!(side_down(&mut held,true));let root=include_str!("ui/main_window.rs");assert!(root.contains("workspace_native_side(event.button,cx)"));let adapter=include_str!("ui/workspace_inspector.rs");assert!(adapter.contains("native_side_direction(button)"));assert!(adapter.contains("MouseButton::Navigate(NavigationDirection::Forward)=>Some(true)"));assert_eq!(root.matches("on_mouse_up_out(MouseButton::Navigate").count(),2);}
}


#[cfg(test)]mod click_event_contract{#[test]fn pointer_hover_keeps_tooltips_but_cannot_request_inspector(){let map=include_str!("ui/workspace_usage.rs");let controller=include_str!("ui/workspace_inspector.rs");let hover=map.split(".on_mouse_move(").nth(1).unwrap().split(".on_hover(").next().unwrap();assert!(!hover.contains("inspector_"));assert!(hover.contains("usage_hover = hit"));assert!(map.contains(".when_some(hover_detail"));assert!(controller.contains("!this.workspace.inspector.current(&request)"));assert!(!controller.contains("inspector_freeze"));}}

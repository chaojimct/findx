//! Bounded indexed-usage page to GUI render model. No filesystem scan or allocated-size inference.
use crate::disk_usage::{self, Scan, Limits};
use std::{path::PathBuf, time::Duration};
pub fn scan_from_page(page: &findx2_ipc::UsagePageDto, requested: PathBuf) -> Scan {
    let degraded=page.watch_error.is_some() || page.metadata_error.is_some();
    let convert =
        |n: &findx2_ipc::UsageItemDto, path: PathBuf, parent: Option<usize>| {
            disk_usage::Node {
                path,
                parent,
                children: vec![],
                entries: n.files + n.directories,
                files: n.files,
                directories: n.directories,
                category: 0,
                own_allocated: 0,
                directory: n.directory,
                logical: n.logical,
                mtime: 0,
                allocated: 0,
                hardlink_duplicate: false,
                uncertain: n.unknown > 0,
            }
        };
    let mut nodes = vec![convert(&page.root, requested.clone(), None)];
    for child in &page.children {
        nodes.push(convert(child, requested.join(&child.name), Some(0)));
    }
    nodes[0].children = (1..nodes.len()).collect();
    let scan = Scan {
        logical_generation: Some((page.epoch, page.generation)),
        nodes,
        errors: 0,
        skipped_links: 0,
        hardlinks: 0,
        unknown: page.root.unknown,
        partial: page.root.unknown > 0 || page.next.is_some() || degraded,
        cancelled: false,
        elapsed: Duration::ZERO,
        entry_limit_reached: false,
        time_limit_reached: false,
        memory_limit_reached: false,
        alloc_failed: false,
        limits: Limits::default(),
        volume: None,
    };
    scan
}

pub fn ensure_not_cancelled(cancel:&std::sync::atomic::AtomicBool)->Result<(),String>{
 if cancel.load(std::sync::atomic::Ordering::Acquire){Err("usage request cancelled".into())}else{Ok(())}
}
/// One asynchronous GUI batch may skip at most 16 empty continuation pages.
/// It retains the final continuation on yield; no corpus coverage is truncated.
pub const MAX_EMPTY_PAGE_HOPS:usize=16;
pub struct EmptyPageGuard {seen:[u32;17],len:usize,hops:usize,expected:Option<(u64,u64)>}
impl EmptyPageGuard {
 pub fn new(expected:Option<(u64,u64)>,after:Option<u32>)->Self {
  let mut s=Self{seen:[0;17],len:0,hops:0,expected};
  if let Some(a)=after{s.seen[0]=a;s.len=1;}s
 }
 pub fn follow(&mut self,page:&findx2_ipc::UsagePageDto)->Result<Option<u32>,String>{
  let stamp=(page.epoch,page.generation);
  if self.expected.is_some_and(|e|e!=stamp){return Err("usage generation changed while skipping empty pages; refresh".into())}
  self.expected=Some(stamp);
  let Some(next)=page.next else{return Ok(None)};
  if self.seen[..self.len].contains(&next){return Err("usage continuation cycle detected; refresh".into())}
  if !page.children.is_empty(){return Ok(None)}
  self.hops+=1;
  if self.hops>=MAX_EMPTY_PAGE_HOPS{return Ok(None)}
  self.seen[self.len]=next;self.len+=1;Ok(Some(next))
 }
 pub fn skipped(&self)->usize{self.hops}
}


/// Child identities and order only; generation, unknown counts and logical totals are ignored.
pub fn same_structure(scan:&Scan, requested:&std::path::Path, page:&findx2_ipc::UsagePageDto)->bool{
    let Some(root)=scan.nodes.first() else {return false};
    if root.path!=requested || !root.directory || !page.root.directory {return false;}
    if let Some((epoch,_))=scan.logical_generation { if epoch!=page.epoch {return false;} }
    if root.children.len()!=page.children.len() {return false;}
    root.children.iter().zip(page.children.iter()).all(|(&ix,child)|{
        let n=match scan.nodes.get(ix){Some(n)=>n,None=>return false};
        n.directory==child.directory && n.path.file_name().and_then(|x|x.to_str())==Some(child.name.as_str())
    })
}

/// Update logical totals/unknown flags without replacing the node graph or scan allocation identity.
pub fn patch_metrics(scan:&mut Scan, page:&findx2_ipc::UsagePageDto){
    let degraded=page.watch_error.is_some()||page.metadata_error.is_some();
    if let Some(root)=scan.nodes.first_mut(){
        root.logical=page.root.logical;
        root.files=page.root.files;
        root.directories=page.root.directories;
        root.entries=page.root.files.saturating_add(page.root.directories);
        root.uncertain=page.root.unknown>0;
    }
    if let Some(children)=scan.nodes.first().map(|n|n.children.clone()){
        for (&ix,child) in children.iter().zip(page.children.iter()){
            if let Some(n)=scan.nodes.get_mut(ix){
                n.logical=child.logical;
                n.files=child.files;
                n.directories=child.directories;
                n.entries=child.files.saturating_add(child.directories);
                n.uncertain=child.unknown>0;
            }
        }
    }
    scan.logical_generation=Some((page.epoch,page.generation));
    scan.unknown=page.root.unknown;
    scan.partial=page.root.unknown>0||page.next.is_some()||degraded;
}

#[cfg(test)]
mod continuation_tests {
 use super::*;
 fn page(next:Option<u32>,children:bool)->findx2_ipc::UsagePageDto {
  let item=findx2_ipc::UsageItemDto{id:0,name:"public".into(),directory:true,logical:1,files:1,directories:1,unknown:0};
  findx2_ipc::UsagePageDto{epoch:1,generation:2,root:item.clone(),children:if children{vec![item]}else{vec![]},next,resident_bytes:0,state:"test".into(),watch_error:None,metadata_error:None}
 }
 #[test]fn cancelled_batch_cannot_issue_another_page(){
  let cancel=std::sync::atomic::AtomicBool::new(false);ensure_not_cancelled(&cancel).unwrap();
  let mut g=EmptyPageGuard::new(None,None);assert_eq!(g.follow(&page(Some(1),false)).unwrap(),Some(1));
  cancel.store(true,std::sync::atomic::Ordering::Release);assert!(ensure_not_cancelled(&cancel).is_err());
 }
 #[test]fn empty_prefix_is_followed_until_visible_or_complete(){
  let mut g=EmptyPageGuard::new(None,None);
  for token in 1..5{assert_eq!(g.follow(&page(Some(token),false)).unwrap(),Some(token));}
  assert_eq!(g.follow(&page(Some(5),true)).unwrap(),None);assert_eq!(g.skipped(),4);
  assert_eq!(EmptyPageGuard::new(None,None).follow(&page(None,false)).unwrap(),None);
 }
 #[test]fn cycles_and_generation_changes_are_visible_errors(){
  let mut g=EmptyPageGuard::new(Some((1,2)),Some(3));assert!(g.follow(&page(Some(3),false)).unwrap_err().contains("cycle"));
  let mut g=EmptyPageGuard::new(None,None);g.follow(&page(Some(1),false)).unwrap();g.follow(&page(Some(2),false)).unwrap();assert!(g.follow(&page(Some(1),false)).is_err());
  let mut p=page(Some(4),false);p.generation=3;assert!(EmptyPageGuard::new(Some((1,2)),None).follow(&p).is_err());
 }
 #[test]fn bounded_batch_yields_without_discarding_continuation(){
  let mut g=EmptyPageGuard::new(None,None);let mut last=None;
  for token in 1..=MAX_EMPTY_PAGE_HOPS as u32 {let p=page(Some(token),false);last=g.follow(&p).unwrap();if last.is_none(){assert!(p.next.is_some());}}
  assert_eq!(last,None);assert_eq!(g.skipped(),MAX_EMPTY_PAGE_HOPS);
  let mut resumed=EmptyPageGuard::new(Some((1,2)),Some(16));assert_eq!(resumed.follow(&page(Some(17),false)).unwrap(),Some(17));
 }
}

#[cfg(test)]
mod structure_tests {
    use super::*;
    use std::path::PathBuf;
    fn item(name:&str,logical:u64,unknown:u64)->findx2_ipc::UsageItemDto{
        findx2_ipc::UsageItemDto{id:1,name:name.into(),directory:true,logical,files:3,directories:2,unknown}
    }
    fn page(generation:u64,unknown:u64,logical:u64,child_name:&str,child_logical:u64)->findx2_ipc::UsagePageDto{
        findx2_ipc::UsagePageDto{epoch:9,generation,root:item("dev",logical,unknown),children:vec![item(child_name,child_logical,0)],next:None,resident_bytes:0,state:"ok".into(),watch_error:None,metadata_error:None}
    }
    #[test]fn generation_and_unknown_ticks_keep_structure(){
        let requested=PathBuf::from(r"C:\Users\chaoj\dev");
        let first=page(7,1,100,"tools",80);
        let mut scan=scan_from_page(&first,requested.clone());
        let later=page(12,4,100,"tools",80);
        assert!(same_structure(&scan,&requested,&later));
        let ptr=scan.nodes.as_ptr();
        patch_metrics(&mut scan,&later);
        assert_eq!(scan.nodes.as_ptr(),ptr);
        assert_eq!(scan.logical_generation,Some((9,12)));
        assert_eq!(scan.unknown,4);
        assert_eq!(scan.nodes[0].uncertain,true);
        assert_eq!(scan.nodes[0].logical,100);
        assert_eq!(scan.nodes[1].logical,80);
        let renamed=page(13,4,100,"other",80);
        assert!(!same_structure(&scan,&requested,&renamed));
        let mut other_epoch=later; other_epoch.epoch=10;
        assert!(!same_structure(&scan,&requested,&other_epoch));
    }
}

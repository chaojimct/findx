//! One-parent, bounded, read-only Usage menu adapter. No disk traversal.
use crate::{indexed_depth::PageCache,indexed_usage,navigation::{Sibling,Siblings}};
use findx2_ipc::{UsageItemDto as Item,UsagePageDto as Page};
use std::{collections::HashSet,future::Future,path::PathBuf,sync::{Arc,Mutex,atomic::AtomicBool}};
pub const MAX_PAGES:usize=16;
pub const MAX_ITEMS:usize=2048;
const MAX_BYTES:usize=1024*1024;
pub struct Listing{pub items:Siblings,pub total:u64,pub stamp:(u64,u64),pub root:Item,pub children:Arc<Vec<Item>>}
fn basename(s:&str)->bool{!s.is_empty()&&s!="."&&s!=".."&&!s.contains(['/', '\\', ':','\0'])}
fn equivalent(a:&Item,b:&Item)->bool{a.id==b.id&&a.directory==b.directory}
fn listing(path:&std::path::Path,root:&Item,children:&[Item],partial:bool,stamp:(u64,u64))->Listing{
 let mut entries=children.iter().map(|n|Sibling{path:path.join(&n.name),name:n.name.clone(),directory:n.directory,bytes:Some(n.logical),uncertain:n.unknown>0}).collect::<Vec<_>>();
 entries.sort_by(|a,b|b.bytes.cmp(&a.bytes).then_with(||a.name.cmp(&b.name)));
 Listing{items:Siblings{entries,partial},total:root.logical,stamp,root:root.clone(),children:Arc::new(children.to_vec())}
}
pub async fn load<F,Fut>(path:PathBuf,expected:Option<(u64,u64)>,cache:Arc<Mutex<PageCache>>,cancel:Arc<AtomicBool>,mut fetch:F)->Result<Listing,String>
where F:FnMut(Option<u32>,Option<(u64,u64)>)->Fut,Fut:Future<Output=Result<Page,String>>{
 let mut after=None;let mut stamp=expected;let mut root:Option<Item>=None;let mut children=Vec::new();let mut names=HashSet::new();let mut cursors=HashSet::new();let mut bytes=0;let mut partial=false;
 let started=std::time::Instant::now();
 for page_index in 0..MAX_PAGES{
  indexed_usage::ensure_not_cancelled(&cancel)?;
  let p=fetch(after,stamp).await?;
  indexed_usage::ensure_not_cancelled(&cancel)?;
  let current=(p.epoch,p.generation);
  if stamp.is_some_and(|s|s.0!=current.0){return Err("索引已重载，请重新打开".into())}stamp=Some((current.0,0));
  if let Some(r)=&root{if !equivalent(r,&p.root){return Err("父目录汇总已变化，请重新打开".into())}}
  else{
   if !p.root.directory{return Err("当前层不是目录".into())}
   if let Some(cached)=cache.lock().map_err(|_|"目录缓存不可用")?.menu_get(current,&path,&p.root){
    return Ok(listing(&path,&p.root,&cached[..cached.len().min(MAX_ITEMS)],cached.len()>MAX_ITEMS||p.watch_error.is_some()||p.metadata_error.is_some(),current))
   }
   root=Some(p.root.clone());
  }
  partial|=p.watch_error.is_some()||p.metadata_error.is_some();
  for child in p.children{
   if !basename(&child.name)||!names.insert(child.name.clone()){return Err("同级目录记录有冲突，请进入父目录核对".into())}
   bytes+=path.as_os_str().len()+child.name.len();
   if children.len()>=MAX_ITEMS||bytes>MAX_BYTES{return Ok(listing(&path,root.as_ref().unwrap(),&children,true,current))}
   children.push(child);
  }
  let Some(next)=p.next else{
   let root=root.unwrap();
   cache.lock().map_err(|_|"目录缓存不可用")?.menu_put(current,path.clone(),root.clone(),children.clone());
   return Ok(listing(&path,&root,&children,partial,current))
  };
  if !cursors.insert(next){return Err("目录分页重复，请重新打开".into())}
  after=Some(next);
  if page_index+1==MAX_PAGES||started.elapsed()>std::time::Duration::from_secs(3){return Ok(listing(&path,root.as_ref().unwrap(),&children,true,current))}
 }
 unreachable!()
}
#[cfg(test)]mod tests{
 use super::*;use std::sync::atomic::Ordering;
 fn item(id:u32,name:&str,size:u64,dir:bool)->Item{Item{id,name:name.into(),logical:size,directory:dir,files:1,directories:1,unknown:0}}
 fn page(children:Vec<Item>,next:Option<u32>)->Page{Page{epoch:7,generation:11,root:item(1,"C",100,true),children,next,resident_bytes:0,state:"fixture".into(),watch_error:None,metadata_error:None}}
 #[tokio::test]async fn parent_only_paging_size_order_identity_and_verified_cache(){
  let cache=Arc::new(Mutex::new(PageCache::default()));let cancel=Arc::new(AtomicBool::new(false));let mut calls=0;
  let result=load("C".into(),Some((7,11)),cache.clone(),cancel.clone(),|after,expected|{calls+=1;if after.is_none(){assert_eq!(expected,Some((7,11)))}else{assert_eq!(expected,Some((7,0)))}let p=if after.is_none(){page(vec![item(2,"Windows",20,true)],Some(3))}else{assert_eq!(after,Some(3));page(vec![item(3,"Users",80,true)],None)};async move{Ok(p)}}).await.unwrap();
  assert_eq!(calls,2);assert_eq!(result.items.entries[0].path,PathBuf::from("C").join("Users"));assert_eq!(result.total,100);assert!(!result.items.partial);
  let again=load("C".into(),Some((7,11)),cache,cancel,|after,_|{assert_eq!(after,None);async{Ok(page(vec![],Some(99)))}}).await.unwrap();assert_eq!(again.items.entries.len(),2);
 }
 #[tokio::test]async fn live_generation_drift_is_accepted_while_conflicts_cycles_and_cancel_are_rejected(){
  // Generation churn alone is a normal live-volume condition.
  let live=load("C".into(),Some((7,11)),Arc::new(Mutex::new(PageCache::default())),Arc::new(AtomicBool::new(false)),move|_,_|{let mut p=page(vec![],None);p.generation+=1;async move{Ok(p)}}).await;
  assert!(live.is_ok(),"generation drift must not reject siblings: {:?}",live.err());
  for variant in 1..4{
   let cancel=Arc::new(AtomicBool::new(false));let c=cancel.clone();
   let result=load("C".into(),Some((7,11)),Arc::new(Mutex::new(PageCache::default())),cancel,move|_,_|{let mut p=page(vec![],Some(5));match variant{1=>p.children=vec![item(2,"../escape",1,true)],2=>{},_=>c.store(true,Ordering::Release)}async move{Ok(p)}}).await;
   assert!(result.is_err());
  }
 }
 #[tokio::test]async fn empty_directory_and_hard_page_budget_are_honest(){
  let cache=Arc::new(Mutex::new(PageCache::default()));let cancel=Arc::new(AtomicBool::new(false));
  let empty=load("empty".into(),None,cache.clone(),cancel.clone(),|_,_|async{Ok(page(vec![],None))}).await.unwrap();assert!(empty.items.entries.is_empty());assert!(!empty.items.partial);
  let mut n=0;let limited=load("large".into(),None,cache,cancel,|_,_|{n+=1;let p=page(vec![item(n+1,&format!("d{n}"),1,true)],Some(n));async move{Ok(p)}}).await.unwrap();assert_eq!(n as usize,MAX_PAGES);assert!(limited.items.partial);
 }
}

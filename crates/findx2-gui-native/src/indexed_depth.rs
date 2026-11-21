//! Bounded read-only descendant composition; no filesystem scan or index mutation.
use crate::{disk_usage::Scan,indexed_usage};
use findx2_ipc::{UsageItemDto as Item,UsagePageDto as Page};
use std::{collections::{BinaryHeap,HashMap,HashSet,VecDeque},path::PathBuf,sync::{Arc,Mutex,atomic::AtomicBool},future::Future};
pub const MAX_NODES:usize=8192;
pub const MAX_REQUESTS:usize=32;
pub const MAX_PAGES:usize=64;
pub const MAX_DIRECTORY_PAGES:usize=16;
pub const MAX_PATH_BYTES:usize=8*1024*1024;
type Key=(u64,PathBuf);
/// Only complete, verified directories or explicitly closed conflicts are cached.
/// Opaque service continuation leases NEVER survive into another batch.
struct Directory {root:Item,children:Arc<Vec<Item>>,conflict:Option<String>}
#[derive(Default)]
pub struct PageCache{dirs:VecDeque<(Key,Arc<Directory>)>,items:usize,path_bytes:usize}
/// Identity of the directory record, not its live aggregate. Logical totals,
/// file counts and unknown flags move on every watcher delta; requiring them
/// to be byte-identical made descendant expansion unusable on busy volumes.
fn equivalent(a:&Item,b:&Item)->bool{a.id==b.id&&a.directory==b.directory}
impl PageCache{
 fn has(&self,k:&Key,expected:&Item)->bool{self.dirs.iter().any(|(key,d)|key==k&&equivalent(&d.root,expected))}
 fn get(&mut self,k:&Key,expected:&Item)->Option<Arc<Directory>>{let i=self.dirs.iter().position(|(key,d)|key==k&&equivalent(&d.root,expected))?;let v=self.dirs.remove(i)?;let out=v.1.clone();self.dirs.push_back(v);Some(out)}
 /// Menu reads share the same bounded cache, but first revalidate the parent
 /// identity, metrics and epoch with one authoritative parent page.
 pub(crate) fn menu_get(&mut self,stamp:(u64,u64),path:&std::path::Path,root:&Item)->Option<Arc<Vec<Item>>>{self.get(&(stamp.0,path.to_path_buf()),root).filter(|d|d.conflict.is_none()).map(|d|d.children.clone())}
 pub(crate) fn menu_put(&mut self,stamp:(u64,u64),path:PathBuf,root:Item,children:Vec<Item>){self.put((stamp.0,path),Arc::new(Directory{root,children:Arc::new(children),conflict:None}));}
 fn put(&mut self,k:Key,d:Arc<Directory>){
  if d.children.len()>MAX_NODES{return}
  let bytes=k.1.as_os_str().len();if bytes>MAX_PATH_BYTES{return}
  if let Some(i)=self.dirs.iter().position(|(key,_)|key==&k){let (old_key,old)=self.dirs.remove(i).unwrap();self.items-=old.children.len();self.path_bytes-=old_key.1.as_os_str().len();}
  while self.dirs.len()>=MAX_NODES||self.items+d.children.len()>MAX_NODES||self.path_bytes+bytes>MAX_PATH_BYTES{let Some((old_key,old))=self.dirs.pop_front()else{break};self.items-=old.children.len();self.path_bytes-=old_key.1.as_os_str().len();}
  self.items+=d.children.len();self.path_bytes+=bytes;self.dirs.push_back((k,d));
 }
}
pub struct TreeLoad{pub scan:Scan,pub max_depth:usize,pub more:bool,pub limited:bool,pub requests:usize,pub cache_hits:usize,pub issue:Option<String>,pub blocked:HashSet<PathBuf>}
fn leaf_name(s:&str)->bool{!s.is_empty()&&s!="."&&s!=".."&&!s.contains(['/', '\\', ':','\0'])}
pub fn data_depth(scan:&Scan)->usize{scan.nodes.iter().enumerate().filter(|(_,n)|n.logical>0).map(|(mut i,_)|{let mut d=0;while let Some(parent)=scan.nodes[i].parent{d+=1;if parent>=i||d>=6{break}i=parent;}d}).max().unwrap_or(1).max(1)}
/// Directory pages are staged and validated BEFORE any child or queue index is
/// published. Conflicting names close only that directory; every authoritative
/// parent total remains intact and confirmed siblings can still expand.
pub async fn expand<F,Fut>(root:&Page,path:PathBuf,depth:usize,cache:Arc<Mutex<PageCache>>,cancel:Arc<AtomicBool>,mut fetch:F)->Result<TreeLoad,String>
where F:FnMut(PathBuf,Option<u32>,(u64,u64))->Fut,Fut:Future<Output=Result<Page,String>>{
 indexed_usage::ensure_not_cancelled(&cancel)?;
 let stamp=(root.epoch,0);let depth=depth.clamp(1,6);
 {let mut c=cache.lock().map_err(|_|"usage cache poisoned")?;c.dirs.retain(|(k,_)|k.0==stamp.0);c.items=c.dirs.iter().map(|(_,d)|d.children.len()).sum();c.path_bytes=c.dirs.iter().map(|(k,_)|k.1.as_os_str().len()).sum();}
 if root.children.len()+1>MAX_NODES{return Err("indexed root page exceeds GUI node budget".into())}
 if (root.children.len()+1).saturating_mul(path.as_os_str().len()).saturating_add(root.children.iter().map(|c|c.name.len()+1).sum::<usize>())>MAX_PATH_BYTES{return Err("indexed root paths exceed GUI byte budget".into())}
 let mut scan=indexed_usage::scan_from_page(root,path);
 let mut items=vec![root.root.clone()];items.extend(root.children.iter().cloned());
 let mut levels=vec![0];levels.resize(items.len(),1);
 let mut queue=BinaryHeap::new();let mut blocked=HashSet::new();let mut issue=None;
 let mut root_names=HashMap::new();
 for i in 1..items.len(){
  if !leaf_name(&items[i].name){return Err("invalid root child basename".into())}
  if let Some(first)=root_names.insert(items[i].name.clone(),i){blocked.insert(scan.nodes[i].path.clone());scan.nodes[first].uncertain=true;scan.nodes[i].uncertain=true;issue=Some("根目录存在同名不同记录，相关项未展开且不可操作".into());}
 }
 for i in 1..items.len(){if depth>1&&items[i].directory&&items[i].logical>0&&!blocked.contains(&scan.nodes[i].path){let cached=cache.lock().map_err(|_|"usage cache poisoned")?.has(&(stamp.0,scan.nodes[i].path.clone()),&items[i]);queue.push((std::cmp::Reverse(1usize),cached,items[i].logical,std::cmp::Reverse(scan.nodes[i].path.clone()),i));}}
 let mut path_bytes=scan.nodes.iter().map(|n|n.path.as_os_str().len()).sum::<usize>();let started=std::time::Instant::now();
 let mut requests=0;let mut cache_hits=0;let mut visits=0;let mut limited=false;
 while let Some((std::cmp::Reverse(level),_,_,_,parent))=queue.pop(){
  indexed_usage::ensure_not_cancelled(&cancel)?;if level>=depth{continue}
  let key=(stamp.0,scan.nodes[parent].path.clone());
  let cached=cache.lock().map_err(|_|"usage cache poisoned")?.get(&key,&items[parent]);let fresh=cached.is_none();
  let dir=if let Some(d)=cached{cache_hits+=1;d}else{
   let mut after=None;let mut seen=HashSet::new();let mut names:HashMap<String,u32>=HashMap::new();let mut staged=Vec::new();let mut conflict=None;let mut complete=false;let mut staged_bytes=0;let mut directory_budget=false;
   for _ in 0..MAX_DIRECTORY_PAGES{
    indexed_usage::ensure_not_cancelled(&cancel)?;
    if requests>=MAX_REQUESTS||visits>=MAX_PAGES||started.elapsed()>std::time::Duration::from_secs(3){limited=true;break}
    requests+=1;visits+=1;
    let p=match fetch(scan.nodes[parent].path.clone(),after,stamp).await{
     Ok(p)=>p,
     Err(e)=>{indexed_usage::ensure_not_cancelled(&cancel)?;let kind=e.to_ascii_lowercase();if ["generation","epoch","identity","cursor","continuation","代际","身份"].iter().any(|k|kind.contains(k)){return Err(e)}scan.nodes[parent].uncertain=true;limited=true;scan.partial=true;issue=Some(format!("部分目录读取失败：{}\n{}",scan.nodes[parent].path.display(),e));directory_budget=true;break;}
    };
    indexed_usage::ensure_not_cancelled(&cancel)?;
    if p.epoch!=stamp.0{return Err("usage index reloaded during descendant expansion; refresh".into())}
    if !equivalent(&p.root,&items[parent]){return Err("usage directory identity/metrics changed during expansion; refresh".into())}
    if p.watch_error.is_some()||p.metadata_error.is_some(){scan.partial=true;if issue.is_none(){issue=p.watch_error.clone().or_else(||p.metadata_error.clone());}}
    for child in &p.children{
     if !leaf_name(&child.name){conflict=Some(format!("目录名称无效，保留汇总未展开：{}",scan.nodes[parent].path.display()));break}
     if let Some(first)=names.insert(child.name.clone(),child.id){conflict=Some(format!("目录身份冲突，保留汇总未展开：{} / {}（记录{}与{}）",scan.nodes[parent].path.display(),child.name,first,child.id));break}
     staged_bytes+=scan.nodes[parent].path.as_os_str().len()+child.name.len()+1;
     if scan.nodes.len()+staged.len()+1>MAX_NODES||path_bytes+staged_bytes>MAX_PATH_BYTES{limited=true;directory_budget=true;break}
     staged.push(child.clone());
    }
    if conflict.is_some()||directory_budget{break}
    if scan.nodes.len()+staged.len()>=MAX_NODES||path_bytes+staged_bytes>=MAX_PATH_BYTES{limited=true;break}
    let Some(next)=p.next else{complete=true;break};if !seen.insert(next){return Err("usage descendant continuation cycle".into())}after=Some(next);
   }
   if !complete&&conflict.is_none(){limited=true;continue}
   Arc::new(Directory{root:items[parent].clone(),children:Arc::new(if conflict.is_some(){vec![]}else{staged}),conflict})
  };
  if let Some(reason)=&dir.conflict{
   scan.nodes[parent].uncertain=true;blocked.insert(scan.nodes[parent].path.clone());scan.partial=true;issue=Some(reason.clone());
   if fresh{cache.lock().map_err(|_|"usage cache poisoned")?.put(key,dir.clone());}continue;
  }
  let bytes=dir.children.iter().map(|c|scan.nodes[parent].path.as_os_str().len()+c.name.len()+1).sum::<usize>();
  if scan.nodes.len()+dir.children.len()>MAX_NODES||path_bytes+bytes>MAX_PATH_BYTES{limited=true;continue}
  for child in dir.children.iter(){
   let id=scan.nodes.len();let child_path=scan.nodes[parent].path.join(&child.name);path_bytes+=child_path.as_os_str().len();
   scan.nodes.push(crate::disk_usage::Node{path:child_path.clone(),parent:Some(parent),children:vec![],entries:child.files.saturating_add(child.directories),files:child.files,directories:child.directories,category:0,own_allocated:0,directory:child.directory,logical:child.logical,mtime:0,allocated:0,hardlink_duplicate:false,uncertain:child.unknown>0});
   scan.nodes[parent].children.push(id);items.push(child.clone());levels.push(level+1);
   // Terminal-depth children are published, but can never be expanded in this batch.
   // Do not clone their paths, scan the cache or enqueue/pop thousands of no-op visits.
   if level+1<depth&&child.directory&&child.logical>0{let cached=cache.lock().map_err(|_|"usage cache poisoned")?.has(&(stamp.0,child_path.clone()),child);queue.push((std::cmp::Reverse(level+1),cached,child.logical,std::cmp::Reverse(child_path),id));}
  }
  if fresh{cache.lock().map_err(|_|"usage cache poisoned")?.put(key,dir);}
 }
 let max_depth=data_depth(&scan);
 let more=scan.nodes.iter().enumerate().any(|(i,n)|n.directory&&n.logical>0&&n.children.is_empty()&&(n.entries>0||n.uncertain)&&levels[i]>0&&!blocked.contains(&n.path));
 scan.partial|=limited||!blocked.is_empty();
 Ok(TreeLoad{scan,max_depth,more,limited,requests,cache_hits,issue,blocked})
}
pub fn same_geometry(a:&Scan,b:&Scan)->bool{a.logical_generation.map(|s|s.0)==b.logical_generation.map(|s|s.0)&&a.nodes.len()==b.nodes.len()&&a.nodes.iter().zip(&b.nodes).all(|(a,b)|a.path==b.path&&a.parent==b.parent&&a.children==b.children&&a.directory==b.directory&&a.logical==b.logical)}
#[cfg(test)]
mod tests{
 use super::*;
 fn item(id:u32,name:&str,directory:bool,logical:u64,files:u64,directories:u64)->Item{Item{id,name:name.into(),directory,logical,files,directories,unknown:0}}
 fn page(root:Item,children:Vec<Item>)->Page{Page{epoch:7,generation:11,root,children,next:None,resident_bytes:0,state:"owned fixture".into(),watch_error:None,metadata_error:None}}
 fn fixture()->(Page,Page,Page){
  let a=item(2,"A",true,80,2,1);let d=item(3,"deep",true,80,2,0);
  (page(item(1,"R",true,100,3,2),vec![a.clone(),item(9,"root.txt",false,20,1,0)]),page(a,vec![d.clone()]),page(d,vec![item(4,"a.txt",false,40,1,0),item(5,"b.bin",false,40,1,0)]))
 }
 #[tokio::test] async fn real_adapter_depth_masks_and_parent_geometry_are_stable_and_cache_reuses_all_descendants(){
  let(r,a,d)=fixture();let cache=Arc::new(Mutex::new(PageCache::default()));let c=Arc::new(AtomicBool::new(false));let mut scenes=vec![];
  for depth in 1..=3{
   let a=a.clone();let d=d.clone();let t=expand(&r,"R".into(),depth,cache.clone(),c.clone(),move|path,_,_|{let p=if path.ends_with("deep"){d.clone()}else{a.clone()};async move{Ok(p)}}).await.unwrap();
   assert_eq!(t.max_depth,depth);assert!(!t.limited);
   let scene=crate::usage_view::build_scene(&t.scan,crate::usage_view::SceneKey{scan:1,root:0,depth:6,width:1000,height:620,detail:1});
   assert!(scene.tiles.iter().any(|x|x.depth==depth));
   let parents=scene.tiles.iter().filter(|t|t.depth==1).map(|t|t.rect).collect::<Vec<_>>();
   scenes.push(parents);
  }
  assert_eq!(scenes[0],scenes[1]);assert_eq!(scenes[1],scenes[2]);
  let t=expand(&r,"R".into(),3,cache,c,|_,_,_|async{panic!("unchanged refresh must reuse cached descendants")}).await.unwrap();
  assert_eq!(t.requests,0);assert_eq!(t.cache_hits,2);assert_eq!(t.max_depth,3);assert!(!t.more);
 }
 #[tokio::test] async fn live_generation_drift_is_accepted_but_identity_changes_are_not(){
  let(r,a,d)=fixture();
  // Ordinary watch deltas advance the page generation; they must not abort a
  // descendant read on an actively written volume.
  let mut live=a.clone();live.generation+=1;
  let ok=expand(&r,"R".into(),3,Arc::default(),Arc::new(AtomicBool::new(false)),move|path,_,_|{let a=live.clone();let d=d.clone();async move{Ok(if path.ends_with("deep"){d}else{a})}}).await;
  assert!(ok.is_ok(),"generation drift must not abort expansion: {:?}",ok.err());
  // A changed record identity or a server-side refusal still aborts.
  for mode in 1..3{
   let mut a=a.clone();if mode==1{a.root.id+=1}
   let err=expand(&r,"R".into(),3,Arc::default(),Arc::new(AtomicBool::new(false)),move|_,_,_|{let a=a.clone();async move{if mode==2{Err("generation changed".into())}else{Ok(a)}}}).await.err().unwrap();assert!(!err.is_empty());
  }
 }
 #[tokio::test] async fn cancellation_stops_new_descendant_requests(){
  let(r,a,_)=fixture();let c=Arc::new(AtomicBool::new(false));let cc=c.clone();
  let result=expand(&r,"R".into(),3,Arc::default(),c,move|_,_,_|{let a=a.clone();cc.store(true,std::sync::atomic::Ordering::Release);async move{Ok(a)}}).await;
  assert!(result.is_err());
 }
 #[tokio::test] async fn budget_is_explicit_and_never_changes_parent_weights(){
  let children=(0..40).map(|i|item(i+1,&format!("D{i}"),true,10,1,0)).collect::<Vec<_>>();let r=page(item(0,"R",true,400,40,40),children.clone());
  let t=expand(&r,"R".into(),3,Arc::default(),Arc::new(AtomicBool::new(false)),move|path,_,_|{let n=path.file_name().unwrap().to_str().unwrap();let p=page(children.iter().find(|c|c.name==n).unwrap().clone(),vec![item(100,"file.txt",false,10,1,0)]);async move{Ok(p)}}).await.unwrap();
  assert!(t.limited&&t.more&&t.scan.partial);assert_eq!(t.requests,MAX_REQUESTS);assert_eq!(t.scan.nodes[0].logical,400);assert!(t.scan.nodes.len()<=MAX_NODES);
 }
 #[tokio::test] async fn nested_pagination_is_retained_and_cycles_rejected(){
  let(r,a,_)=fixture();
  let mut first=a.clone();first.children=vec![item(5,"one.txt",false,40,1,0)];first.next=Some(123);
  let mut second=first.clone();second.children=vec![item(6,"two.txt",false,40,1,0)];second.next=None;
  let t=expand(&r,"R".into(),2,Arc::default(),Arc::new(AtomicBool::new(false)),move|_,after,_|{let p=if after==Some(123){second.clone()}else{first.clone()};async move{Ok(p)}}).await.unwrap();assert_eq!(t.scan.nodes[1].children.len(),2);
  let mut loop_page=a;loop_page.children.clear();loop_page.next=Some(123);
  let bad=expand(&r,"R".into(),2,Arc::default(),Arc::new(AtomicBool::new(false)),move|_,_,_|{let p=loop_page.clone();async move{Ok(p)}}).await;
  assert!(bad.err().unwrap().contains("cycle"));
 }
}

#[cfg(test)] mod conflict_regressions {
 use super::*;
 fn item(id:u32,name:&str,dir:bool,size:u64,files:u64,dirs:u64)->Item{Item{id,name:name.into(),directory:dir,logical:size,files,directories:dirs,unknown:0}}
 fn page(root:Item,children:Vec<Item>)->Page{Page{epoch:1,generation:5,root,children,next:None,resident_bytes:0,state:"fixture".into(),watch_error:None,metadata_error:None}}
 #[tokio::test] async fn conflicting_identity_branch_is_closed_atomically_while_confirmed_sibling_reaches_depth_three(){
  let a=item(2,"bad",true,80,2,0);let b=item(3,"good",true,20,1,1);let nested=item(4,"nested",true,20,1,0);
  let root=page(item(1,"R",true,100,3,3),vec![a.clone(),b.clone()]);
  let bad=page(a,vec![item(4881824,"same.rmeta",false,40,1,0),item(4881717,"same.rmeta",false,40,1,0)]);
  let good=page(b,vec![nested.clone()]);let leaf=page(nested,vec![item(9,"owned.txt",false,20,1,0)]);
  let cache=Arc::new(Mutex::new(PageCache::default()));
  let tree=expand(&root,"R".into(),3,cache.clone(),Arc::new(AtomicBool::new(false)),move|path,_,_|{let p=if path.ends_with("bad"){bad.clone()}else if path.ends_with("good"){good.clone()}else{leaf.clone()};async move{Ok(p)}}).await.unwrap();
  assert_eq!(tree.max_depth,3);assert_eq!(tree.scan.nodes[0].logical,100);assert_eq!(tree.scan.nodes[1].logical,80);assert!(tree.scan.nodes[1].children.is_empty());assert!(tree.scan.nodes[1].uncertain);assert!(tree.blocked.contains(&PathBuf::from("R").join("bad")));assert!(tree.scan.partial);assert_eq!(tree.scan.nodes.len(),5);
  assert!(tree.issue.as_ref().unwrap().contains("4881824与4881717"));
  let again=expand(&root,"R".into(),3,cache,Arc::new(AtomicBool::new(false)),|_,_,_|async{panic!("confirmed directories and exact conflict must be cacheable without opaque cursors")}).await.unwrap();
  assert_eq!(again.requests,0);assert!(same_geometry(&tree.scan,&again.scan));assert_eq!(tree.blocked,again.blocked);
 }
 #[tokio::test] async fn conflicting_later_page_never_leaks_previously_staged_names_or_children(){
  let a=item(2,"bad",true,80,2,0);let root=page(item(1,"R",true,80,2,1),vec![a.clone()]);let mut first=page(a.clone(),vec![item(4,"dup",false,40,1,0)]);first.next=Some(90);let second=page(a,vec![item(5,"dup",false,40,1,0)]);
  let t=expand(&root,"R".into(),3,Arc::default(),Arc::new(AtomicBool::new(false)),move|_,after,_|{let p=if after.is_some(){second.clone()}else{first.clone()};async move{Ok(p)}}).await.unwrap();
  assert_eq!(t.scan.nodes.len(),2);assert!(t.scan.nodes[1].children.is_empty());assert_eq!(t.scan.nodes[1].logical,80);assert_eq!(t.blocked.len(),1);
 }
 #[tokio::test] async fn next_epoch_does_not_reuse_a_previous_conflict_or_old_children(){
  let a=item(2,"A",true,80,2,0);let root=page(item(1,"R",true,80,2,1),vec![a.clone()]);let bad=page(a.clone(),vec![item(4,"dup",false,40,1,0),item(5,"dup",false,40,1,0)]);let cache=Arc::new(Mutex::new(PageCache::default()));
  let t=expand(&root,"R".into(),2,cache.clone(),Arc::new(AtomicBool::new(false)),move|_,_,_|{let p=bad.clone();async move{Ok(p)}}).await.unwrap();assert_eq!(t.blocked.len(),1);
  let mut root=root;root.epoch+=1;let mut good=page(a,vec![item(4,"one",false,40,1,0),item(5,"two",false,40,1,0)]);good.epoch+=1;
  let t=expand(&root,"R".into(),2,cache,Arc::new(AtomicBool::new(false)),move|_,_,_|{let p=good.clone();async move{Ok(p)}}).await.unwrap();assert!(t.blocked.is_empty());assert_eq!(t.requests,1);assert_eq!(t.scan.nodes.len(),4);
 }
}

#[cfg(test)] mod retained_directory_cache_tests {
 use super::*;
 #[tokio::test] async fn more_than_sixty_four_verified_directories_survive_refresh_without_refetch_or_flatten(){
  let item=|id,name:String,directory,logical,files,directories|Item{id,name,directory,logical,files,directories,unknown:0};
  let root=Page{epoch:1,generation:8,root:item(1,"R".into(),true,80,80,80),children:(0..80).map(|i|item(i+2,format!("dir{i:03}"),true,1,1,0)).collect(),next:None,resident_bytes:0,state:"fixture".into(),watch_error:None,metadata_error:None};
  let cache=Arc::new(Mutex::new(PageCache::default()));let mut counts=vec![];
  for pass in 0..4 {
   let r=root.clone();let tree=expand(&root,"R".into(),2,cache.clone(),Arc::new(AtomicBool::new(false)),move|path,_,_|{let name=path.file_name().unwrap().to_str().unwrap();let i=name.trim_start_matches("dir").parse::<u32>().unwrap();let mut p=r.clone();p.root=r.children[i as usize].clone();p.children=vec![Item{id:1000+i,name:"leaf".into(),directory:false,logical:1,files:1,directories:0,unknown:0}];async move{Ok(p)}}).await.unwrap();
   counts.push(tree.scan.nodes.len());if pass==3{assert_eq!(tree.requests,0);assert_eq!(tree.scan.nodes.len(),161);}
  }
  assert!(counts.windows(2).all(|w|w[1]>=w[0]));
 }
}

#[cfg(test)]mod partial_failure_tests{
 use super::*;
 #[tokio::test]async fn unreadable_descendant_keeps_authoritative_parent_and_reports_exact_scope(){
 let i=|id,name:&str,logical|Item{id,name:name.into(),directory:true,logical,files:10,directories:2,unknown:10};let r=Page{epoch:1,generation:2,root:i(1,"R",100),children:vec![i(2,"denied",80),i(3,"good",20)],next:None,resident_bytes:0,state:"fixture".into(),watch_error:None,metadata_error:None};
 let tree=expand(&r,"R".into(),2,Arc::default(),Arc::new(AtomicBool::new(false)),move|path,_,_|{let good=Page{epoch:1,generation:2,root:i(3,"good",20),children:vec![],next:None,resident_bytes:0,state:"fixture".into(),watch_error:None,metadata_error:None};async move{if path.ends_with("denied"){Err("access denied".into())}else{Ok(good)}}}).await.unwrap();
 assert_eq!(tree.scan.nodes[0].logical,100);assert!(tree.scan.nodes.iter().any(|n|n.path.ends_with("good")));assert!(tree.scan.partial&&tree.limited);let issue=tree.issue.unwrap();assert!(issue.contains("denied")&&issue.contains("access denied"));
 }
}

/// Rendering availability is not a user preference mutation.
pub fn effective_depth(preferred:usize,available:usize)->usize{preferred.clamp(1,6).min(available.max(1))}
#[cfg(test)]mod preferred_depth_regression{
 use super::*;
 #[tokio::test]async fn selected_three_or_two_survives_leaf_parent_cache_and_async_publication(){
  use findx2_ipc::{UsageItemDto as I,UsagePageDto as P};
  let item=|id,name:&str,dir,files,dirs|I{id,name:name.into(),directory:dir,logical:10,files,directories:dirs,unknown:0};
  let root=item(1,"R",true,1,2);let a=item(2,"A",true,1,1);let d=item(3,"D",true,1,0);let f=item(4,"file",false,1,0);
  let page=|root,children|P{epoch:7,generation:11,root,children,next:None,resident_bytes:0,state:"owned".into(),watch_error:None,metadata_error:None};
  let deep=page(root,vec![a.clone()]);let pa=page(a,vec![d.clone()]);let pd=page(d,vec![f.clone()]);let shallow=page(item(90,"leaf",true,1,0),vec![f]);
  for preferred in [3,2,1]{let cache:Arc<Mutex<PageCache>>=Arc::default();let cancel=Arc::new(AtomicBool::new(false));let aa=pa.clone();let dd=pd.clone();let t=expand(&deep,"R".into(),preferred,cache.clone(),cancel.clone(),move|p,_,_|{let page=if p.ends_with("D"){dd.clone()}else{aa.clone()};async move{Ok(page)}}).await.unwrap();assert_eq!(effective_depth(preferred,t.max_depth),preferred);
   let leaf=expand(&shallow,"leaf".into(),preferred,Arc::default(),cancel.clone(),|_,_,_|async{panic!("leaf must not fetch descendants")}).await.unwrap();assert_eq!(effective_depth(preferred,leaf.max_depth),1);assert_eq!(preferred,preferred.clamp(1,6));
   let restored=expand(&deep,"R".into(),preferred,cache,cancel,|_,_,_|async{panic!("parent return uses bounded cache")}).await.unwrap();assert_eq!(effective_depth(preferred,restored.max_depth),preferred);
  }
 }
 #[test]fn production_publication_and_navigation_never_write_preference(){let ws=include_str!("ui/workspace.rs");assert_eq!(ws.matches("self.workspace.usage_depth=").count(),1);assert!(!ws.contains("this.workspace.usage_depth="));assert!(!include_str!("ui/workspace_usage.rs").contains("this.workspace.usage_depth ="));assert!(!ws.contains("usage_target_depth"));}
}

#[cfg(test)] mod retained_cache_regression {
 use super::*;
 fn item(id:u32,name:&str,bytes:u64,dirs:u64)->Item{Item{id,name:name.into(),directory:true,logical:bytes,files:0,directories:dirs,unknown:0}}
 fn page(root:Item,children:Vec<Item>)->Page{Page{epoch:7,generation:11,root,children,next:None,resident_bytes:0,state:"bounded roundtrip".into(),watch_error:None,metadata_error:None}}
 #[tokio::test] async fn cross_root_return_reuses_only_matching_stamp_identity_and_metrics(){
  let child=item(2,"A",80,1);let deep=item(3,"B",80,0);let root=page(item(1,"R",80,2),vec![child.clone()]);let a=page(child,vec![deep.clone()]);let b=page(deep,vec![]);let cache=Arc::new(Mutex::new(PageCache::default()));
  let warm=expand(&root,"C:/roundtrip".into(),3,cache.clone(),Arc::new(AtomicBool::new(false)),|p,_,_|{let a=a.clone();let b=b.clone();async move{Ok(if p.ends_with("A"){a}else{b})}}).await.unwrap();assert_eq!(warm.requests,2);
  let entered=expand(&a,"C:/roundtrip/A".into(),2,cache.clone(),Arc::new(AtomicBool::new(false)),|_,_,_|async{panic!("valid child root must reuse cached direct descendants")}).await.unwrap();assert_eq!(entered.requests,0);assert_eq!(entered.cache_hits,1);
  let back=expand(&root,"C:/roundtrip".into(),3,cache.clone(),Arc::new(AtomicBool::new(false)),|_,_,_|async{panic!("unchanged return must reuse verified cache")}).await.unwrap();assert_eq!(back.requests,0);assert_eq!(back.cache_hits,2);assert!(same_geometry(&warm.scan,&back.scan));
  let mut changed=root.clone();changed.children[0].id=99;assert!(expand(&changed,"C:/roundtrip".into(),3,cache.clone(),Arc::new(AtomicBool::new(false)),|_,_,_|{let a=a.clone();async move{Ok(a)}}).await.err().unwrap().contains("identity"));
  // Watch deltas advance the data generation; the verified cache remains valid.
  let mut changed=root.clone();changed.generation+=1;assert_eq!(expand(&changed,"C:/roundtrip".into(),3,cache.clone(),Arc::new(AtomicBool::new(false)),|_,_,_|async{panic!("generation drift must reuse verified cache")}).await.unwrap().requests,0);
  // A runtime reload (epoch) is the real boundary.
  let mut changed=root.clone();changed.epoch+=1;assert!(expand(&changed,"C:/roundtrip".into(),3,cache.clone(),Arc::new(AtomicBool::new(false)),|_,_,_|{let a=a.clone();async move{Ok(a)}}).await.err().unwrap().contains("reloaded"));
  assert!(expand(&root,"C:/roundtrip".into(),3,cache,Arc::new(AtomicBool::new(true)),|_,_,_|async{panic!("cancelled reuse must not fetch")}).await.is_err());
 }
 #[test] fn navigation_does_not_destroy_bounded_cache_but_resets_available_depth(){let s=include_str!("ui/workspace.rs");assert!(!s.contains("self.workspace.usage_page_cache=Arc::new"));assert!(s.contains("self.workspace.usage_loaded_depth=1"));}
}

#[cfg(test)] mod boundary_work_tests {
 use super::*;
 fn item(id:u32,name:String,logical:u64,dirs:u64)->Item{Item{id,name,directory:true,logical,files:0,directories:dirs,unknown:0}}
 fn page(root:Item,children:Vec<Item>)->Page{Page{epoch:7,generation:11,root,children,next:None,resident_bytes:0,state:"bounded directory-only fixture".into(),watch_error:None,metadata_error:None}}
 #[tokio::test] #[ignore] async fn profile_warm_depth_boundary(){
  let root=item(1,"R".into(),64000,6464);
  let parents=(0..64).map(|i|item(2+i,format!("parent-{i:03}"),1000,100)).collect::<Vec<_>>();
  let p=page(root,parents.clone());let cache=Arc::new(Mutex::new(PageCache::default()));
  {let mut c=cache.lock().unwrap();for (i,parent) in parents.iter().enumerate(){c.menu_put((7,11),PathBuf::from("C:/warm-fixture").join(&parent.name),parent.clone(),(0..100).map(|j|item(1000+i as u32*100+j,format!("terminal-{j:03}"),10,1)).collect());}}
  let mut elapsed=Vec::new();for _ in 0..12{
   let t=std::time::Instant::now();let r=expand(&p,PathBuf::from("C:/warm-fixture"),2,cache.clone(),Arc::new(AtomicBool::new(false)),|_,_,_|async{panic!("warm-only must not fetch")}).await.unwrap();elapsed.push(t.elapsed().as_micros());
   assert_eq!(r.scan.nodes.len(),6465);assert_eq!(r.requests,0);assert_eq!(r.cache_hits,64);assert_eq!(r.max_depth,2);assert!(r.more);assert!(!r.limited);assert!(r.blocked.is_empty());assert!(r.issue.is_none());assert_eq!(r.scan.nodes[0].logical,64000);
  }elapsed.sort();eprintln!("warm_boundary_fixture nodes=6465 depth=2 iterations=12 median_us={} min_us={} max_us={}",elapsed[6],elapsed[0],elapsed[11]);
 }
}

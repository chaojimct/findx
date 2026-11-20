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
/// Freshly started services bump the usage generation while a page is in flight.
/// Retry the directory from page 0 a few times, then stop.
pub const MAX_GENERATION_RESTARTS: u32 = 3;
pub const GENERATION_RESTART_MESSAGE: &str = "索引正在实时更新，请稍后重试";

pub fn is_usage_generation_restart(err: &str) -> bool {
    let kind = err.to_ascii_lowercase();
    kind.contains("generation changed")
        || kind.contains("restart page")
        || kind.contains("restart pagination")
}

pub enum GenerationRetry {
    Again(Duration),
    Exhausted,
    Other,
}

pub fn generation_restart_backoff(restart: u32) -> Duration {
    Duration::from_millis(120 * u64::from(restart.max(1)))
}

/// `restarts` is how many full page-0 retries have already been issued.
pub fn generation_retry(restarts: u32, err: &str) -> GenerationRetry {
    if !is_usage_generation_restart(err) {
        return GenerationRetry::Other;
    }
    if restarts >= MAX_GENERATION_RESTARTS {
        GenerationRetry::Exhausted
    } else {
        GenerationRetry::Again(generation_restart_backoff(restarts + 1))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UsagePageCursor {
    pub expected: Option<(u64, u64)>,
    pub after: Option<u32>,
    pub restarts: u32,
}

impl UsagePageCursor {
    /// A generation mismatch drops the continuation and starts again at page 0.
    pub fn restart_after(mut self, err: &str) -> (Self, GenerationRetry) {
        let decision = generation_retry(self.restarts, err);
        if matches!(decision, GenerationRetry::Again(_)) {
            self.restarts += 1;
            self.expected = None;
            self.after = None;
        }
        (self, decision)
    }
}

/// Footer copy for a failed usage load. "保留原画面" only when a scene is still shown.
pub fn usage_failure_footer(status: &str, busy: bool, scene_visible: bool) -> Option<&'static str> {
    let failed = status.starts_with("目录读取失败") || status == GENERATION_RESTART_MESSAGE;
    if !failed {
        return None;
    }
    Some(if busy {
        "正在重新读取目录"
    } else if scene_visible {
        "目录读取失败，保留原画面"
    } else if status == GENERATION_RESTART_MESSAGE {
        GENERATION_RESTART_MESSAGE
    } else {
        "目录读取失败，可稍后重试"
    })
}
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
 #[test]fn generation_change_restarts_from_page_zero_then_stops(){
  assert!(is_usage_generation_restart("usage generation changed; restart page"));
  assert!(is_usage_generation_restart("usage generation changed during descendant expansion; refresh"));
  assert!(is_usage_generation_restart("usage generation changed while skipping empty pages; refresh"));
  assert!(!is_usage_generation_restart("usage directory identity/metrics changed during expansion; refresh"));
  assert!(!is_usage_generation_restart("usage request cancelled"));
  assert!(matches!(generation_retry(0,"usage generation changed; restart page"),GenerationRetry::Again(_)));
  assert!(matches!(generation_retry(MAX_GENERATION_RESTARTS-1,"usage generation changed; restart page"),GenerationRetry::Again(delay) if delay==generation_restart_backoff(MAX_GENERATION_RESTARTS)));
  assert!(matches!(generation_retry(MAX_GENERATION_RESTARTS,"usage generation changed; restart page"),GenerationRetry::Exhausted));
  assert!(matches!(generation_retry(0,"permission denied"),GenerationRetry::Other));
  let continued=UsagePageCursor{expected:Some((1,2)),after:Some(9),restarts:0};
  let (restarted,decision)=continued.restart_after("usage generation changed; restart page");
  assert!(matches!(decision,GenerationRetry::Again(_)));
  assert_eq!(restarted,UsagePageCursor{expected:None,after:None,restarts:1});
  let mut cursor=restarted;
  for _ in 1..MAX_GENERATION_RESTARTS {
   let (next,decision)=cursor.restart_after("usage generation changed during descendant expansion; refresh");
   assert!(matches!(decision,GenerationRetry::Again(_)));
   assert_eq!((next.expected,next.after),(None,None));
   cursor=next;
  }
  let (exhausted,decision)=cursor.restart_after("usage generation changed; restart page");
  assert!(matches!(decision,GenerationRetry::Exhausted));
  assert_eq!(exhausted.restarts,MAX_GENERATION_RESTARTS);
  let kept=UsagePageCursor{expected:Some((4,5)),after:Some(2),restarts:0};
  let (kept,decision)=kept.restart_after("usage directory identity/metrics changed during expansion; refresh");
  assert!(matches!(decision,GenerationRetry::Other));
  assert_eq!(kept.expected,Some((4,5)));
  assert_eq!(kept.after,Some(2));
  assert!(!GENERATION_RESTART_MESSAGE.contains("generation"));
  assert_eq!(usage_failure_footer(GENERATION_RESTART_MESSAGE,false,false),Some(GENERATION_RESTART_MESSAGE));
  assert_eq!(usage_failure_footer(GENERATION_RESTART_MESSAGE,false,true),Some("目录读取失败，保留原画面"));
  assert_eq!(usage_failure_footer("目录读取失败：拒绝访问",true,true),Some("正在重新读取目录"));
  assert_eq!(usage_failure_footer("目录读取失败：拒绝访问",false,false),Some("目录读取失败，可稍后重试"));
  assert_eq!(usage_failure_footer("当前层完整",false,true),None);
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
        let requested=PathBuf::from(r"C:\Users\example\dev");
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

/// One identity for scene construction, paint, scroll and hit testing.
/// Engine revisions change only for geometry/scope changes, never badge ticks.
pub fn scene_identity(engine: bool, revision: usize, address: usize) -> usize {
    if engine { revision } else { address }
}
pub fn same_layout(scan: &Scan, requested: &std::path::Path, page: &findx2_ipc::UsagePageDto) -> bool {
    same_structure(scan, requested, page)
        && scan.nodes[0].logical == page.root.logical
        && scan.nodes[0].children.iter().zip(&page.children)
            .all(|(&i, child)| scan.nodes[i].logical == child.logical)
}

/// A result belongs to exactly one navigation request, not merely a root index (all engine pages use root 0).
pub fn request_is_current(current: u64, request: u64, current_path: &std::path::Path, requested: &std::path::Path, cancelled: bool) -> bool {
    current == request && current_path == requested && !cancelled
}
#[cfg(test)]
mod navigation_scene_tests {
    use super::*;
    use crate::usage_view::{build_scene, SceneKey};
    use std::sync::Arc;
    fn page(generation: u64, unknown: u64, bytes: u64) -> findx2_ipc::UsagePageDto {
        let item = |name: &str, logical| findx2_ipc::UsageItemDto { id: 1, name: name.into(), directory: true, logical, files: 10, directories: 2, unknown };
        findx2_ipc::UsagePageDto { epoch: 9, generation, root: item("C:", bytes), children: vec![item("Users", bytes)], next: None, resident_bytes: 0, state: "ok".into(), watch_error: None, metadata_error: None }
    }
    #[test]
    fn root_navigation_scene_is_visible_and_badges_during_pending_layout_do_not_invalidate_it() {
        let root = PathBuf::from(r"C:\");
        let first = page(1, 0, 100);
        let captured = Arc::new(scan_from_page(&first, root.clone()));
        let identity = scene_identity(true, 2, Arc::as_ptr(&captured) as usize);
        let key = SceneKey { scan: identity, root: 0, depth: 3, width: 960, height: 640, detail: 1 };
        // The background task retains the old Arc; a badge update clones its scan.
        let mut live = (*captured).clone();
        let badge = page(3, 278, 100);
        assert!(same_layout(&live, &root, &badge));
        patch_metrics(&mut live, &badge);
        let live = Arc::new(live);
        let scene = build_scene(&captured, key);
        assert_eq!(scene.key.scan, scene_identity(true, 2, Arc::as_ptr(&live) as usize));
        assert!(scene.tiles.iter().any(|t| t.depth > 0 && t.rect.w > 0. && t.rect.h > 0.));
        assert_ne!(scene.key.scan, scene_identity(true, 3, Arc::as_ptr(&live) as usize));
        assert!(!same_layout(&live, &root, &page(4, 278, 101)));
        // While a genuine metric update is building a new scene, the previous
        // published snapshot stays drawable, but never under another address.
        let changed = scan_from_page(&page(4,278,101),root.clone());
        assert!(!same_layout(&live,&root,&page(4,278,101)));
        assert!(scene_can_paint(&live,scene.key.root,&root));
        assert!(scene_can_paint(&changed,0,&root));
        assert!(!scene_can_paint(&live,scene.key.root,&PathBuf::from(r"C:\Other")));

        assert!(!same_layout(&live, &PathBuf::from(r"C:\Users"), &badge));
    }
    #[test]
    fn navigation_cancel_and_late_results_cannot_clobber_root() {
        let root=PathBuf::from(r"C:\"); let users=PathBuf::from(r"C:\Users");
        assert!(!request_is_current(3,2,&root,&users,false));
        assert!(!request_is_current(3,3,&root,&users,false));
        assert!(!request_is_current(3,3,&root,&root,true));
        assert!(request_is_current(3,3,&root,&root,false));
        assert_eq!(scene_identity(false,2,12345),12345);
    }
}

/// A pending same-scope metric/layout refresh must keep its immutable published
/// snapshot drawable. Scope changes must never paint another directory's scene.
pub fn scene_can_paint(scan: &Scan, root: usize, requested: &std::path::Path) -> bool {
    scan.nodes.get(root).is_some_and(|n| n.path == requested)
}

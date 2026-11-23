
use findx2_gui_native::{ipc::IpcClient,indexed_depth::{self,PageCache},usage_view::{self,SceneKey}};
use findx2_ipc::{IpcRequest,IpcResponse,UsagePageDto};
use std::{path::PathBuf,sync::{Arc,Mutex,atomic::AtomicBool}};
async fn get(ipc:IpcClient,endpoint:String,path:PathBuf,after:Option<u32>,expected:Option<(u64,u64)>)->Result<UsagePageDto,String>{
 eprintln!("depth_probe_fetch path={:?} after={:?}",path,after);
 match ipc.request(endpoint,IpcRequest::UsageChildren{path:path.to_string_lossy().into_owned(),after,expected,max_bytes:48*1024}).await.map_err(|_|"channel closed")??{IpcResponse::UsageChildrenResult{page}=>Ok(page),IpcResponse::Error{message}=>Err(message),_=>Err("wrong response".into())}
}
#[tokio::main]
async fn main(){
 let args=std::env::args().collect::<Vec<_>>();assert_eq!(args.len(),4);let endpoint=args[1].clone();assert_eq!(endpoint,r"\\.\pipe\findx2-t165-real","only existing independent readonly snapshot accepted");
 let path=PathBuf::from(&args[2]);let cache=Arc::new(Mutex::new(PageCache::default()));let mut reports=vec![];let mut parent_boxes=None;let mut old:Option<findx2_gui_native::disk_usage::Scan>=None;
 for depth in [1,2,3,3,3]{
  let root=get(IpcClient::start(),endpoint.clone(),path.clone(),None,None).await.unwrap();let ipc=IpcClient::start();let ep=endpoint.clone();let t=std::time::Instant::now();
  let tree=indexed_depth::expand(&root,path.clone(),depth,cache.clone(),Arc::new(AtomicBool::new(false)),move|p,a,g|{get(ipc.clone(),ep.clone(),p,a,Some(g))}).await.unwrap();
  let scene=usage_view::build_scene(&tree.scan,SceneKey{scan:1,root:0,depth:6,width:720,height:530,detail:1});
  let parents=scene.tiles.iter().filter(|t|t.depth==1).map(|t|t.rect).collect::<Vec<_>>();if let Some(ref b)=parent_boxes{assert_eq!(b,&parents,"parent geometry moved with depth")}else{parent_boxes=Some(parents)}
  let counts=(1..=3).map(|d|scene.tiles.iter().filter(|t|usage_view::visible_at_depth(t.depth,d)).count()).collect::<Vec<_>>();
  if depth==3{assert!(scene.tiles.iter().any(|t|t.depth==3),"real indexed tree has no visible third-level tiles");if let Some(ref before)=old{let current=tree.scan.nodes.iter().map(|n|(n.path.clone(),n)).collect::<std::collections::HashMap<_,_>>(); for n in &before.nodes {let now=current.get(&n.path).expect("refresh lost a confirmed node");assert_eq!(n.logical,now.logical);if !n.children.is_empty(){let prior=n.children.iter().map(|i|before.nodes[*i].path.clone()).collect::<std::collections::BTreeSet<_>>();let after=now.children.iter().map(|i|tree.scan.nodes[*i].path.clone()).collect::<std::collections::BTreeSet<_>>();assert_eq!(prior,after,"refresh flattened or changed confirmed subtree");}}}}
  reports.push(serde_json::json!({"root":path,"epoch":root.epoch,"generation":root.generation,"requested":depth,"max_depth":tree.max_depth,"nodes":tree.scan.nodes.len(),"visible_counts_at_1_2_3":counts,"actual_tiles_at_depth":(1..=3).map(|d|scene.tiles.iter().filter(|t|t.depth==d).count()).collect::<Vec<_>>(),"reads":tree.requests,"cache_hits":tree.cache_hits,"blocked":tree.blocked,"issue":tree.issue,"partial":tree.scan.partial,"limited":tree.limited,"more":tree.more,"elapsed_ms":t.elapsed().as_millis(),"parent_geometry_stable":true,"root_unknown":root.root.unknown,"watch_error":root.watch_error}));
  std::fs::write(&args[3],serde_json::to_vec_pretty(&reports).unwrap()).unwrap();
  if depth==3{old=Some(tree.scan)}
 }
 std::fs::write(&args[3],serde_json::to_vec_pretty(&reports).unwrap()).unwrap();println!("{}",serde_json::to_string_pretty(&reports).unwrap());
}

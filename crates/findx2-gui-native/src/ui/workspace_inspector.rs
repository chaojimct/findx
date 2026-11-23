//! Inspector owns a bounded read-only view, not navigation or mouse selection.
use super::*;
use crate::usage_inspector::{self as model,ActionKey,Snapshot};
pub(in super::super) fn native_side_direction(button:MouseButton)->Option<bool>{match button{MouseButton::Navigate(NavigationDirection::Back)=>Some(false),MouseButton::Navigate(NavigationDirection::Forward)=>Some(true),_=>None}}
#[cfg(test)]mod event_tests{use super::{native_side_direction,MouseButton,NavigationDirection};#[test]fn actual_platform_navigation_buttons_map_once_and_never_accept_left_or_middle(){assert_eq!(native_side_direction(MouseButton::Navigate(NavigationDirection::Back)),Some(false));assert_eq!(native_side_direction(MouseButton::Navigate(NavigationDirection::Forward)),Some(true));assert_eq!(native_side_direction(MouseButton::Left),None);assert_eq!(native_side_direction(MouseButton::Middle),None);}}

impl MainWindow {
 pub(in super::super) fn dismiss_usage_facts(&mut self)->bool{let open=self.workspace.usage_facts_open;self.workspace.usage_facts_open=false;open}

 pub(super) fn inspector_source(&self)->Option<model::Source>{self.workspace.usage_scene.as_ref().map(|s|(s.key.scan,s.key.root))}
 pub(super) fn usage_inspector_key(&self)->Option<ActionKey>{self.workspace.inspector.displayed.as_ref().filter(|s|Some(s.source)==self.inspector_source()).map(|s|s.key())}
 pub(super) fn inspector_data(&self,selected:Option<usize>,root:Option<usize>)->Option<(Arc<Scan>,usize,usize)>{
  if let Some(s)=self.workspace.inspector.displayed.as_ref().filter(|s|Some(s.source)==self.inspector_source()){return Some((s.scan.clone(),s.selected,s.root))}
  Some((self.usage_render_scan()?.clone(),selected.or(root)?,root?))
 }
 pub(super) fn inspector_select(&mut self,index:usize,cx:&mut Context<Self>){
  if self.workspace.scanning{return}let Some(source)=self.inspector_source()else{return};let Some(scan)=self.usage_render_scan().cloned()else{return};
  if index>=scan.nodes.len()||source.1>=scan.nodes.len(){return}
  let path=&scan.nodes[index].path;
  if self.workspace.inspector.pending.as_ref().is_some_and(|p|p.path==*path&&(p.key.0,p.key.1)==source){return}
  if self.workspace.inspector.pending.is_none()&&self.workspace.inspector.displayed.as_ref().is_some_and(|s|s.source==source&&s.node().path==*path&&s.issue.is_none()){return}
  let seed=model::seed(&scan,index,source.1,source,self.workspace.engine_usage);self.inspector_request(seed,cx);
 }
 pub(super) fn inspector_default(&mut self,cx:&mut Context<Self>){if let Some(scene)=&self.workspace.usage_scene{let i=self.workspace.usage_scene_selected.unwrap_or(scene.key.root);self.inspector_select(i,cx);}}
 fn inspector_request(&mut self,seed:Snapshot,cx:&mut Context<Self>){
  let Some(request)=self.workspace.inspector.begin(seed.clone())else{cx.notify();return};
  if !self.workspace.engine_usage||!seed.node().directory{let mut ready=seed;ready.issue=None;self.workspace.inspector.finish(&request,ready);cx.notify();return}
  let cache=self.workspace.usage_page_cache.clone();let session=self.session.read(cx);let ipc=session.ipc.clone();let endpoint=session.endpoint.clone();let expected=seed.scan.logical_generation;
  cx.spawn(async move|this,cx|{
   if request.cancel.load(Ordering::Acquire){return}
   let path=display_path(&request.path);let cancel=request.cancel.clone();
   let result=crate::indexed_siblings::load(request.path.clone(),expected,cache,cancel.clone(),|after,stamp|{
    let ipc=ipc.clone();let endpoint=endpoint.clone();let path=path.clone();let cancel=cancel.clone();
    async move{match ipc.request_cancellable(endpoint,findx2_ipc::IpcRequest::UsageChildren{path,expected:stamp,after,max_bytes:48*1024},cancel).await.map_err(|_|"详情读取已取消".to_string())??{
     findx2_ipc::IpcResponse::UsageChildrenResult{page}=>Ok(page),findx2_ipc::IpcResponse::Error{message}=>Err(message),_=>Err("详情响应不兼容".into())
    }}
   }).await;
   this.update(cx,|this,cx|{
    if !this.workspace.inspector.current(&request){return}
    if this.workspace.mode!=ViewMode::Usage||this.inspector_source()!=Some(seed.source){this.workspace.inspector.cancel();return}
    let ready=match result{Ok(list)=>model::loaded(seed,list),Err(message)=>{let mut seed=seed;seed.issue=Some(format!("子项暂不可用：{message}"));seed.partial=true;seed}};
    if this.workspace.inspector.finish(&request,ready){cx.notify();}
   }).ok();
  }).detach();cx.notify();
 }
 pub(super) fn inspector_action(&mut self,key:Option<ActionKey>,action:u8,window:Option<&mut Window>,cx:&mut Context<Self>){
  if !self.usage_inspector_current(key){return}let node=self.workspace.inspector.displayed.as_ref().unwrap().node().clone();
  if self.workspace.usage_blocked_paths.contains(&super::key(&node.path)){return}
  match action{
   0=>if let Some(window)=window{let path=if node.directory{node.path}else{node.path.parent().unwrap_or(Path::new("C:\\")).to_path_buf()};self.begin_scoped_search(path,window,cx);},
   1=>{let parent=node.path.parent().unwrap_or(Path::new("C:\\")).to_path_buf();self.workspace_navigate(parent,ViewMode::Directory,Some(display_path(&node.path)),true,cx);},
   2=>if node.directory{self.workspace_navigate(node.path,ViewMode::Usage,None,true,cx);},
   _=>{}
  }
 }
 pub(super) fn inspector_pick(&mut self,key:Option<ActionKey>,index:usize,enter:bool,cx:&mut Context<Self>){
  if !self.usage_inspector_current(key){return}let current=self.workspace.inspector.displayed.as_ref().unwrap().clone();let Some(node)=current.scan.nodes.get(index).cloned()else{return};
  if self.workspace.usage_blocked_paths.contains(&super::key(&node.path)){return}
  if enter&&node.directory{self.workspace_navigate(node.path,ViewMode::Usage,None,true,cx);return}
  let in_scene=self.usage_render_scan().and_then(|s|s.nodes.iter().position(|n|super::key(&n.path)==super::key(&node.path)));
  if let Some(i)=in_scene{self.select_usage_node(i,false,cx);}else{
   self.workspace.usage_focus=Some(display_path(&node.path));self.workspace.inspector_pin=Some((current.source,node.clone()));
   let seed=model::seed(&current.scan,index,current.root,current.source,self.workspace.engine_usage);self.inspector_request(seed,cx);
  }
  if enter&&!node.directory{self.preview_force=Some(ForceState::Show);self.preview_active_selection(cx);}
 }
 pub(in super::super) fn workspace_native_side(&mut self,button:MouseButton,cx:&mut Context<Self>){if let Some(forward)=native_side_direction(button){self.workspace_side_button(forward,cx);cx.stop_propagation();}}
 pub(in super::super) fn workspace_side_button(&mut self,forward:bool,cx:&mut Context<Self>){
  if !model::side_down(&mut self.workspace.side_buttons,forward){return}
  if self.file_menu_is_open()||self.workspace.address_menu.is_some()||self.workspace.usage_facts_open{
   self.dismiss_file_menu(cx);self.dismiss_address_menu(cx);self.dismiss_usage_facts();cx.notify();return
  }
  if self.workspace.op_busy{return}self.cancel_path_edit(cx);self.workspace_history(forward,cx);
 }
 pub(in super::super) fn release_side_button(&mut self,forward:bool){self.workspace.side_buttons[usize::from(forward)]=false;}
 pub(in super::super) fn reset_side_buttons(&mut self){self.workspace.side_buttons=[false;2];}
}

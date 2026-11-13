#![cfg(windows)]
use findx2_gui_native::{ipc::IpcClient,indexed_usage::scan_from_page};
use findx2_ipc::{IpcRequest,IpcResponse,UsagePageDto};
use std::{path::PathBuf,process::{Command,Child,Stdio},time::{Instant,Duration},io::Write};
struct Fixture { child:Child, root:PathBuf, stop:PathBuf }
impl Drop for Fixture {
 fn drop(&mut self) {
  let _=std::fs::write(&self.stop,b"stop");
  for _ in 0..100 { if self.child.try_wait().ok().flatten().is_some() { let _=std::fs::remove_dir_all(&self.root); return; } std::thread::sleep(Duration::from_millis(20)); }
  let _=self.child.kill(); let _=self.child.wait(); let _=std::fs::remove_dir_all(&self.root);
 }
}
fn request(client:&IpcClient,endpoint:&str,req:IpcRequest)->IpcResponse {
 client.request(endpoint.into(),req).blocking_recv().unwrap().unwrap()
}
fn page(client:&IpcClient,endpoint:&str,path:&str)->UsagePageDto {
 match request(client,endpoint,IpcRequest::UsageChildren{path:path.into(),expected:None,after:None,max_bytes:4096}) {IpcResponse::UsageChildrenResult{page}=>page,other=>panic!("unexpected fixture response {other:?}")}
}
#[test]
#[ignore="explicit isolated service fixture; requires FINDX_T109_SERVICE_EXE"]
fn compact_service_native_client_render_model_and_delayed_metadata() {
 let exe=PathBuf::from(std::env::var_os("FINDX_T109_SERVICE_EXE").expect("explicit isolated exe required"));
 assert_eq!(exe.parent().unwrap().file_name().unwrap(),"native-usage-t109");
 let nonce=std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
 let root=std::env::temp_dir().join(format!("findx-t109-integration-{}-{nonce}",std::process::id()));
 let data=root.join("synthetic");std::fs::create_dir_all(data.join("nested")).unwrap();
 std::fs::write(data.join("a.bin"),vec![1;11]).unwrap();std::fs::write(data.join("nested/b.bin"),vec![2;22]).unwrap();
 let script=root.join("events.jsonl");std::fs::write(&script,"").unwrap();let stop=root.join("stop");
 let pipe=format!("findx-t109-integration-{}-{nonce}",std::process::id());let endpoint=format!(r"\\.\pipe\{pipe}");
 let child=Command::new(exe).args(["--no-watch","--no-backfill","--no-everything-ipc","--usage-memory-mib","32","--volume","F:","--pipe",&pipe,"--index"])
  .arg(root.join("synthetic.bin")).arg("--fixture-root").arg(&data).arg("--fixture-events").arg(&script).arg("--shutdown-file").arg(&stop)
  .stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
 let _fixture=Fixture{child,root,stop};let client=IpcClient::start();
 let indexed_path=data.to_string_lossy().into_owned();
 let start=Instant::now();
 loop {
  let response=client.request(endpoint.clone(),IpcRequest::UsageCapabilities).blocking_recv();
  if matches!(response,Ok(Ok(IpcResponse::UsageCapabilitiesResult{logical_tree:1,allocated:false}))) {break;}
  assert!(start.elapsed()<Duration::from_secs(15),"isolated fixture startup timed out");std::thread::sleep(Duration::from_millis(50));
 }
 let initial=page(&client,&endpoint,&indexed_path);assert_eq!(initial.root.logical,33);assert_eq!(initial.root.files,2);assert_eq!(initial.root.unknown,0);
 assert!(initial.watch_error.as_ref().unwrap().contains("no real USN/MFT"));
 let model=scan_from_page(&initial,data.clone());assert_eq!(model.nodes[0].logical,33);assert_eq!(model.nodes[0].allocated,0);assert!(model.partial);assert!(model.nodes.len()<=16);
 let mut events=std::fs::OpenOptions::new().append(true).open(&script).unwrap();
 writeln!(events,"{}",serde_json::json!({"op":"pending","file_id":777,"parent_id":1,"name":"delayed.bin"})).unwrap();events.flush().unwrap();
 let changed=Instant::now();let pending=loop {let p=page(&client,&endpoint,&indexed_path);if p.root.unknown==1{break p;}assert!(changed.elapsed()<Duration::from_secs(5));std::thread::sleep(Duration::from_millis(20));};
 let pending_ms=changed.elapsed().as_millis();
 assert_eq!(pending.root.logical,33);assert_eq!(pending.root.files,3);assert_ne!(pending.generation,initial.generation);
 let stale=request(&client,&endpoint,IpcRequest::UsageChildren{path:indexed_path.clone(),expected:Some((initial.epoch,initial.generation)),after:None,max_bytes:4096});assert!(matches!(stale,IpcResponse::Error{..}));
 writeln!(events,"{}",serde_json::json!({"op":"size","file_id":777,"size":44})).unwrap();events.flush().unwrap();
 let metadata=Instant::now();let final_page=loop {let p=page(&client,&endpoint,&indexed_path);if p.root.logical==77&&p.root.unknown==0{break p;}assert!(metadata.elapsed()<Duration::from_secs(5));std::thread::sleep(Duration::from_millis(20));};
 let model=scan_from_page(&final_page,data);assert_eq!(model.nodes[0].logical,77);assert!(model.nodes.iter().all(|n|n.allocated==0));assert_eq!(model.logical_generation,Some((final_page.epoch,final_page.generation)));
 println!("isolated real-pipe native-client render-model fixture passed; pending_visible_ms={} metadata_visible_ms={} bounded_model_nodes={}",pending_ms,metadata.elapsed().as_millis(),model.nodes.len());
}

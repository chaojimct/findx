use findx2_gui_native::{usage_volume,ipc::IpcClient};
use findx2_ipc::{IpcRequest,IpcResponse};
#[tokio::main] async fn main(){
 let a=std::env::args().collect::<Vec<_>>();assert_eq!(a.len(),3);let p=std::path::PathBuf::from(&a[1]);assert!(p.starts_with(r"C:\Users\chaoj"));
 let v=usage_volume::query(&p).unwrap();
 let response=IpcClient::start().request(r"\\.\pipe\findx2-t165-real".into(),IpcRequest::UsageChildren{path:p.to_string_lossy().into_owned(),after:None,expected:None,max_bytes:48*1024}).await.unwrap().unwrap();
 let IpcResponse::UsageChildrenResult{page}=response else{panic!("private UsageChildren unavailable")};
 let dev=page.children.iter().filter(|c|c.name=="dev").collect::<Vec<_>>();assert_eq!(dev.len(),1,"dev selection must be identity-unambiguous");
 let result=serde_json::json!({"path":p,"os":{"root":v.root,"total":v.total,"free":v.free,"available":v.available,"used":v.used()},"index":{"epoch":page.epoch,"generation":page.generation,"logical":page.root.logical,"unknown":page.root.unknown,"watch_error":page.watch_error,"state":page.state,"dev_id":dev[0].id,"dev_logical":dev[0].logical},"same_measurement_basis":false});
 std::fs::write(&a[2],serde_json::to_vec_pretty(&result).unwrap()).unwrap();println!("{}",result);
}

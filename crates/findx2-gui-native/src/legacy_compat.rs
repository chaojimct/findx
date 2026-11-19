//! Bounded data-only compatibility. Never open an index or change a legacy file.
use serde::{Deserialize,Serialize};
use serde_json::{Value,json};
use std::{collections::BTreeMap,path::{Path,PathBuf},io::Write};
#[derive(Debug,Clone,PartialEq,Serialize,Deserialize)]
#[serde(rename_all="camelCase",default)]
pub struct Resume {pub version:u32,pub tabs:Vec<String>,pub active:usize,pub directory:bool,#[serde(flatten)]pub extra:BTreeMap<String,Value>}
impl Default for Resume{fn default()->Self{Self{version:1,tabs:vec![],active:0,directory:false,extra:Default::default()}}}
impl Resume{
 pub fn validate(&self)->Result<(),String>{
  if self.version!=1||self.tabs.len()>12||(!self.tabs.is_empty()&&self.active>=self.tabs.len()){return Err("unsupported/invalid session; preserved, not replaced".into());}
  for p in &self.tabs{let b=p.as_bytes();let drive=b.len()>=3&&b[0].is_ascii_alphabetic()&&b[1]==b':'&&matches!(b[2],b'\\'|b'/');let unc=p.starts_with("\\\\")&&!p.starts_with("\\\\?\\")&&!p.starts_with("\\\\.\\");if p.len()>32768||p.contains('\0')||(!drive&&!unc){return Err("invalid session path; preserved".into());}}
  Ok(())
 }
}
/// Atomic no-clobber installation: even a concurrent native setting wins.
pub fn install_new(path:&Path,value:&Value)->Result<bool,String>{
 if path.exists(){return Ok(false);}let parent=path.parent().ok_or("missing destination parent")?;std::fs::create_dir_all(parent).map_err(|e|e.to_string())?;
 let tmp=parent.join(format!(".findx-import-{}-{}",std::process::id(),std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos()));
 let result=(||{let mut f=std::fs::OpenOptions::new().write(true).create_new(true).open(&tmp).map_err(|e|e.to_string())?;f.write_all(&serde_json::to_vec_pretty(value).map_err(|e|e.to_string())?).map_err(|e|e.to_string())?;f.sync_all().map_err(|e|e.to_string())?;drop(f);match std::fs::hard_link(&tmp,path){Ok(())=>Ok(true),Err(e)if e.kind()==std::io::ErrorKind::AlreadyExists=>Ok(false),Err(e)=>Err(e.to_string())}})();let _=std::fs::remove_file(tmp);result
}
pub fn import_settings(source:&Path,target:&Path,official:bool,pipe:&str,index:&str)->Result<bool,String>{
 if !official||target.exists()||!source.exists(){return Ok(false);}let m=std::fs::symlink_metadata(source).map_err(|e|e.to_string())?;if !m.is_file()||m.file_type().is_symlink()||m.len()>1024*1024{return Err("unsafe/oversize legacy settings; untouched".into());}
 let mut v:Value=serde_json::from_slice(&std::fs::read(source).map_err(|e|e.to_string())?).map_err(|e|e.to_string())?;let o=v.as_object_mut().ok_or("legacy settings must be an object")?;
 // Preserve unknown fields and the old source. Only connection boundaries are pinned.
 o.insert("pipeName".into(),json!(pipe));o.insert("indexPath".into(),json!(index));install_new(target,&v)
}
pub fn decode_string(bytes:&[u8])->Result<String,String>{match bytes.split_first(){Some((1,b))=>Ok(b.iter().map(|&v|char::from(v)).collect()),Some((0,b))if b.len()%2==0=>String::from_utf16(&b.chunks_exact(2).map(|x|u16::from_le_bytes([x[0],x[1]])).collect::<Vec<_>>()).map_err(|e|e.to_string()),_=>Err("unknown Chromium string format; source retained".into())}}
const KEYS:[&str;8]=["findx2_ui_theme","findx2_preview_open","findx2_sidebar_open","findx2_preview_width","findx2_col_fractions","findx2_search_history","findx2_last_update_check_ms","findx2_update_banner_dismissed_for_tag"];
/// Accept only the known Chromium schema/origin; no string scraping, tombstone
/// resurrection or opening the source DB. LevelDB recovers only a private copy.
pub fn webview_values(source:&Path)->Result<BTreeMap<String,String>,String>{
 use rusty_leveldb::{DB,Options};if !source.exists(){return Ok(Default::default());}
 let mut lock=std::fs::OpenOptions::new();lock.read(true);#[cfg(windows)]{use std::os::windows::fs::OpenOptionsExt;lock.share_mode(0);}
 let _closed_source=lock.open(source.join("LOCK")).map_err(|e|format!("legacy browser active/unreadable; retry without modifying it: {e}"))?;
 let temp=std::env::temp_dir().join(format!("FindX-legacy-copy-{}-{}",std::process::id(),std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos()));std::fs::create_dir(&temp).map_err(|e|e.to_string())?;
 let result=(||{
  let mut total=0u64;let mut count=0;for entry in std::fs::read_dir(source).map_err(|e|e.to_string())?{let e=entry.map_err(|e|e.to_string())?;if e.file_name()=="LOCK"{continue;}let m=e.metadata().map_err(|e|e.to_string())?;if !e.file_type().map_err(|e|e.to_string())?.is_file(){return Err("legacy DB contains nonregular object; untouched".into());}total=total.checked_add(m.len()).ok_or("legacy size overflow")?;count+=1;if count>256||total>64*1024*1024{return Err("legacy DB exceeds bounded import; untouched".into());}std::fs::copy(e.path(),temp.join(e.file_name())).map_err(|e|e.to_string())?;}
  let mut opt=Options::default();opt.create_if_missing=false;opt.paranoid_checks=true;opt.reuse_logs=false;opt.reuse_manifest=false;let mut db=DB::open(&temp,opt).map_err(|e|e.to_string())?;let snap=db.get_snapshot();
  if db.get_at(&snap,b"VERSION").map_err(|e|e.to_string())?.as_deref()!=Some(b"1"){return Err("unsupported WebView storage schema; original retained".into());}
  let mut out=BTreeMap::new();for key in KEYS{for format in [1u8,0]{let mut k=b"_http://tauri.localhost\0".to_vec();k.push(format);if format==1{k.extend_from_slice(key.as_bytes());}else{for c in key.encode_utf16(){k.extend_from_slice(&c.to_le_bytes());}}if let Some(v)=db.get_at(&snap,&k).map_err(|e|e.to_string())?{if v.len()>1024*1024{return Err("legacy value too large".into());}let decoded=decode_string(&v)?;if out.get(key).is_some_and(|old|old!=&decoded){return Err("conflicting legacy key encodings; retained".into());}out.insert(key.into(),decoded);}}}Ok(out)
 })();let _=std::fs::remove_dir_all(temp);result
}
pub fn map_prefs(values:&BTreeMap<String,String>)->Result<Value,String>{
 let mut out=serde_json::Map::new();if let Some(t)=values.get("findx2_ui_theme"){if !matches!(t.as_str(),"light"|"dark"|"system"){return Err("invalid legacy theme".into());}out.insert("theme".into(),json!(t));}
 for(old,new)in[("findx2_preview_open","previewOpen"),("findx2_sidebar_open","sidebarOpen")]{if let Some(v)=values.get(old){if !matches!(v.as_str(),"0"|"1"){return Err("invalid legacy boolean".into());}out.insert(new.into(),json!(v=="1"));}}
 if let Some(v)=values.get("findx2_preview_width"){let n:f64=v.parse().map_err(|_|"invalid legacy width")?;if !n.is_finite()||!(120.0..=4096.0).contains(&n){return Err("invalid legacy width".into());}out.insert("previewWidth".into(),json!(n));}
 if let Some(v)=values.get("findx2_last_update_check_ms"){let n:i64=v.parse().map_err(|_|"invalid legacy update time")?;if n<0{return Err("invalid legacy update time".into());}out.insert("lastOfficialUpdateCheckUnix".into(),json!(n/1000));}
 // Ratio/history are not pixel widths or filesystem recents. Keep exact legacy
 // values without inventing a conversion or changing the frozen native layout.
 out.insert("legacyWebView".into(),json!(values));Ok(Value::Object(out))
}
#[cfg(test)]mod tests{
 use super::*;
 fn dir(name:&str)->PathBuf{let p=std::env::temp_dir().join(format!("fx-compat-{name}-{}-{}",std::process::id(),std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));std::fs::create_dir(&p).unwrap();p}
 #[test]fn no_clobber_idempotent_preserves_unknown_source_and_pins_connection(){let d=dir("import");let old=d.join("old.json");let target=d.join("new.json");let b=br#"{"pipeName":"dev","indexPath":"old","future":{"x":1}}"#;std::fs::write(&old,b).unwrap();assert!(!import_settings(&old,&target,false,"findx2","official").unwrap());assert!(import_settings(&old,&target,true,"findx2","official").unwrap());let v:Value=serde_json::from_slice(&std::fs::read(&target).unwrap()).unwrap();assert_eq!(v["pipeName"],"findx2");assert_eq!(v["future"]["x"],1);assert!(!import_settings(&old,&target,true,"other","other").unwrap());assert_eq!(std::fs::read(&old).unwrap(),b);std::fs::remove_dir_all(d).unwrap();}
 #[test]fn corrupt_or_wrong_type_never_installs_or_mutates_source(){let d=dir("bad");let old=d.join("old");let target=d.join("new");for b in [b"broken".as_slice(),b"[]"]{std::fs::write(&old,b).unwrap();assert!(import_settings(&old,&target,true,"findx2","official").is_err());assert!(!target.exists());assert_eq!(std::fs::read(&old).unwrap(),b);}std::fs::remove_dir_all(d).unwrap();}
 #[test]fn chromium_types_units_and_raw_values_are_preserved(){assert_eq!(decode_string(&[1,233]).unwrap(),"é");assert_eq!(decode_string(&[0,0x2d,0x4e]).unwrap(),"中");assert!(decode_string(&[0,1]).is_err());assert!(decode_string(&[2,1]).is_err());let v=BTreeMap::from([("findx2_ui_theme".into(),"dark".into()),("findx2_preview_open".into(),"0".into()),("findx2_last_update_check_ms".into(),"1700000000123".into()),("findx2_search_history".into(),"[\"old query\"]".into())]);let m=map_prefs(&v).unwrap();assert_eq!(m["theme"],"dark");assert_eq!(m["previewOpen"],false);assert_eq!(m["lastOfficialUpdateCheckUnix"],1700000000i64);assert_eq!(m["legacyWebView"]["findx2_search_history"],"[\"old query\"]");}
 #[test]fn resume_roundtrip_keeps_unavailable_paths_and_unknown_fields(){let r=Resume{tabs:vec![r"Z:\missing".into(),r"\\server\share\offline".into()],active:1,directory:true,extra:BTreeMap::from([("future".into(),json!(123))]),..Default::default()};r.validate().unwrap();let q:Resume=serde_json::from_slice(&serde_json::to_vec(&r).unwrap()).unwrap();assert_eq!(r,q);let mut bad=q;bad.version=99;assert!(bad.validate().is_err());bad.version=1;bad.tabs[0]="C:".into();assert!(bad.validate().is_err());}
 #[test]fn actual_leveldb_copy_respects_tombstones_and_never_changes_source(){use rusty_leveldb::{DB,Options};let d=dir("ldb");let dbdir=d.join("db");let mut db=DB::open(&dbdir,Options::default()).unwrap();db.put(b"VERSION",b"1").unwrap();let key=b"_http://tauri.localhost\0\x01findx2_ui_theme";db.put(key,b"\x01dark").unwrap();db.flush().unwrap();drop(db);let before:Vec<_>=std::fs::read_dir(&dbdir).unwrap().map(|e|{let e=e.unwrap();(e.file_name(),std::fs::read(e.path()).unwrap())}).collect();assert_eq!(webview_values(&dbdir).unwrap()["findx2_ui_theme"],"dark");for(n,b)in before{assert_eq!(std::fs::read(dbdir.join(n)).unwrap(),b);}let mut db=DB::open(&dbdir,Options::default()).unwrap();db.delete(key).unwrap();db.flush().unwrap();drop(db);assert!(!webview_values(&dbdir).unwrap().contains_key("findx2_ui_theme"));std::fs::remove_dir_all(d).unwrap();}
 #[test]fn competing_new_destination_has_exactly_one_winner_and_source_failure_retries(){
  let d=dir("race");let target=d.join("new.json");let threads:Vec<_>=(0..6).map(|i|{let p=target.clone();std::thread::spawn(move||install_new(&p,&json!({"winner":i})).unwrap())}).collect();assert_eq!(threads.into_iter().map(|t|t.join().unwrap()).filter(|&v|v).count(),1);let before=std::fs::read(&target).unwrap();assert!(!install_new(&target,&json!({"winner":"replacement"})).unwrap());assert_eq!(std::fs::read(&target).unwrap(),before);
  let old=d.join("old.json");let retry=d.join("retry.json");std::fs::write(&old,b"bad").unwrap();assert!(import_settings(&old,&retry,true,"findx2","safe").is_err());assert!(!retry.exists());std::fs::write(&old,b"{}").unwrap();assert!(import_settings(&old,&retry,true,"findx2","safe").unwrap());std::fs::remove_dir_all(d).unwrap();
 }

}

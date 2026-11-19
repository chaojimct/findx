//! CI-only deliberate missing-directory COPY of an attested private VHD index.
//! Never edits a running index or changes its watermark/format; missing namespace
//! entries model the historical defect, not complete-volume acceptance.
#[cfg(windows)]fn main()->Result<(),Box<dyn std::error::Error>>{
 use std::path::Path;
 let a:Vec<_>=std::env::args().collect();if a.len()!=4{return Err("SOURCE_INDEX NEW_COPY OWNED_ROOT".into());}
 if std::env::var("GITHUB_ACTIONS").as_deref()!=Ok("true"){return Err("CI only".into());}
 let root=&a[3];if root.len()!=3||!b"ZYXWVUT".contains(&root.as_bytes()[0])||!root.ends_with(":\\"){return Err("attested private drive required".into());}
 if std::fs::read_to_string(format!("{root}.findx-identity-ci-owned"))?!=std::env::var("GITHUB_SHA")?{return Err("private-volume marker mismatch".into());}
 let temp=std::fs::canonicalize(std::env::var("RUNNER_TEMP")?)?;
 let source=std::fs::canonicalize(&a[1])?;let target=Path::new(&a[2]);let parent=std::fs::canonicalize(target.parent().ok_or("no copy parent")?)?;
 if !source.starts_with(&temp)||!parent.starts_with(&temp)||target.exists(){return Err("only a new disposable index copy".into());}
 let original=findx2_core::load_index_bin(&source)?;if original.volumes.len()!=1{return Err("one private volume only".into());}let v=&original.volumes[0];
 if v.volume_letter!=root.as_bytes()[0]||v.volume_serial!=findx2_windows::get_volume_serial_number(root)?{return Err("source/volume identity mismatch".into());}
 let(files,mut dirs)=findx2_windows::scan_volume_fast(root)?;let before=dirs.len();dirs.retain(|d|!matches!(d.name.as_str(),"lost-a"|"lost-b"|"lost-c"|"lost-d"));if before-dirs.len()!=4{return Err("did not omit exactly four intended parents".into());}
 let store=findx2_core::IndexBuilder::new(v.volume_letter,v.volume_serial,v.usn_journal_id,v.last_usn).build_from_raw(files,dirs,false)?;
 findx2_core::save_index_bin(target,&store)?;findx2_core::v6_save_full(&findx2_core::V6Layout::for_index(target),&store)?;
 println!("{}",serde_json::json!({"fixture":"missing-parent-index-copy","omitted":4,"watermark_changed":false,"metadata_ready":false,"journal":v.usn_journal_id,"last_usn":v.last_usn}));Ok(())
}
#[cfg(not(windows))]fn main(){panic!("Windows CI only")}

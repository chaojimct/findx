//! Exact-identity NTFS internal unlink translation. No path/name heuristics in event matching.
use findx2_core::ChangeEvent;
#[derive(Debug,Clone)]
pub enum NamespaceResolveError { Retry(String), Unsafe(String) }
impl From<String> for NamespaceResolveError{fn from(s:String)->Self{Self::Unsafe(s)}}
impl From<&str> for NamespaceResolveError{fn from(s:&str)->Self{Self::Unsafe(s.into())}}
impl std::fmt::Display for NamespaceResolveError{fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result{match self{Self::Retry(s)|Self::Unsafe(s)=>write!(f,"{s}")}}}
impl std::error::Error for NamespaceResolveError{}
impl NamespaceResolveError{pub fn retryable(&self)->bool{matches!(self,Self::Retry(_))}}
#[derive(Debug)] pub(crate) struct FileIdOpenError {pub win32:u32,pub native:u32}
impl FileIdOpenError {
 fn absence_candidate(&self)->bool{matches!(self.win32,2|3|87|1168)&&self.native!=0xc0000056}
 fn classification(&self)->&'static str{if self.native==0xc0000056{"delete-pending"}else if self.win32==5{"permission-denied"}else{"identity-open-unavailable"}}
}
#[link(name="ntdll")] unsafe extern "system" {fn RtlGetLastNtStatus()->i32;}
pub(crate) fn open_file_by_id_diagnostic(hint:windows::Win32::Foundation::HANDLE,id:u64)->Result<windows::Win32::Foundation::HANDLE,FileIdOpenError>{
 use windows::Win32::Storage::FileSystem::*;let mut d=FILE_ID_DESCRIPTOR::default();d.dwSize=std::mem::size_of::<FILE_ID_DESCRIPTOR>() as u32;d.Type=FileIdType;d.Anonymous.FileId=id as i64;
 match unsafe{OpenFileById(hint,&d,0,FILE_SHARE_READ|FILE_SHARE_WRITE|FILE_SHARE_DELETE,None,FILE_FLAG_BACKUP_SEMANTICS|FILE_FLAG_OPEN_REPARSE_POINT)}{Ok(h)=>Ok(h),Err(e)=>{let native=unsafe{RtlGetLastNtStatus()} as u32;Err(FileIdOpenError{win32:e.code().0 as u32&0xffff,native})}}
}
#[derive(Clone,Debug)]
pub struct OrdinaryLink { pub parent:u64, pub name:String }
#[derive(Clone,Copy,Debug)]
pub struct UnlinkBinding { pub volume:u8, pub serial:u32, pub journal:u64, pub parent:u64 }
/// Binding is constructed only after volume/journal/FRN and canonical metadata verification.
fn rewrite_bound_event(binding:UnlinkBinding,volume:u8,serial:u32,journal:u64,event:&ChangeEvent,ordinary:Option<&[OrdinaryLink]>) -> Result<Vec<ChangeEvent>,String> {
    if binding.volume!=volume || binding.serial!=serial || binding.journal!=journal {return Err("internal namespace binding epoch/volume mismatch; cursor retained".into());}
    let (id,parent,attrs,is_dir,extended)=match event {
        ChangeEvent::CreatePending{file_id,parent_id,attrs,is_dir,file_id_128,..}=>(*file_id,*parent_id,*attrs,*is_dir,*file_id_128),
        ChangeEvent::Rename{file_id,new_parent_id,..}=>(*file_id,*new_parent_id,0,false,None),
        _=>return Ok(vec![event.clone()]),
    };
    if parent!=binding.parent{return Ok(vec![event.clone()]);}
    if extended.is_some_and(|x|x[8..].iter().any(|b|*b!=0)){return Err("full 128-bit internal identity unresolved".into());}
    let links=ordinary.ok_or("ordinary hardlink enumeration unresolved; cursor retained")?;
    let mut result=vec![ChangeEvent::Delete{file_id:id}];
    if let Some(link)=links.iter().find(|x|x.parent!=binding.parent){
        result.clear();
        result.push(ChangeEvent::Rename{file_id:id,new_parent_id:link.parent,new_name:link.name.clone()});
    }
    Ok(result)
}

/// FILE_LINK_ENTRY_INFORMATION.FileNameLength counts WCHARs, not bytes.
/// Bounds use actual returned bytes and each record's next offset.
/// Microsoft ntifs documentation and a local public-hardlink query confirm
/// the units. Malformed/truncated enumeration always fails closed.
fn parse_hardlink_records(buffer:&[u8],internal_parent:u64)->Result<Vec<OrdinaryLink>,String>{
 if buffer.len()<8{return Err("incomplete hardlink header".into());}
 let needed=u32::from_le_bytes(buffer[..4].try_into().unwrap()) as usize;
 if needed>buffer.len(){return Err("incomplete hardlink result; cursor retained".into());}
 let count=u32::from_le_bytes(buffer[4..8].try_into().unwrap()) as usize;
 if count>1024{return Err("hardlink query exceeds 1024-link bound; cursor retained".into());}
 let mut offset=8usize;let mut links=Vec::new();
 for index in 0..count{
  let head_end=offset.checked_add(20).filter(|&n|n<=buffer.len()).ok_or("malformed hardlink record")?;
  let next=u32::from_le_bytes(buffer[offset..offset+4].try_into().unwrap()) as usize;
  let parent=u64::from_le_bytes(buffer[offset+8..offset+16].try_into().unwrap());
  let units=u32::from_le_bytes(buffer[offset+16..head_end].try_into().unwrap()) as usize;
  let bytes=units.checked_mul(2).ok_or("hardlink filename length overflow")?;
  let end=head_end.checked_add(bytes).ok_or("hardlink filename bounds overflow")?;
  let record_end=if next==0{buffer.len()}else{
   if next<20 || next%8!=0{return Err("invalid hardlink record offset".into());}
   offset.checked_add(next).filter(|&n|n<buffer.len()).ok_or("invalid hardlink record offset")?
  };
  if end>record_end || units==0{return Err("invalid hardlink name bounds".into());}
  if (index+1<count && next==0)||(index+1==count && next!=0){return Err("incomplete hardlink record chain; cursor retained".into());}
  if parent!=internal_parent{
   let wide:Vec<u16>=buffer[head_end..end].chunks_exact(2).map(|b|u16::from_le_bytes([b[0],b[1]])).collect();
   links.push(OrdinaryLink{parent,name:String::from_utf16_lossy(&wide)});
  }
  offset=record_end;
 }
 Ok(links)
}

#[cfg(windows)]
mod windows_resolver {
 use super::*;
 use std::mem::size_of;
 use windows::Win32::Foundation::{CloseHandle,HANDLE};
 use windows::Win32::Storage::FileSystem::{OpenFileById,GetFileInformationByHandle,GetFinalPathNameByHandleW,FILE_ID_DESCRIPTOR,FileIdType,FILE_SHARE_READ,FILE_SHARE_WRITE,FILE_SHARE_DELETE,FILE_FLAG_BACKUP_SEMANTICS,FILE_FLAG_OPEN_REPARSE_POINT,BY_HANDLE_FILE_INFORMATION,FILE_NAME_NORMALIZED};
 #[repr(C)] struct IoStatus { status:usize, information:usize }
 #[link(name="ntdll")]
 extern "system" { fn NtQueryInformationFile(handle:HANDLE,ios:*mut IoStatus,buffer:*mut u8,length:u32,class:u32)->i32; }
 pub struct NtfsUnlinkResolver { handle:HANDLE,volume:u8,serial:u32,journal:u64,epoch:crate::ntfs_identity::Epoch,bound:Option<UnlinkBinding>, last_metadata:Option<(u64,u64,u64,u32)> }
 impl Drop for NtfsUnlinkResolver {fn drop(&mut self){unsafe{let _=CloseHandle(self.handle);}}}
 impl NtfsUnlinkResolver {
  pub fn new(volume_path:&str,volume:u8,serial:u32,journal:u64)->Result<Self,NamespaceResolveError>{
   if crate::get_volume_serial_number(volume_path).map_err(|e|NamespaceResolveError::Retry(e.to_string()))?!=serial{return Err("namespace volume serial mismatch".into());}
   let handle=crate::usn::open_volume_handle(&crate::usn::volume_device_path(volume_path)).map_err(|e|NamespaceResolveError::Retry(e.to_string()))?;
   let checked=crate::ntfs_identity::epoch(handle).and_then(|epoch|if epoch.journal!=journal||epoch.volume.serial as u32!=serial{Err("namespace volume/journal identity mismatch".into())}else{Ok(epoch)});
   let epoch=match checked{Ok(e)=>e,Err(e)=>{unsafe{let _=CloseHandle(handle);}return Err(e)}};
   Ok(Self{handle,volume,serial,journal,epoch,bound:None,last_metadata:None})
  }
  fn open_id(&self,id:u64)->Result<HANDLE,FileIdOpenError>{super::open_file_by_id_diagnostic(self.handle,id)}
  fn bind_candidate(&mut self,parent:u64)->Result<(),NamespaceResolveError>{
   let cached=self.bound.is_some_and(|b|b.parent==parent);
   let handle=match self.open_id(parent){Ok(h)=>h,Err(e)=>{
    if !cached && e.absence_candidate() && crate::ntfs_identity::absent(self.handle,parent,self.epoch)? {return Ok(());}
    return Err(NamespaceResolveError::Retry(format!("parent unavailable: {} win32={} ntstatus={:#x}; exact prefix retained",e.classification(),e.win32,e.native)));
   }};
   let result=(||->Result<_,NamespaceResolveError>{
    let mut info=BY_HANDLE_FILE_INFORMATION::default();unsafe{GetFileInformationByHandle(handle,&mut info)}.map_err(|e|NamespaceResolveError::Retry(e.to_string()))?;
    if info.dwVolumeSerialNumber!=self.serial{return Err("parent volume serial mismatch".into());}
    if ((info.nFileIndexHigh as u64)<<32|info.nFileIndexLow as u64)!=parent{return Err("parent FRN sequence verification mismatch".into());}
    let mut path=vec![0u16;32768];let n=unsafe{GetFinalPathNameByHandleW(handle,&mut path,FILE_NAME_NORMALIZED)} as usize;
    if n==0 || n>=path.len(){return Err(NamespaceResolveError::Retry("parent canonical identity unavailable".into()));}
    let canonical=String::from_utf16_lossy(&path[..n]);let expected=format!(r"\\?\{}:\$Extend\$Deleted",self.volume as char);
    if info.dwFileAttributes&0x16==0x16 && canonical.eq_ignore_ascii_case(&expected){
     self.bound=Some(UnlinkBinding{volume:self.volume,serial:self.serial,journal:self.journal,parent});
     tracing::info!("verified NTFS internal unlink parent binding (numeric evidence remains local)");
    } else if cached {return Err("cached internal parent canonical identity changed".into());}
    Ok(())
   })();unsafe{let _=CloseHandle(handle);}result
  }
  fn ordinary_links(&mut self,id:u64,binding:UnlinkBinding)->Result<Vec<OrdinaryLink>,NamespaceResolveError>{
   self.last_metadata=None;
   let handle=match self.open_id(id){
    Ok(h)=>h,
    Err(e) if e.absence_candidate()=>{
     if crate::ntfs_identity::absent(self.handle,id,self.epoch)? {return Ok(Vec::new());}
     return Err(NamespaceResolveError::Retry("identity remains live; no deletion proof, batch retained".into()));
    },
    Err(e)=>return Err(NamespaceResolveError::Retry(format!("{} win32={} ntstatus={:#x}; batch retained",e.classification(),e.win32,e.native))),
   };
   let result=(||->Result<_,NamespaceResolveError>{
    let mut info=BY_HANDLE_FILE_INFORMATION::default();unsafe{GetFileInformationByHandle(handle,&mut info)}.map_err(|e|NamespaceResolveError::Retry(e.to_string()))?;
    if ((info.nFileIndexHigh as u64)<<32|info.nFileIndexLow as u64)!=id{return Err("child FRN sequence mismatch".into());}
    if info.dwVolumeSerialNumber!=self.serial{return Err("hardlink volume serial mismatch".into());}
    self.last_metadata=Some((((info.nFileSizeHigh as u64)<<32)|info.nFileSizeLow as u64,((info.ftLastWriteTime.dwHighDateTime as u64)<<32)|info.ftLastWriteTime.dwLowDateTime as u64,((info.ftCreationTime.dwHighDateTime as u64)<<32)|info.ftCreationTime.dwLowDateTime as u64,info.dwFileAttributes));
    // The count is not used as a count of ordinary names: enumerate actual links.
    let mut buffer=vec![0u8;65536];let mut ios=IoStatus{status:0,information:0};
    let status=unsafe{NtQueryInformationFile(handle,&mut ios,buffer.as_mut_ptr(),buffer.len() as u32,46)};
    if status<0{return Err(NamespaceResolveError::Retry(format!("bounded hardlink query unresolved status={status:#x}; cursor retained")));}
    if ios.information<8 || ios.information>buffer.len(){return Err("incomplete hardlink query result; cursor retained".into());}
    let needed=u32::from_le_bytes(buffer[0..4].try_into().unwrap()) as usize;
    if needed>buffer.len(){return Err("hardlink query buffer insufficient; cursor retained".into());}
    let count=u32::from_le_bytes(buffer[4..8].try_into().unwrap()) as usize;
    if count==0 && info.nNumberOfLinks>0{return Err("positive link count but no link records; cursor retained".into());}
    if count>1024{return Err("hardlink query exceeds 1024-link bound; cursor retained".into());}
    parse_hardlink_records(&buffer[..ios.information],binding.parent).map_err(Into::into)
   })();unsafe{let _=CloseHandle(handle);}result
  }
  pub fn rewrite(&mut self,events:&[ChangeEvent],pending:&[ChangeEvent],missing_parents:&[u64],journal:u64,known_parent:impl Fn(u64)->bool,known_identity:impl Fn(u64)->bool)->Result<Vec<ChangeEvent>,NamespaceResolveError>{
   if journal!=self.journal{return Err("unlink resolver journal changed; cursor retained".into());}
   crate::ntfs_identity::check(self.handle,self.epoch)?;
   if let Some(binding)=self.bound {self.bind_candidate(binding.parent)?;}
   for parent in missing_parents.iter().take(64){self.bind_candidate(*parent)?;if self.bound.is_some(){break;}}
   let Some(binding)=self.bound else{crate::ntfs_identity::check(self.handle,self.epoch)?;return Ok(events.to_vec());};
   let mut result=Vec::new();
   for (index,ev) in events.iter().chain(pending.iter()).enumerate(){
    let parent=match ev{ChangeEvent::CreatePending{parent_id,..}=>Some(*parent_id),ChangeEvent::Rename{new_parent_id,..}=>Some(*new_parent_id),_=>None};
    if parent==Some(binding.parent){
     let id=match ev{ChangeEvent::CreatePending{file_id,..}|ChangeEvent::Rename{file_id,..}=>*file_id,_=>unreachable!()};
     let mut links=self.ordinary_links(id,binding)?;
     if !links.is_empty(){let chosen=links.iter().position(|x|known_parent(x.parent)).ok_or("surviving ordinary alias parent outside resolvable indexed scope; cursor retained")?;links.swap(0,chosen);}
     let rewritten=rewrite_bound_event(binding,self.volume,self.serial,journal,ev,Some(&links))?;
     for placed in rewritten {
      if let ChangeEvent::Rename{file_id,new_parent_id,new_name}=&placed {
       if !known_identity(*file_id){
        let (size,mtime,ctime,attrs)=self.last_metadata.ok_or("new ordinary alias lacks metadata; cursor retained")?;
        result.push(ChangeEvent::Create{entry:findx2_core::RawEntry{file_id:*file_id,file_id_128:None,parent_id:*new_parent_id,name:new_name.clone(),size,mtime,ctime,attrs,is_dir:attrs&16!=0}});continue;
       }
      }
      result.push(placed);
     }
    }else if index<events.len(){result.push(ev.clone());}
   }
   crate::ntfs_identity::check(self.handle,self.epoch)?;
   Ok(result)
  }
 }

 #[cfg(test)]mod namespace_native_owned_tests{
  use super::*;
  use std::{fs::{File,OpenOptions},os::windows::{fs::OpenOptionsExt,io::AsRawHandle}};
  fn fh(f:&File)->HANDLE{HANDLE(f.as_raw_handle() as _)}
  fn identity(f:&File)->(u64,u32){let mut i=BY_HANDLE_FILE_INFORMATION::default();unsafe{GetFileInformationByHandle(fh(f),&mut i)}.unwrap();(((i.nFileIndexHigh as u64)<<32)|i.nFileIndexLow as u64,i.dwVolumeSerialNumber)}
  fn eventually_deleted(resolver:&mut NtfsUnlinkResolver,id:u64,binding:UnlinkBinding){
   let start=std::time::Instant::now();loop{match resolver.ordinary_links(id,binding){Ok(links) if links.is_empty()=>return,Err(e) if !e.retryable()=>panic!("wrong deletion proof stage: {e}"),_=>{}}assert!(start.elapsed()<std::time::Duration::from_secs(5),"actual deleted identity never obtained corroborated absence");std::thread::sleep(std::time::Duration::from_millis(25));}
  }
  #[test]#[ignore="Only attested disposable Windows CI VHD; never host volume"]
  fn namespace_native_owned_actual_absence_pending_alias_and_epoch(){
   assert_eq!(std::env::var("GITHUB_ACTIONS").unwrap(),"true");
   let root=std::env::var("FINDX_IDENTITY_OWNED_ROOT").unwrap();assert!(root.len()==3&&b"ZYXWVUT".contains(&root.as_bytes()[0])&&root.ends_with(":\\"));
   let marker=std::fs::read_to_string(format!("{root}.findx-identity-ci-owned")).unwrap();assert_eq!(marker,std::env::var("GITHUB_SHA").unwrap());
   let d=std::path::PathBuf::from(&root).join("owned-identity");std::fs::create_dir(&d).unwrap();let path=d.join("live.txt");std::fs::write(&path,b"public owned identity").unwrap();
   let held=OpenOptions::new().read(true).share_mode(7).open(&path).unwrap();let(id,serial)=identity(&held);
   let h=crate::usn::open_volume_handle(&crate::usn::volume_device_path(&root)).unwrap();let epoch=crate::ntfs_identity::epoch(h).unwrap();assert_eq!(epoch.volume.serial as u32,serial);
   let mut resolver=NtfsUnlinkResolver::new(&root,root.as_bytes()[0],serial,epoch.journal).unwrap();let binding=UnlinkBinding{volume:root.as_bytes()[0],serial,journal:epoch.journal,parent:u64::MAX};
   assert!(!crate::ntfs_identity::absent(h,id,epoch).unwrap());assert!(!resolver.ordinary_links(id,binding).unwrap().is_empty());
   let old=((((id>>48) as u16).wrapping_add(1).max(1) as u64)<<48)|(id&0x0000_ffff_ffff_ffff);
   assert!(super::super::open_file_by_id_diagnostic(h,old).is_err());assert!(crate::ntfs_identity::absent(h,old,epoch).unwrap());
   let alias=d.join("survivor.txt");std::fs::hard_link(&path,&alias).unwrap();drop(held);std::fs::remove_file(&path).unwrap();
   assert!(resolver.ordinary_links(id,binding).unwrap().iter().any(|l|l.name=="survivor.txt"));assert_eq!(std::fs::read(&alias).unwrap(),b"public owned identity");
   std::fs::remove_file(&alias).unwrap();eventually_deleted(&mut resolver,id,binding);
   let p=d.join("pending.txt");std::fs::write(&p,b"pending public").unwrap();let read=OpenOptions::new().read(true).share_mode(7).open(&p).unwrap();let(pid,_)=identity(&read);
   let deleting=OpenOptions::new().access_mode(0x10000|0x80).share_mode(7).open(&p).unwrap();let disposition=windows::Win32::Storage::FileSystem::FILE_DISPOSITION_INFO{DeleteFile:true};
   unsafe{windows::Win32::Storage::FileSystem::SetFileInformationByHandle(fh(&deleting),windows::Win32::Storage::FileSystem::FileDispositionInfo,(&disposition as *const windows::Win32::Storage::FileSystem::FILE_DISPOSITION_INFO).cast(),std::mem::size_of_val(&disposition) as u32)}.unwrap();drop(deleting);
   let failure=super::super::open_file_by_id_diagnostic(h,pid).unwrap_err();assert_eq!(failure.native,0xc0000056);assert_eq!(failure.classification(),"delete-pending");assert!(!failure.absence_candidate());let error=resolver.ordinary_links(pid,binding).unwrap_err();assert!(error.retryable()&&error.to_string().contains("delete-pending"));
   drop(read);eventually_deleted(&mut resolver,pid,binding);
   assert!(crate::ntfs_identity::check(h,crate::ntfs_identity::Epoch{journal:epoch.journal.wrapping_add(1),..epoch}).is_err());
   unsafe{let _=CloseHandle(h);}drop(resolver);std::fs::remove_dir_all(d).unwrap();
   println!("NAMESPACE_NATIVE_OWNED_PASS live_not_absent=true noncurrent_sequence_proven=true surviving_alias_preserved=true deleted_proven=true real_delete_pending_retained=true repaired_retry=true wrong_epoch_refused=true no_host_volume=true");
  }
 }
}
#[cfg(windows)]pub use windows_resolver::NtfsUnlinkResolver;
#[cfg(test)]
mod tests {
 use super::*;
 fn event()->ChangeEvent {ChangeEvent::CreatePending{file_id:42,file_id_128:None,parent_id:900,name:"internal-temporary".into(),attrs:0,is_dir:false}}
 fn binding()->UnlinkBinding {UnlinkBinding{volume:b'C',serial:1,journal:7,parent:900}}
 #[test]
 fn internal_final_unlink_is_not_a_missing_parent_create() {
  let events=rewrite_bound_event(binding(),b'C',1,7,&event(),Some(&[])).unwrap();
  assert!(matches!(events.as_slice(),[ChangeEvent::Delete{file_id:42}]),"last-link unlink must remove user namespace, not park an internal create");
 }
 #[test]
 fn internal_unlink_with_surviving_hardlink_preserves_identity() {
  let links=[OrdinaryLink{parent:2,name:"surviving.bin".into()}];
  let events=rewrite_bound_event(binding(),b'C',1,7,&event(),Some(&links)).unwrap();
  assert!(matches!(events.as_slice(),[ChangeEvent::Rename{file_id:42,new_parent_id:2,..}]),"atomic resolved rename must preserve ordinary alias metadata and supersede parked internal event");
 }
}

#[cfg(test)]mod boundary_tests {
 use super::*;
 fn b()->UnlinkBinding{UnlinkBinding{volume:b'C',serial:1,journal:7,parent:900}}
 fn ev(parent:u64)->ChangeEvent{ChangeEvent::CreatePending{file_id:42,file_id_128:None,parent_id:parent,name:"same-name".into(),attrs:0,is_dir:false}}
 #[test]fn different_volume_same_frn_rejected(){assert!(rewrite_bound_event(b(),b'D',1,7,&ev(900),Some(&[])).is_err());}
 #[test]fn different_serial_or_journal_rejected(){assert!(rewrite_bound_event(b(),b'C',2,7,&ev(900),Some(&[])).is_err());assert!(rewrite_bound_event(b(),b'C',1,8,&ev(900),Some(&[])).is_err());}
 #[test]fn same_name_ordinary_parent_not_filtered(){let e=ev(901);assert!(matches!(rewrite_bound_event(b(),b'C',1,7,&e,Some(&[])).unwrap().as_slice(),[ChangeEvent::CreatePending{parent_id:901,..}]));}
 #[test]fn different_parent_sequence_not_filtered(){let e=ev(900+(1u64<<48));assert!(matches!(rewrite_bound_event(b(),b'C',1,7,&e,Some(&[])).unwrap().as_slice(),[ChangeEvent::CreatePending{..}]));}
 #[test]fn unresolved_links_fail_closed(){assert!(rewrite_bound_event(b(),b'C',1,7,&ev(900),None).is_err());}
 #[test]fn several_links_preserve_exact_case(){let links=[OrdinaryLink{parent:900,name:"internal".into()},OrdinaryLink{parent:2,name:"KeepCASE.bin".into()},OrdinaryLink{parent:3,name:"other.bin".into()}];let v=rewrite_bound_event(b(),b'C',1,7,&ev(900),Some(&links)).unwrap();assert!(matches!(&v[0],ChangeEvent::Rename{new_name,new_parent_id:2,..} if new_name=="KeepCASE.bin"));}
}

#[cfg(test)]mod hardlink_buffer_tests{
 use super::*;
 fn encoded(names:&[(u64,&str)])->Vec<u8>{
  let mut b=vec![0u8;8];b[4..8].copy_from_slice(&(names.len() as u32).to_le_bytes());
  for (i,(parent,name)) in names.iter().enumerate(){
   let start=b.len();let wide:Vec<_>=name.encode_utf16().collect();let n=(20+wide.len()*2+7)&!7;b.resize(start+n,0);
   if i+1<names.len(){b[start..start+4].copy_from_slice(&(n as u32).to_le_bytes());}
   b[start+8..start+16].copy_from_slice(&parent.to_le_bytes());b[start+16..start+20].copy_from_slice(&(wide.len() as u32).to_le_bytes());
   for (j,c) in wide.iter().enumerate(){b[start+20+j*2..start+22+j*2].copy_from_slice(&c.to_le_bytes());}
  }
  let len=b.len() as u32;b[..4].copy_from_slice(&len.to_le_bytes());b
 }
 #[test]fn odd_even_and_surrogate_names_are_full_wchar_sequences(){
  let unicode=String::from_utf16(&[0x4e2d,0x6587,0xd83d,0xde00,0x2e,0x78]).unwrap();
  let names=[(2,"odd.txt"),(3,"evenfile.txt"),(4,unicode.as_str())];let b=encoded(&names);let links=parse_hardlink_records(&b,900).unwrap();
  for (l,(parent,name)) in links.iter().zip(names){assert_eq!(l.parent,parent);assert_eq!(l.name,name);}
 }
 #[test]fn excludes_only_verified_internal_parent(){let b=encoded(&[(900,"internal"),(2,"KeepCASE.bin")]);let l=parse_hardlink_records(&b,900).unwrap();assert_eq!(l.len(),1);assert_eq!(l[0].name,"KeepCASE.bin");}
 #[test]fn truncated_names_do_not_become_final_unlinks(){let mut b=encoded(&[(2,"odd.txt")]);b.truncate(30);assert!(parse_hardlink_records(&b,900).is_err());}
 #[test]fn cross_record_name_length_is_rejected(){let mut b=encoded(&[(2,"odd.txt"),(3,"next")]);b[24..28].copy_from_slice(&1000u32.to_le_bytes());assert!(parse_hardlink_records(&b,900).is_err());}
 #[test]fn broken_chain_and_excessive_count_fail_closed(){let mut b=encoded(&[(2,"one"),(3,"two")]);b[8..12].copy_from_slice(&0u32.to_le_bytes());assert!(parse_hardlink_records(&b,900).is_err());let mut b=encoded(&[(2,"one")]);b[4..8].copy_from_slice(&1025u32.to_le_bytes());assert!(parse_hardlink_records(&b,900).is_err());}
}

#[cfg(test)] mod ntfs_identity_error_tests {
 use super::*;
 #[test]fn ntfs_identity_error_codes_alone_never_prove_absence(){for win32 in [2,3,87,1168,5,32,33,6]{let e=FileIdOpenError{win32,native:0};assert_eq!(e.absence_candidate(),matches!(win32,2|3|87|1168));}}
 #[test]fn ntfs_identity_delete_pending_and_permission_are_distinct_retained_failures(){let pending=FileIdOpenError{win32:5,native:0xc0000056};assert_eq!(pending.classification(),"delete-pending");assert!(!pending.absence_candidate());assert_eq!(FileIdOpenError{win32:5,native:0xc0000022}.classification(),"permission-denied");}
}

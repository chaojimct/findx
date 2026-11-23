//! Exact-identity NTFS internal unlink translation. No path/name heuristics in event matching.
use findx2_core::ChangeEvent;
#[derive(Clone,Debug)]
pub struct OrdinaryLink { pub parent:u64, pub name:String }
#[derive(Clone,Copy,Debug)]
pub struct UnlinkBinding { pub volume:u8, pub serial:u32, pub journal:u64, pub parent:u64 }
/// A live file observation is admitted only after native full-FRN, volume and
/// journal checks. It is not inferred from an open error or an old pathname.
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
struct NamespaceScope { volume:u8, serial:u32, journal:u64 }
#[derive(Clone,Debug)]
struct LiveFileObservation { scope:NamespaceScope, id:u64, metadata:(u64,u64,u64,u32), links:Vec<OrdinaryLink> }
fn live_pending_placement(event:&ChangeEvent, expected:NamespaceScope, observed:&LiveFileObservation, known_parent:impl Fn(u64)->bool, known_identity:bool)->Result<Option<ChangeEvent>,String>{
 let ChangeEvent::CreatePending{file_id,file_id_128,is_dir:false,..}=event else{return Ok(None)};
 if file_id_128.is_some_and(|id|id[8..].iter().any(|&b|b!=0)){return Err("full 128-bit parked identity unresolved; cursor retained".into());}
 if observed.scope!=expected || observed.id!=*file_id{return Err("parked live identity/scope mismatch; cursor retained".into());}
 let (size,mtime,ctime,attrs)=observed.metadata;
 if attrs&16!=0{return Err("parked file changed type; cursor retained".into());}
 // No known current link means remain parked, not absent/deleted. The current
 // directory may be created by a later journal batch; never invent its lineage.
 let Some(link)=observed.links.iter().find(|link|known_parent(link.parent))else{return Ok(None)};
 if known_identity {Ok(Some(ChangeEvent::Rename{file_id:*file_id,new_parent_id:link.parent,new_name:link.name.clone()}))}
 else {Ok(Some(ChangeEvent::Create{entry:findx2_core::RawEntry{file_id:*file_id,file_id_128:*file_id_128,parent_id:link.parent,name:link.name.clone(),size,mtime,ctime,attrs,is_dir:false}}))}
}
fn reconcile_live_pending(pending:&[ChangeEvent],expected:NamespaceScope,internal_parent:Option<u64>,mut observe:impl FnMut(u64)->Result<LiveFileObservation,String>,known_parent:impl Fn(u64)->bool,known_identity:impl Fn(u64)->bool)->Result<Vec<ChangeEvent>,String>{
 let mut seen=Vec::new();let mut result=Vec::new();seen.try_reserve(pending.len().min(64)).map_err(|_|"parked recovery allocation failed; cursor retained")?;
 for event in pending.iter().take(64){
  let ChangeEvent::CreatePending{file_id,parent_id,is_dir:false,..}=event else{continue};
  if internal_parent==Some(*parent_id)||seen.contains(file_id){continue}seen.push(*file_id);
  let observation=match observe(*file_id){Ok(value)=>value,Err(error)=>{tracing::debug!(file_id,error=%error,"live parked observation unavailable; original record retained");continue;}};
  if let Some(placement)=live_pending_placement(event,expected,&observation,&known_parent,known_identity(*file_id))?{result.try_reserve(1).map_err(|_|"parked recovery output allocation failed; cursor retained")?;result.push(placement);}
 }
 Ok(result)
}
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

/// A complete live-link observation with no indexed parent is a dependency,
/// not a fatal identity error. Keep the original internal event in the bounded
/// orphan transaction until a later journal parent makes a real link resolvable.
/// None never means success/absence and must never acknowledge a cursor.
fn place_bound_links(binding:UnlinkBinding,volume:u8,serial:u32,journal:u64,event:&ChangeEvent,links:&mut [OrdinaryLink],known_parent:impl Fn(u64)->bool)->Result<Option<Vec<ChangeEvent>>,String>{
 if binding.volume!=volume || binding.serial!=serial || binding.journal!=journal{return Err("internal namespace binding epoch/volume mismatch; cursor retained".into());}
 if !links.is_empty(){
  let Some(chosen)=links.iter().position(|link|known_parent(link.parent))else{return Ok(None)};
  links.swap(0,chosen);
 }
 rewrite_bound_event(binding,volume,serial,journal,event,Some(links)).map(Some)
}

/// Only used after the exact internal parent and volume/journal binding are
/// verified. Access denied is NOT evidence of an absent/deleted identity.
fn bound_identity_open_failure(id:u64, code:u32, native:Option<u32>, detail:&str)->Result<Vec<OrdinaryLink>,String> {
    // Last-NTSTATUS is diagnostic only: even the retry classification NEVER
    // proves absence, deletes an identity, or acknowledges a journal cursor.
    if code==5 && native==Some(0xc0000056) {
        return Err(format!("bound unlink identity temporarily delete-pending: FRN={id:#x}; {detail}; original batch retained"));
    }
    match code {
        2|3|1168 => Ok(Vec::new()),
        _ => Err(format!("bound unlink identity metadata unavailable: OpenFileById FRN={id:#x}, win32={code}: {detail}; identity absence not proven; cursor retained")),
    }
}
/// Narrow retry marker produced only by the verified internal-parent resolver.
pub fn is_retryable_namespace_error(error:&str)->bool {
    error.starts_with("bound unlink identity temporarily delete-pending:")
}
#[cfg(test)]
mod denied_identity_tests {
    use super::*;
    #[test] fn observed_denial_never_becomes_an_unlink_or_success() {
        let e=bound_identity_open_failure(42,5,None,"拒绝访问。 (0x80070005)").unwrap_err();
        assert!(e.contains("win32=5"));assert!(e.contains("FRN=0x2a"));
        assert!(e.contains("identity absence not proven"));assert!(e.contains("cursor retained"));
        assert!(rewrite_bound_event(UnlinkBinding{volume:b'C',serial:1,journal:2,parent:900},b'C',1,2,&ChangeEvent::Rename{file_id:42,new_parent_id:900,new_name:"internal".into()},None).is_err());
    }
    #[test] fn delete_pending_is_retry_only_never_absence() {
        let error=bound_identity_open_failure(42,5,Some(0xc0000056),"owned native fixture").unwrap_err();
        assert!(is_retryable_namespace_error(&error));
        for native in [None,Some(0xc0000022),Some(0),Some(0xc000000d)] {
            let error=bound_identity_open_failure(42,5,native,"denied").unwrap_err();
            assert!(!is_retryable_namespace_error(&error));
        }
        assert!(bound_identity_open_failure(42,87,Some(0xc000000d),"invalid parameter is not absence proof").is_err());
    }
    #[test] fn unknown_and_sharing_errors_also_retain_cursor() {
        for code in [5,32,33,87,995,1234] {assert!(bound_identity_open_failure(42,code,None,"fixture").is_err());}
        for code in [2,3,1168] {assert!(bound_identity_open_failure(42,code,None,"fixture").unwrap().is_empty());}
    }
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
 #[derive(Debug)]
 pub struct FileIdOpenError {pub win32:u32,pub last_ntstatus:u32}
 impl FileIdOpenError {pub fn is_transient_delete_pending(&self)->bool{self.win32==5 && self.last_ntstatus==0xc0000056}}
 #[link(name="ntdll")]
 unsafe extern "system" {fn RtlGetLastNtStatus()->i32;}
 /// Capture the native thread status immediately after the Win32 open fails,
 /// before formatting/logging can make unrelated native calls. Diagnostic only:
 /// no error is reclassified as absence and no privileges are changed.
 pub fn open_file_by_id_diagnostic(hint:HANDLE,id:u64)->Result<HANDLE,FileIdOpenError> {
   let mut d=FILE_ID_DESCRIPTOR::default();d.dwSize=size_of::<FILE_ID_DESCRIPTOR>() as u32;d.Type=FileIdType;d.Anonymous.FileId=id as i64;
   match unsafe{OpenFileById(hint,&d,0,FILE_SHARE_READ|FILE_SHARE_WRITE|FILE_SHARE_DELETE,None,FILE_FLAG_BACKUP_SEMANTICS|FILE_FLAG_OPEN_REPARSE_POINT)} {
     Ok(h)=>Ok(h),Err(e)=>{let native=unsafe{RtlGetLastNtStatus()} as u32;Err(FileIdOpenError{win32:e.code().0 as u32 & 0xffff,last_ntstatus:native})}
   }
 }
 #[cfg(test)]
 pub fn inspect_identity_for_test(volume:HANDLE,id:u64)->Result<String,String>{
  let h=open_file_by_id_diagnostic(volume,id).map_err(|e|format!("open failed win32={} ntstatus={:#010x}",e.win32,e.last_ntstatus))?;
  let result=(||{let mut info=BY_HANDLE_FILE_INFORMATION::default();unsafe{GetFileInformationByHandle(h,&mut info)}.map_err(|e|e.to_string())?;
   let actual=(info.nFileIndexHigh as u64)<<32|info.nFileIndexLow as u64;if actual!=id{return Err("full identity mismatch".into());}
   let mut path=vec![0u16;32768];let n=unsafe{GetFinalPathNameByHandleW(h,&mut path,FILE_NAME_NORMALIZED)}as usize;let name=if n>0 && n<path.len(){String::from_utf16_lossy(&path[..n])}else{"<path unavailable>".into()};
   let mut buffer=vec![0u8;65536];let mut ios=IoStatus{status:0,information:0};let status=unsafe{NtQueryInformationFile(h,&mut ios,buffer.as_mut_ptr(),buffer.len()as u32,46)};
   let links=if status>=0 && ios.information<=buffer.len(){format!("{:?}",parse_hardlink_records(&buffer[..ios.information],0))}else{format!("hardlink metadata status={status:#x}")};
   Ok(format!("id={id:#x} serial={} attrs={:#x} path={name:?} links={links}",info.dwVolumeSerialNumber,info.dwFileAttributes))
  })();unsafe{let _=CloseHandle(h);}result
 }
 pub struct NtfsUnlinkResolver { handle:HANDLE,volume:u8,serial:u32,journal:u64,mft_volume:crate::mft_identity::VolumeData,bound:Option<UnlinkBinding>, last_metadata:Option<(u64,u64,u64,u32)> }
 impl Drop for NtfsUnlinkResolver {fn drop(&mut self){unsafe{let _=CloseHandle(self.handle);}}}
 impl NtfsUnlinkResolver {
  pub fn new(volume_path:&str,volume:u8,serial:u32,journal:u64)->Result<Self,String>{
   if crate::get_volume_serial_number(volume_path).map_err(|e|e.to_string())?!=serial{return Err("namespace volume serial mismatch".into());}
   let handle=crate::usn::open_volume_handle(&crate::usn::volume_device_path(volume_path)).map_err(|e|e.to_string())?;
   let setup=(||{
     let actual=unsafe{crate::usn::query_usn_journal(handle)}.map_err(|e|e.to_string())?;
     if actual.UsnJournalID!=journal{return Err("namespace journal mismatch".into());}
     let data=crate::mft_identity::native::volume(handle)?;
     if data.serial as u32!=serial{return Err("namespace NTFS serial mismatch".into());}
     Ok(data)
   })();
   let mft_volume=match setup{Ok(data)=>data,Err(error)=>{unsafe{let _=CloseHandle(handle);}return Err(error);}};
   Ok(Self{handle,volume,serial,journal,mft_volume,bound:None,last_metadata:None})
  }
  fn open_id(&self,id:u64)->Result<HANDLE,FileIdOpenError>{open_file_by_id_diagnostic(self.handle,id)}

  fn identity_absent(&self,id:u64,binding:UnlinkBinding)->Result<bool,String>{
   if binding.volume!=self.volume || binding.serial!=self.serial || binding.journal!=self.journal{return Err("MFT proof binding mismatch".into());}
   let before=unsafe{crate::usn::query_usn_journal(self.handle)}.map_err(|e|e.to_string())?;
   if before.UsnJournalID!=self.journal{return Err("MFT proof journal epoch changed".into());}
   let answer=crate::mft_identity::native::absent(self.handle,id,self.mft_volume,self.journal)?;
   let after=unsafe{crate::usn::query_usn_journal(self.handle)}.map_err(|e|e.to_string())?;
   if after.UsnJournalID!=self.journal{return Err("MFT proof journal changed during query".into());}Ok(answer)
  }
  fn bind_candidate(&mut self,parent:u64)->Result<(),String>{
   if self.bound.is_some(){return Ok(());}
   let handle=match self.open_id(parent){Ok(h)=>h,Err(_)=>return Ok(())};
   let result=(||{
    let mut info=BY_HANDLE_FILE_INFORMATION::default();unsafe{GetFileInformationByHandle(handle,&mut info)}.map_err(|e|e.to_string())?;
    if info.dwVolumeSerialNumber!=self.serial{return Err("parent volume serial mismatch".into());}
    if ((info.nFileIndexHigh as u64)<<32|info.nFileIndexLow as u64)!=parent{return Err("parent FRN sequence verification mismatch".into());}
    let mut path=vec![0u16;32768];let n=unsafe{GetFinalPathNameByHandleW(handle,&mut path,FILE_NAME_NORMALIZED)} as usize;
    if n==0 || n>=path.len(){return Ok(());}
    let canonical=String::from_utf16_lossy(&path[..n]);let expected=format!(r"\\?\{}:\$Extend\$Deleted",self.volume as char);
    if info.dwFileAttributes&0x16==0x16 && canonical.eq_ignore_ascii_case(&expected){
     self.bound=Some(UnlinkBinding{volume:self.volume,serial:self.serial,journal:self.journal,parent});
     tracing::info!("verified NTFS internal unlink parent binding (numeric evidence remains local)");
    }
    Ok(())
   })();unsafe{let _=CloseHandle(handle);}result
  }
  fn ordinary_links(&mut self,id:u64,binding:UnlinkBinding)->Result<Vec<OrdinaryLink>,String>{
   self.last_metadata=None;
   let handle=match self.open_id(id){
    Ok(h)=>h,
    Err(e)=>{
      if e.win32==87 && self.identity_absent(id,binding)? {return Ok(Vec::new());}
      return bound_identity_open_failure(id,e.win32,Some(e.last_ntstatus),&format!("native_thread_ntstatus={:#010x}; volume={}; serial={}; journal={}; bound_parent={:#x}",e.last_ntstatus,self.volume as char,self.serial,self.journal,binding.parent));
    },
   };
   self.read_open_links(id,handle,binding.parent)
  }
  fn read_open_links(&mut self,id:u64,handle:HANDLE,internal_parent:u64)->Result<Vec<OrdinaryLink>,String>{
   let result=(||{
    let mut info=BY_HANDLE_FILE_INFORMATION::default();unsafe{GetFileInformationByHandle(handle,&mut info)}.map_err(|e|e.to_string())?;
    if ((info.nFileIndexHigh as u64)<<32|info.nFileIndexLow as u64)!=id{return Err("child FRN sequence mismatch".into());}
    if info.dwVolumeSerialNumber!=self.serial{return Err("hardlink volume serial mismatch".into());}
    self.last_metadata=Some((((info.nFileSizeHigh as u64)<<32)|info.nFileSizeLow as u64,((info.ftLastWriteTime.dwHighDateTime as u64)<<32)|info.ftLastWriteTime.dwLowDateTime as u64,((info.ftCreationTime.dwHighDateTime as u64)<<32)|info.ftCreationTime.dwLowDateTime as u64,info.dwFileAttributes));
    // The count is not used as a count of ordinary names: enumerate actual links.
    let mut buffer=vec![0u8;65536];let mut ios=IoStatus{status:0,information:0};
    let status=unsafe{NtQueryInformationFile(handle,&mut ios,buffer.as_mut_ptr(),buffer.len() as u32,46)};
    if status<0{return Err(format!("bounded hardlink query unresolved status={status:#x}; cursor retained"));}
    if ios.information<8 || ios.information>buffer.len(){return Err("incomplete hardlink query result; cursor retained".into());}
    let needed=u32::from_le_bytes(buffer[0..4].try_into().unwrap()) as usize;
    if needed>buffer.len(){return Err("hardlink query buffer insufficient; cursor retained".into());}
    let count=u32::from_le_bytes(buffer[4..8].try_into().unwrap()) as usize;
    if count==0 && info.nNumberOfLinks>0{return Err("positive link count but no link records; cursor retained".into());}
    if count>1024{return Err("hardlink query exceeds 1024-link bound; cursor retained".into());}
    parse_hardlink_records(&buffer[..ios.information],internal_parent)
   })();unsafe{let _=CloseHandle(handle);}result
  }
  fn live_observation(&mut self,id:u64)->Result<LiveFileObservation,String>{
   let before=unsafe{crate::usn::query_usn_journal(self.handle)}.map_err(|e|e.to_string())?;
   if before.UsnJournalID!=self.journal{return Err("parked observation journal changed; cursor retained".into());}
   self.last_metadata=None;
   let h=self.open_id(id).map_err(|e|format!("parked identity unavailable win32={} ntstatus={:#x}; record retained",e.win32,e.last_ntstatus))?;
   let links=self.read_open_links(id,h,self.bound.map(|b|b.parent).unwrap_or(0))?;
   let metadata=self.last_metadata.ok_or("parked observation metadata unavailable; cursor retained")?;
   let after=unsafe{crate::usn::query_usn_journal(self.handle)}.map_err(|e|e.to_string())?;
   if after.UsnJournalID!=self.journal{return Err("parked observation journal changed during query; cursor retained".into());}
   Ok(LiveFileObservation{scope:NamespaceScope{volume:self.volume,serial:self.serial,journal:after.UsnJournalID},id,metadata,links})
  }
  pub fn rewrite(&mut self,events:&[ChangeEvent],pending:&[ChangeEvent],missing_parents:&[u64],journal:u64,known_parent:impl Fn(u64)->bool,known_identity:impl Fn(u64)->bool)->Result<Vec<ChangeEvent>,String>{
   if journal!=self.journal{return Err("unlink resolver journal changed; cursor retained".into());}
   for parent in missing_parents.iter().take(64){self.bind_candidate(*parent)?;if self.bound.is_some(){break;}}
   let binding=self.bound;
   let mut result=Vec::new();
   for (index,ev) in events.iter().chain(pending.iter()).enumerate(){
    let parent=match ev{ChangeEvent::CreatePending{parent_id,..}=>Some(*parent_id),ChangeEvent::Rename{new_parent_id,..}=>Some(*new_parent_id),_=>None};
    if let Some(binding)=binding.filter(|b|parent==Some(b.parent)){
     let id=match ev{ChangeEvent::CreatePending{file_id,..}|ChangeEvent::Rename{file_id,..}=>*file_id,_=>unreachable!()};
     let mut links=self.ordinary_links(id,binding).map_err(|e|format!("{e}; source_event={ev:?}"))?;
     let Some(rewritten)=place_bound_links(binding,self.volume,self.serial,journal,ev,&mut links,&known_parent)? else {
      // New input must reach the orphan queue; an already parked input is
      // carried by the core transaction itself, so do not duplicate it.
      // Keep original exact identity/internal placement, never synthesize a
      // guessed ancestor or delete a file whose ordinary aliases are live.
      tracing::debug!(file_id=id,?links,"live ordinary parent dependency retained for later journal placement");
      if index<events.len(){result.push(ev.clone());}
      continue;
     };
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
   let expected=NamespaceScope{volume:self.volume,serial:self.serial,journal:self.journal};
   result.extend(reconcile_live_pending(pending,expected,binding.map(|b|b.parent),|id|self.live_observation(id),&known_parent,&known_identity)?);
   Ok(result)
  }
 }
}
#[cfg(all(windows,test))]pub(crate) use windows_resolver::inspect_identity_for_test;
#[cfg(windows)]pub use windows_resolver::{NtfsUnlinkResolver,FileIdOpenError,open_file_by_id_diagnostic};
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

#[cfg(test)]
mod live_pending_tests {
 use super::*;
 use findx2_core::{IndexBuilder,RawEntry,SearchEngine};
 fn scope()->NamespaceScope{NamespaceScope{volume:b'C',serial:123,journal:7}}
 fn id(n:u64)->u64{(7u64<<48)|(1000+n)}
 fn pending(n:u64,parent:u64)->ChangeEvent{ChangeEvent::CreatePending{file_id:id(n),file_id_128:None,parent_id:parent,name:format!("old-staging-{n}"),attrs:0,is_dir:false}}
 fn observed(n:u64)->LiveFileObservation{LiveFileObservation{scope:scope(),id:id(n),metadata:(97,10,11,32),links:vec![OrdinaryLink{parent:99999,name:"unresolved-alias".into()},OrdinaryLink{parent:1,name:format!("current-{n}-你好.exe")} ]}}
 fn row(id:u64,parent:u64,name:String,size:u64,dir:bool)->RawEntry{RawEntry{file_id:id,file_id_128:None,parent_id:parent,name,size,mtime:1,ctime:1,attrs:if dir{16}else{32},is_dir:dir}}
 fn engine()->SearchEngine{let mut files=(0..3).map(|n|row(id(n),1,format!("old-{n}"),40+n,false)).collect::<Vec<_>>();files.push(row((8u64<<48)|1000,1,"new-sequence-must-survive".into(),88,false));SearchEngine::new(IndexBuilder::new(b'C',123,7,100).build_from_raw(files,vec![row(1,0,"known".into(),0,true)],true).unwrap())}
 #[test]fn thirteen_positions_eleven_live_identities_rebind_atomically_and_roundtrip(){
  let e=engine();let mut original=(0..11).map(|n|pending(n,900+n)).collect::<Vec<_>>();original.push(pending(2,1902));original.push(pending(3,1903));
  e.apply_volume_batch(b'C',&original,7,200).unwrap();assert!(e.has_pending_namespace(b'C'));assert_eq!(e.index_store().volumes[0].last_usn,100);
  let mut calls=0;let placements=reconcile_live_pending(&original,scope(),None,|full|{calls+=1;Ok(observed((full&0x0000_ffff_ffff_ffff)-1000))},|p|p==1,|full|e.namespace_identity_live(b'C',full)).unwrap();
  assert_eq!(calls,11);assert_eq!(placements.len(),11);assert!(!placements.iter().any(|ev|matches!(ev,ChangeEvent::Delete{..})));
  e.apply_volume_batch(b'C',&placements,7,300).unwrap();assert!(!e.has_pending_namespace(b'C'));
  let g=e.index_store();assert_eq!(g.volumes[0].last_usn,300);
  for n in 0..11{let entry=&g.entries[g.entry_on_volume(b'C',id(n)).unwrap()as usize];assert!(!entry.is_deleted());assert_eq!(entry.size,if n<3{40+n}else{97});}
  let new=&g.entries[g.entry_on_volume(b'C',(8u64<<48)|1000).unwrap()as usize];assert!(!new.is_deleted());assert_eq!(new.size,88);
  let dir=std::env::temp_dir().join(format!("findx-live-rebind-{}-{}",std::process::id(),std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));std::fs::create_dir(&dir).unwrap();let layout=findx2_core::V6Layout::for_index(&dir.join("index.bin"));findx2_core::v6_save_full(&layout,&g).unwrap();drop(g);let loaded=findx2_core::v6_load(&layout).unwrap();assert_eq!(loaded.volumes[0].last_usn,300);for n in 0..11{assert!(!loaded.entries[loaded.entry_on_volume(b'C',id(n)).unwrap()as usize].is_deleted());}std::fs::remove_dir_all(dir).unwrap();
 }
 #[test]fn live_unknown_or_unresolvable_links_never_become_delete(){let event=pending(0,900);let mut o=observed(0);o.links.clear();assert!(live_pending_placement(&event,scope(),&o,|_|true,false).unwrap().is_none());o=observed(0);assert!(live_pending_placement(&event,scope(),&o,|_|false,false).unwrap().is_none());assert!(matches!(live_pending_placement(&event,scope(),&o,|p|p==1,true).unwrap(),Some(ChangeEvent::Rename{new_parent_id:1,..})));}
 #[test]fn full_identity_volume_serial_and_journal_mismatches_fail_closed(){let event=pending(0,900);for bad in [NamespaceScope{volume:b'D',..scope()},NamespaceScope{serial:124,..scope()},NamespaceScope{journal:8,..scope()}]{let mut o=observed(0);o.scope=bad;assert!(live_pending_placement(&event,scope(),&o,|_|true,true).is_err());}let mut o=observed(0);o.id+=1u64<<48;assert!(live_pending_placement(&event,scope(),&o,|_|true,true).is_err());o=observed(0);o.metadata.3|=16;assert!(live_pending_placement(&event,scope(),&o,|_|true,true).is_err());}
 #[test]fn arbitrary_observation_failure_keeps_pending_and_durable_cursor(){let e=engine();let original=vec![pending(0,900)];e.apply_volume_batch(b'C',&original,7,200).unwrap();let placements=reconcile_live_pending(&original,scope(),None,|_|Err("access denied5".into()),|_|true,|_|true).unwrap();assert!(placements.is_empty());e.apply_volume_batch(b'C',&placements,7,300).unwrap();assert!(e.has_pending_namespace(b'C'));assert_eq!(e.index_store().volumes[0].last_usn,100);}
 #[test]fn work_is_bounded_and_internal_or_directory_positions_not_reinterpreted(){let mut pending=(0..100).map(|n|pending(n,900+n)).collect::<Vec<_>>();let mut calls=0;let result=reconcile_live_pending(&pending,scope(),None,|full|{calls+=1;Ok(observed((full&0x0000_ffff_ffff_ffff)-1000))},|p|p==1,|_|false).unwrap();assert_eq!(calls,64);assert_eq!(result.len(),64);pending.truncate(1);let mut calls=0;assert!(reconcile_live_pending(&pending,scope(),Some(900),|_|{calls+=1;Ok(observed(0))},|_|true,|_|true).unwrap().is_empty());assert_eq!(calls,0);if let ChangeEvent::CreatePending{is_dir,..}=&mut pending[0]{*is_dir=true};assert!(reconcile_live_pending(&pending,scope(),None,|_|panic!("directory recovery is not this file-only rule"),|_|true,|_|true).unwrap().is_empty());}
 #[test]fn high_128_bits_are_not_aliased_to_low64(){let mut event=pending(0,900);if let ChangeEvent::CreatePending{file_id_128,..}=&mut event{let mut extended=[0u8;16];extended[..8].copy_from_slice(&id(0).to_le_bytes());extended[8]=1;*file_id_128=Some(extended);}assert!(live_pending_placement(&event,scope(),&observed(0),|_|true,false).is_err());}
}

#[cfg(test)]mod deferred_internal_alias_tests{
 use super::*;
 fn binding()->UnlinkBinding{UnlinkBinding{volume:b'C',serial:1,journal:7,parent:900}}
 fn event()->ChangeEvent{ChangeEvent::CreatePending{file_id:42,file_id_128:None,parent_id:900,name:"internal".into(),attrs:0,is_dir:false}}
 #[test]fn live_unknown_parent_defers_instead_of_unlink_or_fatal_fence(){
  let mut links=vec![OrdinaryLink{parent:77,name:"live.txt".into()}];
  assert!(place_bound_links(binding(),b'C',1,7,&event(),&mut links,|_|false).unwrap().is_none());
  assert_eq!(links[0].parent,77);
  let recovered=place_bound_links(binding(),b'C',1,7,&event(),&mut links,|p|p==77).unwrap().unwrap();
  assert!(matches!(recovered.as_slice(),[ChangeEvent::Rename{file_id:42,new_parent_id:77,..}]));
  assert!(place_bound_links(binding(),b'D',1,7,&event(),&mut links,|_|false).is_err());
 }
 #[test]fn unknown_first_link_never_hides_later_known_alias(){
  let mut links=vec![OrdinaryLink{parent:66,name:"unknown".into()},OrdinaryLink{parent:77,name:"live".into()}];
  let out=place_bound_links(binding(),b'C',1,7,&event(),&mut links,|p|p==77).unwrap().unwrap();
  assert!(matches!(out.as_slice(),[ChangeEvent::Rename{new_parent_id:77,new_name,..}]if new_name=="live"));
 }
 #[test]fn delayed_parent_replay_keeps_cursor_and_live_metadata_until_exact_attachment(){
  use findx2_core::{IndexBuilder,RawEntry,SearchEngine};
  let raw=|id,parent,name:&str,dir|RawEntry{file_id:id,file_id_128:None,parent_id:parent,name:name.into(),size:if dir{0}else{123},mtime:0,ctime:0,attrs:if dir{16}else{0},is_dir:dir};
  let index=IndexBuilder::new(b'C',1,7,10).build_from_raw(vec![raw(42,1,"before.txt",false)],vec![raw(1,0,"",true)],true).unwrap();
  let engine=SearchEngine::new(index);engine.enable_usage(4*1024*1024).unwrap();
  let mut links=vec![OrdinaryLink{parent:77,name:"after.txt".into()}];
  for usn in [20,30,40]{
   let result=place_bound_links(binding(),b'C',1,7,&event(),&mut links,|_|false).unwrap();assert!(result.is_none());
   // Only the initial source event is submitted. Idle retries append nothing:
   // core carries its bounded orphan, no duplicate/replaced identity.
   let batch=if usn==20{vec![event()]}else{vec![]};engine.apply_volume_batch(b'C',&batch,7,usn).unwrap();
   assert!(engine.has_pending_namespace(b'C'));assert_eq!(engine.index_store().volumes[0].last_usn,10);
   assert_eq!(engine.usage_page("C:\\",None,None,4096).unwrap().root.logical,123);
  }
  let mut events=vec![ChangeEvent::Create{entry:raw(77,1,"later-parent",true)}];
  events.extend(place_bound_links(binding(),b'C',1,7,&event(),&mut links,|p|p==77).unwrap().unwrap());
  engine.apply_volume_batch(b'C',&events,7,50).unwrap();assert!(!engine.has_pending_namespace(b'C'));
  assert_eq!(engine.index_store().volumes[0].last_usn,50);
  let page=engine.usage_page("C:\\later-parent",None,None,4096).unwrap();assert_eq!(page.root.logical,123);assert_eq!(page.root.files,1);
 }
}

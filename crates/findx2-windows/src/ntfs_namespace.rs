//! Exact-identity NTFS internal unlink translation. No path/name heuristics in event matching.
use findx2_core::ChangeEvent;
#[derive(Debug,Clone)]
pub enum NamespaceResolveError { Retry(String), Unsafe(String), EpochChanged(String) }
impl From<String> for NamespaceResolveError{fn from(s:String)->Self{Self::Unsafe(s)}}
impl From<&str> for NamespaceResolveError{fn from(s:&str)->Self{Self::Unsafe(s.into())}}
impl std::fmt::Display for NamespaceResolveError{fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result{match self{Self::Retry(s)|Self::Unsafe(s)|Self::EpochChanged(s)=>write!(f,"{s}")}}}
impl std::error::Error for NamespaceResolveError{}
impl NamespaceResolveError{pub fn retryable(&self)->bool{matches!(self,Self::Retry(_))}pub fn requires_replay(&self)->bool{matches!(self,Self::EpochChanged(_))}}
#[derive(Debug)] pub(crate) struct FileIdOpenError {pub win32:u32,pub native:u32}
impl FileIdOpenError {
 fn absence_candidate(&self)->bool{matches!(self.win32,2|3|87|1168)&&self.native!=0xc0000056}
 fn classification(&self)->&'static str{if self.native==0xc0000056{"delete-pending"}else if self.win32==5{"permission-denied"}else{"identity-open-unavailable"}}
}
#[cfg(windows)]
#[link(name="ntdll")] unsafe extern "system" {fn RtlGetLastNtStatus()->i32;}
#[cfg(windows)]
pub(crate) fn open_file_by_id_diagnostic(hint:windows::Win32::Foundation::HANDLE,id:u64)->Result<windows::Win32::Foundation::HANDLE,FileIdOpenError>{
 // DesiredAccess 0 is STATUS_INVALID_PARAMETER (0xC000000D / win32 87) on real NTFS.
 open_file_by_id_access(hint,id,windows::Win32::Storage::FileSystem::FILE_READ_ATTRIBUTES.0)
}
#[cfg(windows)]
fn open_file_by_id_access(hint:windows::Win32::Foundation::HANDLE,id:u64,access:u32)->Result<windows::Win32::Foundation::HANDLE,FileIdOpenError>{
 use windows::Win32::Storage::FileSystem::*;let mut d=FILE_ID_DESCRIPTOR::default();d.dwSize=std::mem::size_of::<FILE_ID_DESCRIPTOR>() as u32;d.Type=FileIdType;d.Anonymous.FileId=id as i64;
 match unsafe{OpenFileById(hint,&d,access,FILE_SHARE_READ|FILE_SHARE_WRITE|FILE_SHARE_DELETE,None,FILE_FLAG_BACKUP_SEMANTICS|FILE_FLAG_OPEN_REPARSE_POINT)}{Ok(h)=>Ok(h),Err(e)=>{let native=unsafe{RtlGetLastNtStatus()} as u32;Err(FileIdOpenError{win32:e.code().0 as u32&0xffff,native})}}
}
#[derive(Clone,Debug)]
pub struct OrdinaryLink { pub parent:u64, pub name:String }
#[derive(Clone,Copy,Debug)]
pub struct UnlinkBinding { pub volume:u8, pub serial:u32, pub journal:u64, pub parent:u64 }
fn log_basename(name:&str)->String{
 let base=name.rsplit(['\\','/']).next().unwrap_or(name);
 let base=base.rsplit(':').next().unwrap_or(base);
 base.chars().take(80).map(|ch| if ch.is_control(){'?'}else{ch}).collect()
}
fn event_parts(ev:&ChangeEvent)->(&'static str,u64,u64,&str){
 match ev{
  ChangeEvent::Create{entry}=>("create",entry.file_id,entry.parent_id,entry.name.as_str()),
  ChangeEvent::CreatePending{file_id,parent_id,name,..}=>("create_pending",*file_id,*parent_id,name.as_str()),
  ChangeEvent::Rename{file_id,new_parent_id,new_name}=>("rename",*file_id,*new_parent_id,new_name.as_str()),
  ChangeEvent::Delete{file_id}=>("delete",*file_id,0,""),
  ChangeEvent::DataOrMeta{file_id,create_if_missing,..}=>("metadata",*file_id,create_if_missing.as_ref().map(|c|c.parent_id).unwrap_or(0),create_if_missing.as_ref().map(|c|c.name.as_str()).unwrap_or("")),
 }
}
fn failure_context(role:&str,frn:u64,explicit:Option<&ChangeEvent>,events:&[ChangeEvent],pending:&[ChangeEvent])->String{
 let ev=explicit.or_else(||events.iter().chain(pending.iter()).find(|ev|{let(_,file,parent,_)=event_parts(ev);file==frn||parent==frn}));
 match ev{
  Some(ev)=>{let(kind,file,parent,name)=event_parts(ev);format!(" role={role} frn={frn:#x} event={kind} file={file:#x} parent={parent:#x} name={}",log_basename(name))},
  None=>format!(" role={role} frn={frn:#x} event=none file=0 parent=0 name="),
 }
}
fn annotate(err:NamespaceResolveError,role:&str,frn:u64,explicit:Option<&ChangeEvent>,events:&[ChangeEvent],pending:&[ChangeEvent])->NamespaceResolveError{
 let ctx=failure_context(role,frn,explicit,events,pending);
 match err{NamespaceResolveError::Retry(s)=>NamespaceResolveError::Retry(format!("{s};{ctx}")),NamespaceResolveError::Unsafe(s)=>NamespaceResolveError::Unsafe(format!("{s};{ctx}")),NamespaceResolveError::EpochChanged(s)=>NamespaceResolveError::EpochChanged(format!("{s};{ctx}"))}
}

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

fn map_event_parent(ev: &ChangeEvent, map: &std::collections::HashMap<u64, u64>) -> ChangeEvent {
    let m = |id: u64| map.get(&id).copied().unwrap_or(id);
    match ev {
        ChangeEvent::CreatePending { file_id, file_id_128, parent_id, name, attrs, is_dir } => ChangeEvent::CreatePending {
            file_id: *file_id, file_id_128: *file_id_128, parent_id: m(*parent_id), name: name.clone(), attrs: *attrs, is_dir: *is_dir,
        },
        ChangeEvent::Rename { file_id, new_parent_id, new_name } => ChangeEvent::Rename {
            file_id: *file_id, new_parent_id: m(*new_parent_id), new_name: new_name.clone(),
        },
        ChangeEvent::Create { entry } => {
            let mut entry = entry.clone();
            entry.parent_id = m(entry.parent_id);
            ChangeEvent::Create { entry }
        }
        ChangeEvent::DataOrMeta { file_id, size, mtime, ctime, create_if_missing } => {
            let mut create_if_missing = create_if_missing.clone();
            if let Some(info) = create_if_missing.as_mut() { info.parent_id = m(info.parent_id); }
            ChangeEvent::DataOrMeta { file_id: *file_id, size: *size, mtime: *mtime, ctime: *ctime, create_if_missing }
        }
        other => other.clone(),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct JournalDirectory<'a> {
    pub parent: u64,
    pub name: &'a str,
    pub attrs: u32,
}

/// A directory create already present in this batch. Its name and parent came
/// from the journal, so resolving it does not require opening the object.
/// A non-directory, or a 128-bit identity, is not usable here.
pub(crate) fn journal_directory<'a>(events: &'a [ChangeEvent], pending: &'a [ChangeEvent], id: u64) -> Option<JournalDirectory<'a>> {
    events.iter().chain(pending.iter()).find_map(|ev| match ev {
        ChangeEvent::CreatePending { file_id, file_id_128, parent_id, name, attrs, is_dir: true } if *file_id == id => {
            if file_id_128.is_some_and(|raw| raw[8..].iter().any(|b| *b != 0)) { None }
            else { Some(JournalDirectory { parent: *parent_id, name: name.as_str(), attrs: *attrs }) }
        }
        ChangeEvent::Create { entry } if entry.is_dir && entry.file_id == id => {
            if entry.file_id_128.is_some_and(|raw| raw[8..].iter().any(|b| *b != 0)) { None }
            else { Some(JournalDirectory { parent: entry.parent_id, name: entry.name.as_str(), attrs: entry.attrs }) }
        }
        _ => None,
    })
}

pub(crate) fn parent_probe_required(already_resolved: bool) -> bool { !already_resolved }

pub(crate) fn prefix_defines_dir(prefix: &[ChangeEvent], id: u64) -> bool {
    prefix.iter().any(|ev| match ev {
        ChangeEvent::Create { entry } => entry.is_dir && entry.file_id == id,
        ChangeEvent::CreatePending { file_id, is_dir: true, .. } => *file_id == id,
        _ => false,
    })
}

fn retarget_file_id(ev: &ChangeEvent, id: u64) -> ChangeEvent {
    match ev {
        ChangeEvent::CreatePending { file_id, file_id_128, parent_id, name, attrs, is_dir } => ChangeEvent::CreatePending {
            file_id: id,
            file_id_128: if id == *file_id { *file_id_128 } else { None },
            parent_id: *parent_id, name: name.clone(), attrs: *attrs, is_dir: *is_dir,
        },
        ChangeEvent::Rename { new_parent_id, new_name, .. } => ChangeEvent::Rename {
            file_id: id, new_parent_id: *new_parent_id, new_name: new_name.clone(),
        },
        other => other.clone(),
    }
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
  fn open_id(&self,id:u64)->Result<HANDLE,super::FileIdOpenError>{super::open_file_by_id_diagnostic(self.handle,id)}
  /// `Ok(None)` when the id opens or the failure is not an in-use extension.
  /// `Ok(Some)` is a corroborated base. Errors retain the cursor and are not absence.
  fn extension_after_open_failure(&mut self,id:u64)->Result<Option<crate::ntfs_witness::ExtensionTarget>,NamespaceResolveError>{
   match self.open_id(id){
    Ok(h)=>{unsafe{let _=CloseHandle(h);}Ok(None)}
    Err(e) if e.absence_candidate()=>crate::ntfs_identity::extension_target(self.handle,id,self.epoch),
    Err(_)=>Ok(None),
   }
  }

  fn bind_candidate(&mut self,parent:u64)->Result<(),NamespaceResolveError>{
   let cached=self.bound.is_some_and(|b|b.parent==parent);
   let handle=match super::open_file_by_id_access(self.handle,parent,windows::Win32::Storage::FileSystem::FILE_READ_ATTRIBUTES.0){Ok(h)=>h,Err(e)=>{
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
  // Native full identity and hardlink parent identity, never a basename/path guess.
  // Results remain scratch until the caller's generation-bound atomic commit.
  fn recover_parent(&mut self,id:u64,known:&impl Fn(u64)->bool,prefix:&mut Vec<ChangeEvent>,visiting:&mut std::collections::HashSet<u64>,events:&[ChangeEvent],pending:&[ChangeEvent])->Result<bool,NamespaceResolveError>{
   if super::prefix_defines_dir(prefix,id){return Ok(true);}
   if id>>48==0{return Err("unqualified ordinary parent identity".into());}
   if prefix.len()>=64||visiting.len()>=64||!visiting.insert(id){return Err("ordinary parent chain cyclic or exceeds bounded64; cursor retained".into());}
   // The journal already names this directory and its parent. FILE_READ_ATTRIBUTES
   // can return ACCESS_DENIED without proving the record is gone. Do not open it,
   // and do not invent a parent that the journal did not supply.
   if !super::parent_probe_required(known(id)){visiting.remove(&id);return Ok(true);}
   if let Some(dir)=super::journal_directory(events,pending,id){
    if dir.parent!=id && self.recover_parent(dir.parent,known,prefix,visiting,events,pending)? {
     if !super::prefix_defines_dir(prefix,id){
      prefix.push(ChangeEvent::CreatePending{file_id:id,file_id_128:None,parent_id:dir.parent,name:dir.name.to_string(),attrs:dir.attrs,is_dir:true});
     }
     visiting.remove(&id);return Ok(true);
    }
   }
   self.bind_candidate(id)?;
   if self.bound.is_some_and(|b|b.parent==id){visiting.remove(&id);return Ok(false);}
   let h=match super::open_file_by_id_access(self.handle,id,windows::Win32::Storage::FileSystem::FILE_READ_ATTRIBUTES.0){Ok(h)=>h,Err(e)=>{
    if e.absence_candidate()&&crate::ntfs_identity::absent(self.handle,id,self.epoch)?{visiting.remove(&id);return Ok(false);}
    return Err(NamespaceResolveError::Retry(format!("ordinary parent unavailable: {} win32={} ntstatus={:#x}",e.classification(),e.win32,e.native)));
   }};
   let mut info=BY_HANDLE_FILE_INFORMATION::default();let result=unsafe{GetFileInformationByHandle(h,&mut info)};unsafe{let _=CloseHandle(h);}result.map_err(|e|NamespaceResolveError::Retry(e.to_string()))?;
   if info.dwVolumeSerialNumber!=self.serial||((info.nFileIndexHigh as u64)<<32|info.nFileIndexLow as u64)!=id||info.dwFileAttributes&16==0{return Err("ordinary parent fullFRN/volume/directory identity mismatch".into());}
   if known(id){visiting.remove(&id);return Ok(true);}
   let binding=self.bound.unwrap_or(UnlinkBinding{volume:self.volume,serial:self.serial,journal:self.journal,parent:u64::MAX});
   let mut links=self.ordinary_links(id,binding)?;let meta=self.last_metadata;
   links.sort_by_key(|l|!known(l.parent));
   for link in links {
    if self.recover_parent(link.parent,known,prefix,visiting,events,pending)? {
     let(size,mtime,ctime,attrs)=meta.ok_or("recovered ordinary parent lacks verified metadata")?;
     prefix.push(ChangeEvent::Create{entry:findx2_core::RawEntry{file_id:id,file_id_128:None,parent_id:link.parent,name:link.name,size,mtime,ctime,attrs,is_dir:true}});
     visiting.remove(&id);return Ok(true);
    }
   }
   visiting.remove(&id);Ok(false)
  }
  pub fn rewrite(&mut self,events:&[ChangeEvent],pending:&[ChangeEvent],missing_parents:&[u64],journal:u64,known_parent:impl Fn(u64)->bool,known_identity:impl Fn(u64)->bool)->Result<Vec<ChangeEvent>,NamespaceResolveError>{
   if journal!=self.journal{return Err("unlink resolver journal changed; cursor retained".into());}
   crate::ntfs_identity::check(self.handle,self.epoch)?;
   if let Some(binding)=self.bound {self.bind_candidate(binding.parent)?;}
   // Only ids already known to be missing are classified. A corroborated
   // directory base replaces the parent id. A non-directory base keeps the cursor.
   // The map is dropped with this batch. At most 64 missing parents.
   let mut parent_map=std::collections::HashMap::<u64,u64>::new();
   let mut canonical_missing=Vec::new();
   for parent in missing_parents.iter().copied().take(64){
    let opened=self.extension_after_open_failure(parent).map_err(|e|super::annotate(e,"parent",parent,None,events,pending))?;
    let canon=match opened{
     Some(t)=>match crate::ntfs_witness::extension_action(crate::ntfs_witness::IdRole::Parent,Some(t),false){
      crate::ntfs_witness::ExtensionAction::Retarget(base)=>base,
      crate::ntfs_witness::ExtensionAction::Retain(reason)=>return Err(super::annotate(NamespaceResolveError::Retry(format!("{reason} parent={parent:#x} base_raw={:#x}; cursor retained",t.base)),"parent",parent,None,events,pending)),
      _=>parent,
     },
     None=>parent,
    };
    if canon!=parent{parent_map.insert(parent,canon);}
    if !canonical_missing.contains(&canon){canonical_missing.push(canon);}
   }
   let events:Vec<ChangeEvent>=events.iter().map(|ev| super::map_event_parent(ev,&parent_map)).collect();
   let pending:Vec<ChangeEvent>=pending.iter().map(|ev| super::map_event_parent(ev,&parent_map)).collect();
   let mut prefix=Vec::new();
   for parent in canonical_missing{self.recover_parent(parent,&known_parent,&mut prefix,&mut std::collections::HashSet::new(),&events,&pending).map_err(|e|super::annotate(e,"parent",parent,None,&events,&pending))?;}
   let mut result=Vec::new();
   for (index,ev) in events.iter().chain(pending.iter()).enumerate(){
    let parent=match ev{ChangeEvent::CreatePending{parent_id,..}=>Some(*parent_id),ChangeEvent::Rename{new_parent_id,..}=>Some(*new_parent_id),_=>None};
    let resolved=parent.is_none_or(|id|known_parent(id)||super::prefix_defines_dir(&prefix,id));
    let internal=parent.is_some_and(|id|self.bound.is_some_and(|b|b.parent==id));
    let original_parent_remapped=match ev{
     ChangeEvent::CreatePending{parent_id,..}=>parent_map.values().any(|base| base==parent_id),
     ChangeEvent::Rename{new_parent_id,..}=>parent_map.values().any(|base| base==new_parent_id),
     _=>false,
    };
    if !resolved||internal||original_parent_remapped {
     let mut id=match ev{ChangeEvent::CreatePending{file_id,file_id_128,..}=>{if file_id_128.is_some_and(|x|x[8..].iter().any(|b|*b!=0)){return Err(super::annotate("full128 namespace recovery unsupported; no low64 alias".into(),"file",*file_id,Some(ev),&[],&[]));}*file_id},ChangeEvent::Rename{file_id,..}=>*file_id,_=>unreachable!()};
     if let Some(t)=self.extension_after_open_failure(id).map_err(|e|super::annotate(e,"file",id,Some(ev),&[],&[]))?{
      let binding=UnlinkBinding{volume:self.volume,serial:self.serial,journal:self.journal,parent:u64::MAX};
      let names=self.ordinary_links(t.base,binding).map_err(|e|super::annotate(e,"file",id,Some(ev),&[],&[]))?;
      let name=match ev{ChangeEvent::CreatePending{name,..}=>name.as_str(),ChangeEvent::Rename{new_name,..}=>new_name.as_str(),_=>""};
      let matched=names.iter().any(|link| link.name.eq_ignore_ascii_case(name));
      match crate::ntfs_witness::extension_action(crate::ntfs_witness::IdRole::File,Some(t),matched){
       crate::ntfs_witness::ExtensionAction::Retarget(base)=>id=base,
       crate::ntfs_witness::ExtensionAction::Retain(reason)=>return Err(super::annotate(NamespaceResolveError::Retry(format!("{reason} file={id:#x} base_raw={:#x}; cursor retained",t.base)),"file",id,Some(ev),&[],&[])),
       _=>return Err(super::annotate(NamespaceResolveError::Retry(format!("in-use extension file={id:#x} base_raw={:#x} was not retargeted; cursor retained",t.base)),"file",id,Some(ev),&[],&[])),
      }
     }
     if resolved && !internal {
      result.push(super::retarget_file_id(ev,id));
      continue;
     }
     let binding=self.bound.unwrap_or(UnlinkBinding{volume:self.volume,serial:self.serial,journal:self.journal,parent:u64::MAX});
     let mut links=self.ordinary_links(id,binding).map_err(|e|super::annotate(e,"file-links",id,Some(ev),&[],&[]))?;let meta=self.last_metadata;
     links.sort_by_key(|l|!known_parent(l.parent));let mut chosen=None;let mut ordinary_seen=false;
     for link in links {
      self.bind_candidate(link.parent).map_err(|e|super::annotate(e,"hardlink-parent",link.parent,Some(ev),&[],&[]))?;if self.bound.is_some_and(|b|b.parent==link.parent){continue;}
      ordinary_seen=true;if self.recover_parent(link.parent,&known_parent,&mut prefix,&mut std::collections::HashSet::new(),&events,&pending).map_err(|e|super::annotate(e,"hardlink-parent",link.parent,Some(ev),&[],&[]))?{chosen=Some(link);break;}
     }
     if let Some(link)=chosen {
      if known_identity(id){result.push(ChangeEvent::Rename{file_id:id,new_parent_id:link.parent,new_name:link.name});}
      else{let(size,mtime,ctime,attrs)=meta.ok_or_else(||super::annotate("ordinary alias lacks verified metadata".into(),"file",id,Some(ev),&[],&[]))?;result.push(ChangeEvent::Create{entry:findx2_core::RawEntry{file_id:id,file_id_128:None,parent_id:link.parent,name:link.name,size,mtime,ctime,attrs,is_dir:attrs&16!=0}});}
     }else if !ordinary_seen&&(internal||meta.is_none()){result.push(ChangeEvent::Delete{file_id:id});}
     else{return Err(super::annotate(NamespaceResolveError::Retry("live ordinary alias parent unresolved; exact prefix retained".into()),"file-links",id,Some(ev),&[],&[]));}
    }else if index<events.len(){result.push(ev.clone());}
   }
   prefix.extend(result);let result=prefix;
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
#[cfg(windows)]pub use windows_resolver::{
    FileIdOpenError as DiagFileIdError,
    open_file_by_id_diagnostic as open_file_by_id_diagnostic_raw,
};
#[cfg(all(windows, test))]pub(crate) use windows_resolver::inspect_identity_for_test;
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
 #[test]fn failure_context_records_role_event_and_basename_only(){
  let ev=ChangeEvent::CreatePending{file_id:0x0005_0000_0008_5567,file_id_128:None,parent_id:0x0009_0000_0008_5571,name:r"C:\Users\secret\Microsoft.VisualStudio.WebToolsExtensions.vsix".into(),attrs:0,is_dir:false};
  let text=failure_context("parent",0x0009_0000_0008_5571,None,std::slice::from_ref(&ev),&[]);
  assert!(text.contains("role=parent")&&text.contains("frn=0x9000000085571")&&text.contains("event=create_pending")&&text.contains("file=0x5000000085567")&&text.contains("parent=0x9000000085571"),"{text}");
  assert!(text.contains("name=Microsoft.VisualStudio.WebToolsExtensions.vsix"),"{text}");
  assert!(!text.contains("Users")&&!text.contains('\\')&&!text.contains(':'),"{text}");
 }
 #[test]fn several_links_preserve_exact_case(){let links=[OrdinaryLink{parent:900,name:"internal".into()},OrdinaryLink{parent:2,name:"KeepCASE.bin".into()},OrdinaryLink{parent:3,name:"other.bin".into()}];let v=rewrite_bound_event(b(),b'C',1,7,&ev(900),Some(&links)).unwrap();assert!(matches!(&v[0],ChangeEvent::Rename{new_name,new_parent_id:2,..} if new_name=="KeepCASE.bin"));}
 #[test]
 fn access_denied_parent_uses_the_journal_directory_and_keeps_the_base() {
  const FILE: u64 = 0x3600000004421e;
  const PARENT: u64 = 0x470000002d1a80;
  const BASE: u64 = 0x0005_0000_0008_5567;
  const CHILD: u64 = 0x0001_0000_0004_421f;
  let name = "21d9a857-8903-45c7-a571-052b561452f4";
  let dir = ChangeEvent::CreatePending { file_id: FILE, file_id_128: None, parent_id: PARENT, name: name.into(), attrs: 0x10, is_dir: true };
  let child = ChangeEvent::CreatePending { file_id: CHILD, file_id_128: None, parent_id: FILE, name: "child.tmp".into(), attrs: 0, is_dir: false };
  let found = journal_directory(std::slice::from_ref(&dir), &[], FILE).unwrap();
  assert_eq!(found.parent, PARENT);
  assert_eq!(found.name, name);
  let file_only = ChangeEvent::CreatePending { file_id: FILE, file_id_128: None, parent_id: PARENT, name: name.into(), attrs: 0, is_dir: false };
  assert!(journal_directory(std::slice::from_ref(&file_only), &[], FILE).is_none());
  let root = findx2_core::platform::RawEntry { file_id: 1, file_id_128: None, parent_id: 0, name: String::new(), size: 0, mtime: 0, ctime: 0, attrs: 0x10, is_dir: true };
  let parent = findx2_core::platform::RawEntry { file_id: PARENT, file_id_128: None, parent_id: 1, name: "parent".into(), size: 0, mtime: 0, ctime: 0, attrs: 0x10, is_dir: true };
  let base = findx2_core::platform::RawEntry { file_id: BASE, file_id_128: None, parent_id: 1, name: "Microsoft.VisualStudio.WebToolsExtensions.vsix".into(), size: 9, mtime: 0, ctime: 0, attrs: 0, is_dir: false };
  let store = findx2_core::IndexBuilder::new(b'C', 1, 7, 10).build_from_raw(vec![base], vec![root, parent], false).unwrap();
  let engine = findx2_core::SearchEngine::new(store);
  engine.enable_usage(1024 * 1024).unwrap();
  let events = [dir, child];
  assert!(engine.namespace_parent_resolvable(b'C', FILE, &events));
  assert!(!parent_probe_required(true));
  assert!(parent_probe_required(false));
  engine.apply_volume_batch(b'C', &events, 7, 20).unwrap();
  let g = engine.index_store();
  assert_eq!(g.volumes[0].last_usn, 20);
  assert!(g.dir_on_volume(b'C', FILE).is_some());
  assert!(g.entry_on_volume(b'C', CHILD).is_some());
  let base_idx = g.entry_on_volume(b'C', BASE).unwrap() as usize;
  assert!(!g.entries[base_idx].is_deleted());
  assert_eq!(g.name_bytes(&g.entries[base_idx]), b"Microsoft.VisualStudio.WebToolsExtensions.vsix");
 }
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

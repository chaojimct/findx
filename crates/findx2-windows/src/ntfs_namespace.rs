//! Exact-identity NTFS internal unlink translation. No path/name heuristics in event matching.
use findx2_core::ChangeEvent;
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
 pub struct NtfsUnlinkResolver { handle:HANDLE,volume:u8,serial:u32,journal:u64,bound:Option<UnlinkBinding>, last_metadata:Option<(u64,u64,u64,u32)> }
 impl Drop for NtfsUnlinkResolver {fn drop(&mut self){unsafe{let _=CloseHandle(self.handle);}}}
 impl NtfsUnlinkResolver {
  pub fn new(volume_path:&str,volume:u8,serial:u32,journal:u64)->Result<Self,String>{
   if crate::get_volume_serial_number(volume_path).map_err(|e|e.to_string())?!=serial{return Err("namespace volume serial mismatch".into());}
   let handle=crate::usn::open_volume_handle(&crate::usn::volume_device_path(volume_path)).map_err(|e|e.to_string())?;
   let actual=unsafe{crate::usn::query_usn_journal(handle)}.map_err(|e|e.to_string())?;
   if actual.UsnJournalID!=journal{unsafe{let _=CloseHandle(handle);}return Err("namespace journal mismatch".into());}
   Ok(Self{handle,volume,serial,journal,bound:None,last_metadata:None})
  }
  fn open_id(&self,id:u64)->windows::core::Result<HANDLE>{
   let mut d=FILE_ID_DESCRIPTOR::default();d.dwSize=size_of::<FILE_ID_DESCRIPTOR>() as u32;d.Type=FileIdType;d.Anonymous.FileId=id as i64;
   unsafe{OpenFileById(self.handle,&d,0,FILE_SHARE_READ|FILE_SHARE_WRITE|FILE_SHARE_DELETE,None,FILE_FLAG_BACKUP_SEMANTICS|FILE_FLAG_OPEN_REPARSE_POINT)}
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
    Err(e) if matches!(e.code().0 as u32&0xffff,2|3|87|1168)=>{
     // Not a generic '87 means deleted' rule: only a native unlink event into a
     // verified internal parent, with a valid descriptor/epoch, reaches here.
     return Ok(Vec::new());
    },
    Err(e)=>return Err(format!("bound unlink identity metadata unavailable: {e}; cursor retained")),
   };
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
    parse_hardlink_records(&buffer[..ios.information],binding.parent)
   })();unsafe{let _=CloseHandle(handle);}result
  }
  pub fn rewrite(&mut self,events:&[ChangeEvent],pending:&[ChangeEvent],missing_parents:&[u64],journal:u64,known_parent:impl Fn(u64)->bool,known_identity:impl Fn(u64)->bool)->Result<Vec<ChangeEvent>,String>{
   if journal!=self.journal{return Err("unlink resolver journal changed; cursor retained".into());}
   for parent in missing_parents.iter().take(64){self.bind_candidate(*parent)?;if self.bound.is_some(){break;}}
   let Some(binding)=self.bound else{return Ok(events.to_vec());};
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
   Ok(result)
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

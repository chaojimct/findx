//! Owned-file semantics only. Never opens a raw volume, journal, system metadata,
//! production index, service, or an object outside the newly-created fixture.
#![cfg(windows)]
use std::{fs::{File,OpenOptions}, mem::size_of, os::windows::{fs::OpenOptionsExt,io::{AsRawHandle,FromRawHandle}}, path::Path};
use windows::Win32::{Foundation::HANDLE,Storage::FileSystem::*};
fn handle(f:&File)->HANDLE {HANDLE(f.as_raw_handle() as _)}
thread_local! {static LAST_RETRYABLE:std::cell::Cell<bool>=const{std::cell::Cell::new(false)};}
thread_local! {static LAST_NATIVE:std::cell::Cell<Option<u32>>=const{std::cell::Cell::new(None)};}
fn by_id(hint:&File,id:u64)->Result<File,u32> {
    LAST_NATIVE.with(|s|s.set(None));LAST_RETRYABLE.with(|s|s.set(false));
    findx2_windows::open_file_by_id_diagnostic(handle(hint),id)
       .map(|h|unsafe{File::from_raw_handle(h.0 as _)})
       .map_err(|e|{LAST_RETRYABLE.with(|s|s.set(e.is_transient_delete_pending()));LAST_NATIVE.with(|s|s.set(Some(e.last_ntstatus)));e.win32})
}

fn info(f:&File)->BY_HANDLE_FILE_INFORMATION {
    let mut i=BY_HANDLE_FILE_INFORMATION::default();
    unsafe{GetFileInformationByHandle(handle(f),&mut i)}.unwrap();i
}
fn standard(f:&File)->FILE_STANDARD_INFO {
    let mut i=FILE_STANDARD_INFO::default();
    unsafe{GetFileInformationByHandleEx(handle(f),FileStandardInfo,(&mut i as *mut FILE_STANDARD_INFO).cast(),size_of::<FILE_STANDARD_INFO>() as u32)}.unwrap();i
}
#[repr(C)] struct IoStatus {status:usize, information:usize}
#[link(name="ntdll")]
unsafe extern "system" {fn NtQueryInformationFile(h:HANDLE,io:*mut IoStatus,b:*mut u8,len:u32,class:u32)->i32;}
fn names(f:&File)->serde_json::Value {
    let mut b=vec![0u8;65536];let mut io=IoStatus{status:0,information:0};
    let status=unsafe{NtQueryInformationFile(handle(f),&mut io,b.as_mut_ptr(),b.len() as u32,46)};
    let mut links=vec![];
    if status>=0 && io.information>=8 && io.information<=b.len() {
        let count=u32::from_le_bytes(b[4..8].try_into().unwrap());let mut at=8usize;
        for _ in 0..count.min(1024) {
            if at+20>io.information{break;}
            let next=u32::from_le_bytes(b[at..at+4].try_into().unwrap()) as usize;
            let parent=u64::from_le_bytes(b[at+8..at+16].try_into().unwrap());
            let n=u32::from_le_bytes(b[at+16..at+20].try_into().unwrap()) as usize;
            if at+20+n*2>io.information{break;}
            let wide:Vec<u16>=b[at+20..at+20+n*2].chunks_exact(2).map(|s|u16::from_le_bytes([s[0],s[1]])).collect();
            links.push(serde_json::json!({"parent_id":format!("{parent:#x}"),"name":String::from_utf16_lossy(&wide)}));
            if next==0{break;}at+=next;
        }
    }
    serde_json::json!({"ntstatus":format!("{:#010x}",status as u32),"returned_bytes":io.information,"links":links})
}
fn opened_name(f:&File)->Option<String> {
    let mut b=vec![0u16;32768];let n=unsafe{GetFinalPathNameByHandleW(handle(f),&mut b,FILE_NAME_OPENED)} as usize;
    (n>0 && n<b.len()).then(||String::from_utf16_lossy(&b[..n]))
}
fn observe(root:&Path,posix:bool,alias:bool)->serde_json::Value {
    let dir=root.join(format!("posix-{posix}-hardlink-{alias}"));std::fs::create_dir(&dir).unwrap();
    let path=dir.join("owned.txt");let other=dir.join("surviving-alias.txt");
    std::fs::write(&path,b"owned fixture content").unwrap();if alias {std::fs::hard_link(&path,&other).unwrap();}
    let hint=OpenOptions::new().read(true).share_mode(7).custom_flags(FILE_FLAG_BACKUP_SEMANTICS.0).open(&dir).unwrap();
    // DELETE + FILE_READ_ATTRIBUTES, full sharing; no ACL/token privilege changes.
    let held=OpenOptions::new().read(true).share_mode(7).open(&path).unwrap();
    let pending=OpenOptions::new().access_mode(0x10000|0x80).share_mode(7).open(&path).unwrap();
    let initial=info(&pending);let id=(initial.nFileIndexHigh as u64)<<32|initial.nFileIndexLow as u64;
    let baseline=by_id(&hint,id).expect("same production flags must open the ordinary owned identity before deletion");
    assert_eq!(info(&baseline).dwVolumeSerialNumber,initial.dwVolumeSerialNumber);drop(baseline);
    if posix {
        let d=FILE_DISPOSITION_INFO_EX{Flags:FILE_DISPOSITION_INFO_EX_FLAGS(FILE_DISPOSITION_FLAG_DELETE.0|FILE_DISPOSITION_FLAG_POSIX_SEMANTICS.0)};
        unsafe{SetFileInformationByHandle(handle(&pending),FileDispositionInfoEx,(&d as *const FILE_DISPOSITION_INFO_EX).cast(),size_of::<FILE_DISPOSITION_INFO_EX>() as u32)}.unwrap();
    }else{
        let d=FILE_DISPOSITION_INFO{DeleteFile:true};
        unsafe{SetFileInformationByHandle(handle(&pending),FileDispositionInfo,(&d as *const FILE_DISPOSITION_INFO).cast(),size_of::<FILE_DISPOSITION_INFO>() as u32)}.unwrap();
    }
    // POSIX removes the namespace link only after the disposition handle closes;
    // retain a distinct old read handle to exercise the real orphan lifetime.
    drop(pending);
    let metadata=standard(&held);
    let reopened=by_id(&hint,id);
    let code=reopened.as_ref().err().copied();let native_status=LAST_NATIVE.with(|s|s.get());
    if let Ok(f)=&reopened {let i=info(f);assert_eq!((i.nFileIndexHigh as u64)<<32|i.nFileIndexLow as u64,id);assert_eq!(i.dwVolumeSerialNumber,initial.dwVolumeSerialNumber);}
    if !posix && !alias {assert_eq!(native_status,Some(0xc0000056),"native capture must distinguish STATUS_DELETE_PENDING on this owned fixture");assert_eq!(code,Some(5),"documented classic delete-pending maps to ACCESS_DENIED, not proof of ACL failure");assert!(metadata.DeletePending);assert!(LAST_RETRYABLE.with(|s|s.get()),"actual native delete-pending must request retry, not permanent fence or absence");}
    let by_id_links=reopened.as_ref().ok().map(names);
    let held_links=names(&held);let held_opened_name=opened_name(&held);
    if posix {
        assert!(reopened.is_ok(),"ordinary owned POSIX $Deleted identities remain reopenable with production flags");
        let q=by_id_links.as_ref().unwrap();assert_eq!(q["ntstatus"],"0x00000000");
        let owner=info(&hint);let parent_id=format!("{:#x}",(owner.nFileIndexHigh as u64)<<32|owner.nFileIndexLow as u64);
        let ordinary=q["links"].as_array().unwrap().iter().filter(|l|l["parent_id"]==parent_id).collect::<Vec<_>>();
        if alias {assert!(ordinary.iter().any(|l|l["name"]=="surviving-alias.txt"));}
        else {assert!(ordinary.is_empty(),"do not invent a surviving ordinary namespace name");}
    }
    let alias_during=if alias {Some(std::fs::read(&other).is_ok())}else{None};
    drop(reopened);drop(held);
    let final_reopen=by_id(&hint,id);
    let final_code=final_reopen.as_ref().err().copied();let final_native=LAST_NATIVE.with(|s|s.get());
    if alias {assert!(final_reopen.is_ok(),"ordinary surviving alias must remain reopenable after old handles close");} else {assert!(final_reopen.is_err(),"last-link identity must no longer reopen");}
    drop(final_reopen);
    assert!(!path.exists(),"only the selected fixture link is removed");
    if alias {assert_eq!(std::fs::read(&other).unwrap(),b"owned fixture content");}
    let result=serde_json::json!({"after_last_handle_close_win32_error":final_code,"after_last_handle_close_native":final_native.map(|s|format!("{s:#010x}")),"native_open_status":native_status.map(|s|format!("{s:#010x}")),"held_opened_name":held_opened_name,"held_handle_hardlink_query":held_links,"zero_access_by_id_hardlink_query":by_id_links,"posix":posix,"has_surviving_hardlink":alias,"open_by_id_win32_error":code,"delete_pending_on_existing_handle":metadata.DeletePending,"number_of_links_on_existing_handle":metadata.NumberOfLinks,"alias_readable_while_delete_pending":alias_during,"alias_content_preserved_after_close":alias});
    drop(hint);std::fs::remove_dir_all(dir).unwrap();result
}
#[test]
fn access_denied_is_not_a_safe_identity_absence_or_hardlink_deletion_proof() {
    let stamp=std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let root=std::env::temp_dir().join(format!("findx-t164-owned-delete-semantics-{}-{stamp}",std::process::id()));
    std::fs::create_dir(&root).unwrap();
    let mut outcomes=vec![];for posix in [false,true] {for alias in [false,true] {outcomes.push(observe(&root,posix,alias));}}
    println!("T164_OWNED_NTFS_OUTCOMES={}",serde_json::to_string(&outcomes).unwrap());
    std::fs::remove_dir(root).unwrap();
}

//! 卷句柄 + 文件 ID 取元数据（`OpenFileById`），供 MFT 回填与 USN 增量共用。
//! 先尝试 64 位 `FileIdType`；失败时再尝试 `ExtendedFileIdType` + `FILE_ID_128`（USN v3 / ReFS 等场景）。

use std::mem::size_of;

use windows::Win32::Foundation::{CloseHandle, FILETIME, HANDLE};
use windows::Win32::Storage::FileSystem::{
    ExtendedFileIdType, FileIdType, GetFileInformationByHandle, OpenFileById,
    BY_HANDLE_FILE_INFORMATION, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
    FILE_ID_128, FILE_ID_DESCRIPTOR, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ,
    FILE_SHARE_WRITE,
};

/// 返回 `(size, mtime_filetime_u64, ctime_filetime_u64)`。
unsafe fn fetch_file_metadata_by_id_inner(
    volume: HANDLE,
    file_id_64: u64,
    file_id_128: Option<[u8; 16]>,
) -> Option<(u64, u64, u64)> {
    let mut desc = FILE_ID_DESCRIPTOR::default();
    desc.dwSize = size_of::<FILE_ID_DESCRIPTOR>() as u32;
    desc.Type = FileIdType;
    desc.Anonymous.FileId = file_id_64 as i64;

    let first = OpenFileById(
        volume,
        &desc,
        FILE_READ_ATTRIBUTES.0,
        FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
        None,
        FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
    );

    let hf = match first {
        Ok(h) if !h.is_invalid() => h,
        Ok(h) => {
            let _ = CloseHandle(h);
            let id128 = file_id_128?;
            let mut desc2 = FILE_ID_DESCRIPTOR::default();
            desc2.dwSize = size_of::<FILE_ID_DESCRIPTOR>() as u32;
            desc2.Type = ExtendedFileIdType;
            desc2.Anonymous.ExtendedFileId = FILE_ID_128 { Identifier: id128 };
            let h2 = OpenFileById(
                volume,
                &desc2,
                FILE_READ_ATTRIBUTES.0,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                None,
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            )
            .map_err(|e| { note_metadata_failure("open128", e.code().0 as u32 & 0xffff); e }).ok()?;
            if h2.is_invalid() {
                return None;
            }
            h2
        }
        Err(e) => {
            note_metadata_failure("open64", e.code().0 as u32 & 0xffff);
            let id128 = file_id_128?;
            let mut desc2 = FILE_ID_DESCRIPTOR::default();
            desc2.dwSize = size_of::<FILE_ID_DESCRIPTOR>() as u32;
            desc2.Type = ExtendedFileIdType;
            desc2.Anonymous.ExtendedFileId = FILE_ID_128 { Identifier: id128 };
            let h2 = OpenFileById(
                volume,
                &desc2,
                FILE_READ_ATTRIBUTES.0,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                None,
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            )
            .map_err(|e| { note_metadata_failure("open128", e.code().0 as u32 & 0xffff); e }).ok()?;
            if h2.is_invalid() {
                return None;
            }
            h2
        }
    };

    let mut bh = BY_HANDLE_FILE_INFORMATION::default();
    if let Err(e) = GetFileInformationByHandle(hf, &mut bh) {
        note_metadata_failure("get_information", e.code().0 as u32 & 0xffff);
        let _ = CloseHandle(hf);
        return None;
    }
    let _ = CloseHandle(hf);

    let size = ((bh.nFileSizeHigh as u64) << 32) | bh.nFileSizeLow as u64;
    let mtime = filetime_to_u64(bh.ftLastWriteTime);
    let ctime = filetime_to_u64(bh.ftCreationTime);
    Some((size, mtime, ctime))
}

fn filetime_to_u64(ft: FILETIME) -> u64 {
    ((ft.dwHighDateTime as u64) << 32) | ft.dwLowDateTime as u64
}

fn note_metadata_failure(stage: &'static str, code: u32) {
    LAST_METADATA_ERROR.with(|v|v.set((stage,code)));
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTS: [AtomicU64; 6] = [const { AtomicU64::new(0) }; 6];
    let bucket = match code { 2|3 => 0, 5 => 1, 32|33 => 2, 87 => 3, 1168 => 4, _ => 5 };
    let count = COUNTS[bucket].fetch_add(1, Ordering::Relaxed)+1;
    if count.is_power_of_two() { eprintln!("metadata_win32_failure stage={stage} code={code} bucket={bucket} count={count}"); }
}

thread_local! { static LAST_METADATA_ERROR: std::cell::Cell<(&'static str,u32)> = const { std::cell::Cell::new(("unclassified",0)) }; }
static TERMINAL_COUNTS: [std::sync::atomic::AtomicU64;7] = [const { std::sync::atomic::AtomicU64::new(0) };7];
pub unsafe fn fetch_file_metadata_by_id(volume: HANDLE,file_id_64:u64,file_id_128:Option<[u8;16]>) -> Option<(u64,u64,u64)> {
    use std::sync::atomic::Ordering;
    LAST_METADATA_ERROR.with(|v|v.set(("unclassified",0)));
    let result=fetch_file_metadata_by_id_inner(volume,file_id_64,file_id_128);
    let (stage,code)=LAST_METADATA_ERROR.with(|v|v.get());
    let bucket=if result.is_some(){6}else{match code{2|3=>0,5=>1,32|33=>2,87=>3,1168=>4,_=>5}};
    let count=TERMINAL_COUNTS[bucket].fetch_add(1,Ordering::Relaxed)+1;
    if count.is_power_of_two(){eprintln!("metadata_terminal_outcome stage={stage} code={code} bucket={bucket} count={count} snapshot={:?}",metadata_terminal_counts());}
    result
}
pub fn metadata_terminal_counts()->[u64;7]{std::array::from_fn(|i|TERMINAL_COUNTS[i].load(std::sync::atomic::Ordering::Relaxed))}

//! 卷句柄 + 文件 ID 取元数据（`OpenFileById`），供 MFT 回填与 USN 增量共用。
//! 先尝试 64 位 `FileIdType`；失败时再尝试 `ExtendedFileIdType` + `FILE_ID_128`（USN v3 / ReFS 等场景）。

use std::mem::size_of;

use windows::Win32::Foundation::{CloseHandle, FILETIME, HANDLE};
use windows::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, ExtendedFileIdType, FileIdType, GetFileInformationByHandle,
    OpenFileById, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_ID_128,
    FILE_ID_DESCRIPTOR, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ,
    FILE_SHARE_WRITE,
};

/// 返回 `(size, mtime_filetime_u64, ctime_filetime_u64)`。
pub unsafe fn fetch_file_metadata_by_id(
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
        _ => {
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
            .ok()?;
            if h2.is_invalid() {
                return None;
            }
            h2
        }
    };

    let mut bh = BY_HANDLE_FILE_INFORMATION::default();
    if GetFileInformationByHandle(hf, &mut bh).is_err() {
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

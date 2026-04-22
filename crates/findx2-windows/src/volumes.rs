//! 枚举可用于建索引的本地卷路径（供 CLI / GUI 默认全盘扫描）。

/// `GetDriveTypeW` 返回值（节选）
#[cfg(windows)]
const DRIVE_REMOVABLE: u32 = 2;
#[cfg(windows)]
const DRIVE_FIXED: u32 = 3;

/// 枚举本机「固定磁盘」与「可移动磁盘」盘符（`C:` / `D:`）。
/// 跳过光驱、网络盘或未就绪的根路径。
#[cfg(windows)]
pub fn enumerate_local_drive_letters() -> Vec<String> {
    use windows::Win32::Storage::FileSystem::GetDriveTypeW;

    let mut out = Vec::new();
    for ch in b'A'..=b'Z' {
        let mut root_utf16: Vec<u16> = format!("{}:\\", ch as char).encode_utf16().collect();
        root_utf16.push(0);
        let t = unsafe { GetDriveTypeW(windows::core::PCWSTR(root_utf16.as_ptr())) };
        if t == DRIVE_FIXED || t == DRIVE_REMOVABLE {
            out.push(format!("{}:", ch as char));
        }
    }
    out.sort();
    out
}

#[cfg(not(windows))]
pub fn enumerate_local_drive_letters() -> Vec<String> {
    Vec::new()
}

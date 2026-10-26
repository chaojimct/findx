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

/// 通过 `IOCTL_STORAGE_QUERY_PROPERTY` (StorageDeviceSeekPenaltyProperty) 检测卷是否为 SSD。
///
/// SSD 的 `IncursSeekPenalty = FALSE`（无寻道惩罚），HDD 为 `TRUE`。
/// 返回 `true` 表示 SSD，`false` 表示 HDD 或检测失败（保守回退到 HDD 策略）。
#[cfg(windows)]
pub fn is_ssd(volume: &str) -> bool {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_SHARE_DELETE, FILE_SHARE_READ,
        FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    use windows::Win32::System::Ioctl::{
        DEVICE_SEEK_PENALTY_DESCRIPTOR, IOCTL_STORAGE_QUERY_PROPERTY, PropertyStandardQuery,
        StorageDeviceSeekPenaltyProperty, STORAGE_PROPERTY_QUERY,
    };
    use windows::Win32::System::IO::DeviceIoControl;

    let vol = volume.trim().trim_end_matches('\\');
    let drive_char = vol.chars().next().map(|c| c.to_ascii_uppercase()).unwrap_or('?');
    let dev_path = format!(r"\\.\{}:", drive_char);
    let wide: Vec<u16> = dev_path.encode_utf16().chain(Some(0)).collect();

    let h = unsafe {
        CreateFileW(
            windows::core::PCWSTR(wide.as_ptr()),
            0, // 不需要读写权限，只查询设备属性
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            None,
        )
    };
    let h = match h {
        Ok(h) if !h.is_invalid() => h,
        _ => return false,
    };

    let mut query = STORAGE_PROPERTY_QUERY::default();
    query.PropertyId = StorageDeviceSeekPenaltyProperty;
    query.QueryType = PropertyStandardQuery;

    let mut descriptor = DEVICE_SEEK_PENALTY_DESCRIPTOR::default();
    let mut returned: u32 = 0;

    let ok = unsafe {
        DeviceIoControl(
            h,
            IOCTL_STORAGE_QUERY_PROPERTY,
            Some((&query as *const STORAGE_PROPERTY_QUERY) as *const _),
            std::mem::size_of::<STORAGE_PROPERTY_QUERY>() as u32,
            Some((&mut descriptor as *mut DEVICE_SEEK_PENALTY_DESCRIPTOR) as *mut _),
            std::mem::size_of::<DEVICE_SEEK_PENALTY_DESCRIPTOR>() as u32,
            Some(&mut returned),
            None,
        )
    };
    let _ = unsafe { CloseHandle(h) };

    match ok {
        Ok(()) => !descriptor.IncursSeekPenalty.as_bool(),
        Err(_) => false,
    }
}

#[cfg(not(windows))]
pub fn enumerate_local_drive_letters() -> Vec<String> {
    Vec::new()
}

#[cfg(not(windows))]
pub fn is_ssd(_volume: &str) -> bool {
    false
}

#[cfg(test)]
#[cfg(windows)]
mod tests {
    use super::*;

    #[test]
    fn test_ssd_detection() {
        for drive in &["C:", "D:", "E:"] {
            let ssd = is_ssd(drive);
            eprintln!("{}: {}", drive, if ssd { "SSD" } else { "HDD/unknown" });
        }
    }
}

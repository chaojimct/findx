//! Mirror internal copy/cut paths to the OS clipboard as `CF_HDROP` so
//! Explorer (or any file manager) can paste them. Best effort by design:
//! any failure is silently ignored and the in-app clipboard is unaffected.

#[cfg(windows)]
pub fn write_hdrop(paths: &[std::path::PathBuf]) {
    use std::os::windows::ffi::OsStrExt;
    use windows::{
        Win32::Foundation::{HANDLE, GlobalFree},
        Win32::System::DataExchange::{
            CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
        },
        Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock},
        Win32::System::Ole::CF_HDROP,
    };

    if paths.is_empty() {
        return;
    }
    // Local mirror of the DROPFILES header (shellapi.h): offset to the file
    // list, unused mouse point, then wide-char flag. Defined locally so no
    // extra Shell feature surface is needed.
    #[repr(C)]
    struct DropFiles {
        p_files: u32,
        pt_x: i32,
        pt_y: i32,
        f_nc: i32,
        f_wide: i32,
    }
    // `\\?\`-style paths so >260-char names survive the round trip.
    fn long_path(p: &std::path::Path) -> String {
        use std::path::Component;
        let mut comps = p.components();
        match comps.next() {
            Some(Component::Prefix(_)) => p.to_string_lossy().into_owned(),
            _ => {
                let s = p.to_string_lossy();
                if s.starts_with(r"\\") {
                    format!(r"\\?\UNC\{}", &s[2..])
                } else {
                    format!(r"\\?\{s}")
                }
            }
        }
    }

    let mut wide: Vec<u16> = Vec::new();
    for p in paths {
        let prefixed = long_path(p);
        wide.extend(std::ffi::OsStr::new(&prefixed).encode_wide());
        wide.push(0);
    }
    wide.push(0); // double-null terminator
    let bytes = std::mem::size_of::<DropFiles>() + wide.len() * 2;

    unsafe {
        let mem = match GlobalAlloc(GMEM_MOVEABLE, bytes) {
            Ok(h) if !h.is_invalid() => h,
            _ => return,
        };
        let ptr = GlobalLock(mem) as *mut u8;
        if ptr.is_null() {
            let _ = GlobalFree(Some(mem));
            return;
        }
        let header = DropFiles {
            p_files: std::mem::size_of::<DropFiles>() as u32,
            pt_x: 0,
            pt_y: 0,
            f_nc: 0,
            f_wide: 1,
        };
        std::ptr::copy_nonoverlapping(
            &header as *const DropFiles as *const u8,
            ptr,
            std::mem::size_of::<DropFiles>(),
        );
        std::ptr::copy_nonoverlapping(
            wide.as_ptr() as *const u8,
            ptr.add(std::mem::size_of::<DropFiles>()),
            wide.len() * 2,
        );
        let _ = GlobalUnlock(mem);
        if OpenClipboard(None).is_err() {
            let _ = GlobalFree(Some(mem));
            return;
        }
        let placed = EmptyClipboard()
            .and_then(|_| SetClipboardData(CF_HDROP.0 as u32, Some(HANDLE(mem.0))));
        // On success the system owns the block; on failure free it here.
        if placed.is_err() {
            let _ = GlobalFree(Some(mem));
        }
        let _ = CloseClipboard();
    }
}

#[cfg(not(windows))]
pub fn write_hdrop(_paths: &[std::path::PathBuf]) {}

//! Windows IFilter 文本抽取（.doc/.ppt/.msg/.rtf 等老格式）。
//!
//! 走系统已注册的 IFilter（Office / WPS / Adobe 装完会注册），与 AnyTXT 的
//! `ATFHelper.exe` 同构：**进程内 COM 调用，不启动 Word/PowerPoint**。
//!
//! 正常路径应使用 [`crate::ifilter_pool`]（独立 worker 进程 + 超时 kill 重启）；
//! 本模块的 in-process 版本只作为 worker 缺失时的兜底（IFilter 若挂死会拖住
//! 调用线程，所以生产路径必须走进程池）。

#[cfg(windows)]
pub fn extract(path: &std::path::Path) -> crate::Result<String> {
    imp::extract(path)
}

#[cfg(not(windows))]
pub fn extract(path: &std::path::Path) -> crate::Result<String> {
    Err(crate::Error::Extract(
        path.display().to_string(),
        "IFilter 仅支持 Windows".into(),
    ))
}

#[cfg(windows)]
mod imp {
    use std::ffi::c_void;

    use windows::core::{Interface, HSTRING, PCWSTR, PWSTR};
    use windows::Win32::Foundation::RPC_E_CHANGED_MODE;
    use windows::Win32::Storage::IndexServer::{
        IFilter, LoadIFilter, CHUNK_TEXT, FILTER_E_END_OF_CHUNKS, FILTER_E_NO_MORE_TEXT,
        FILTER_E_NO_TEXT, STAT_CHUNK,
    };
    use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};

    /// 单文件文本上限（字符）：防止病态文档把内存吃光。
    const MAX_CHARS: usize = 64 * 1024 * 1024;

    pub fn extract(path: &std::path::Path) -> crate::Result<String> {
        unsafe {
            // IFilter 普遍要求 STA；已是其它套间（例如宿主已 MTA）时
            // RPC_E_CHANGED_MODE 不代表失败，直接沿用当前套间。
            let hr = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            let did_init = hr.is_ok();
            if !did_init && hr != RPC_E_CHANGED_MODE {
                return Err(crate::Error::Extract(
                    path.display().to_string(),
                    format!("CoInitializeEx 失败: {hr:?}"),
                ));
            }
            let r = run(path);
            if did_init {
                CoUninitialize();
            }
            r
        }
    }

    unsafe fn run(path: &std::path::Path) -> crate::Result<String> {
        let wide = HSTRING::from(path.as_os_str().to_string_lossy().as_ref());
        let mut raw: *mut c_void = std::ptr::null_mut();
        LoadIFilter(PCWSTR(wide.as_ptr()), None, &mut raw).map_err(|e| {
            crate::Error::Extract(
                path.display().to_string(),
                format!("LoadIFilter 失败（无可用 IFilter）: {e}"),
            )
        })?;
        if raw.is_null() {
            return Err(crate::Error::Extract(
                path.display().to_string(),
                "IFilter 未注册".into(),
            ));
        }
        // from_raw 接管这一个引用，Drop 时 Release。
        let filter: IFilter = IFilter::from_raw(raw);

        let mut flags = 0u32;
        let hr = filter.Init(0, &[], &mut flags);
        if hr < 0 {
            return Err(crate::Error::Extract(
                path.display().to_string(),
                format!("IFilter.Init 失败: 0x{:08x}", hr as u32),
            ));
        }

        let mut out = String::new();
        let mut buf = vec![0u16; 8192];
        'chunks: loop {
            let mut stat = STAT_CHUNK::default();
            let hr = filter.GetChunk(&mut stat);
            if hr == FILTER_E_END_OF_CHUNKS.0 {
                break;
            }
            if hr < 0 {
                // 部分过滤器对加密/损坏文件先给若干块再报错：有文本就保留。
                if out.trim().is_empty() {
                    return Err(crate::Error::Extract(
                        path.display().to_string(),
                        format!("IFilter.GetChunk 失败: 0x{:08x}", hr as u32),
                    ));
                }
                break;
            }
            if (stat.flags.0 & CHUNK_TEXT.0) == 0 {
                continue;
            }
            loop {
                let mut n = buf.len() as u32;
                let hr = filter.GetText(&mut n, PWSTR(buf.as_mut_ptr()));
                if hr == FILTER_E_NO_MORE_TEXT.0 || hr == FILTER_E_NO_TEXT.0 {
                    break;
                }
                if hr < 0 {
                    break;
                }
                let written = (n as usize).min(buf.len());
                if written == 0 {
                    break;
                }
                out.push_str(&String::from_utf16_lossy(&buf[..written]));
                if out.chars().count() >= MAX_CHARS {
                    break 'chunks;
                }
                if written < buf.len() {
                    break;
                }
            }
        }

        let text = out.replace("\r\n", "\n").replace('\r', "\n");
        if text.trim().is_empty() {
            return Err(crate::Error::Extract(
                path.display().to_string(),
                "IFilter 未取到文本".into(),
            ));
        }
        Ok(text)
    }
}

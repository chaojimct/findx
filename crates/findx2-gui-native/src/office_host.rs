//! Installed IPreviewHandler lives in a disposable STA helper, never in GPUI.
//! Coordinates are client-relative physical pixels; helper follows owner moves/DPI.
use crate::preview_worker::Cancel;
use serde::{Deserialize, Serialize};
use std::{
    io::{BufRead, BufReader, Read, Write},
    path::Path,
    process::{Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};
use windows::{
    Win32::{
        Foundation::{HWND, POINT, RECT},
        Graphics::Gdi::ClientToScreen,
        System::{Com::*, Registry::*},
        UI::{
            Shell::PropertiesSystem::{IInitializeWithFile, IInitializeWithStream},
            Shell::{IPreviewHandler, SHCreateStreamOnFileEx},
            WindowsAndMessaging::*,
        },
    },
    core::{GUID, Interface, PCWSTR},
};
#[derive(Clone, Copy, Serialize, Deserialize, PartialEq, Default)]
struct Position {
    rect: [i32; 4],
    visible: bool,
}
pub struct Surface {
    position: Mutex<Position>,
    suppressed: AtomicBool,
    cancel: Cancel,
    alive: AtomicBool,
}
impl Surface {
    pub fn place(&self, bounds: gpui_kit::Bounds<gpui_kit::Pixels>, scale: f32) {
        if let Ok(mut p) = self.position.lock() {
            if self.suppressed.load(Ordering::Acquire)
                || self.cancel.load(Ordering::Acquire)
                || !self.is_alive()
            {
                p.visible = false;
                return;
            }
            *p = Position {
                rect: [
                    (bounds.origin.x.as_f32() * scale).round() as i32,
                    (bounds.origin.y.as_f32() * scale).round() as i32,
                    (bounds.size.width.as_f32() * scale).round() as i32,
                    (bounds.size.height.as_f32() * scale).round() as i32,
                ],
                visible: true,
            };
        }
    }
    pub fn set_suppressed(&self, suppressed: bool) {
        self.suppressed.store(suppressed, Ordering::Release);
        if suppressed {
            self.hide();
        }
    }
    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Acquire)
    }
    pub fn hide(&self) {
        if let Ok(mut p) = self.position.lock() {
            p.visible = false;
        }
    }
}
pub struct NativeClient {
    pub surface: Arc<Surface>,
}
impl Drop for NativeClient {
    fn drop(&mut self) {
        self.surface.set_suppressed(true);
        self.surface.cancel.store(true, Ordering::Release);
    }
}

pub fn start(path: &str, cancel: Cancel) -> Result<NativeClient, String> {
    let meta = std::fs::metadata(path).map_err(|e| e.to_string())?;
    if meta.len() > 32 * 1024 * 1024 {
        return Err("文档超过 32 MiB 系统预览上限".into());
    }
    let owner = find_owner();
    if owner == 0 {
        return Err("原生主窗口尚未就绪".into());
    }
    let mut command = Command::new(std::env::current_exe().map_err(|e| e.to_string())?);
    command.args(["--office-preview-host", path, &owner.to_string()]);
    use std::os::windows::process::CommandExt;
    command.creation_flags(0x08000000);
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    let stdout = child.stdout.take().ok_or("预览宿主输出不可用")?;
    let (tx, rx) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let mut line = String::new();
        let result = BufReader::new(stdout)
            .take(8192)
            .read_line(&mut line)
            .map_err(|e| e.to_string())
            .and_then(|_| {
                serde_json::from_str::<Result<(), String>>(&line).map_err(|e| e.to_string())
            })
            .and_then(|r| r);
        let _ = tx.send(result);
    });
    let start = Instant::now();
    let ready = loop {
        if cancel.load(Ordering::Acquire) {
            break Err("系统预览已取消".into());
        }
        if start.elapsed() > Duration::from_secs(8) {
            break Err("系统预览处理器超时（8 秒）；已终止独立宿主".into());
        }
        match rx.recv_timeout(Duration::from_millis(20)) {
            Ok(r) => break r,
            Err(mpsc::RecvTimeoutError::Disconnected) => break Err("系统预览宿主提前退出".into()),
            Err(_) => {}
        }
    };
    if let Err(e) = ready {
        let _ = child.kill();
        let _ = child.wait();
        return Err(e);
    }
    let surface = Arc::new(Surface {
        position: Mutex::new(Position::default()),
        suppressed: AtomicBool::new(false),
        cancel,
        alive: AtomicBool::new(true),
    });
    let state = surface.clone();
    std::thread::spawn(move || {
        let mut stdin = child.stdin.take();
        let mut last = None;
        while !state.cancel.load(Ordering::Acquire) {
            if child.try_wait().ok().flatten().is_some() {
                break;
            }
            let p = state.position.lock().map(|p| *p).unwrap_or_default();
            if last != Some(p) {
                if let Some(input) = stdin.as_mut() {
                    if serde_json::to_writer(&mut *input, &p).is_err()
                        || input.write_all(b"\n").is_err()
                        || input.flush().is_err()
                    {
                        break;
                    }
                }
                last = Some(p);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        state.alive.store(false, Ordering::Release);
        drop(stdin); // EOF asks STA to Unload and destroy its own window.
        for _ in 0..10 {
            if child.try_wait().ok().flatten().is_some() {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = child.kill();
        let _ = child.wait();
    });
    Ok(NativeClient { surface })
}
fn find_owner() -> usize {
    unsafe extern "system" fn visit(
        h: HWND,
        data: windows::Win32::Foundation::LPARAM,
    ) -> windows::core::BOOL {
        unsafe {
            let mut pid = 0;
            GetWindowThreadProcessId(h, Some(&mut pid));
            if pid == std::process::id() {
                let mut title = [0u16; 128];
                let n = GetWindowTextW(h, &mut title);
                if n > 0
                    && crate::isolation::is_main_window_title(&String::from_utf16_lossy(&title[..n as usize]))
                {
                    *(data.0 as *mut usize) = h.0 as usize;
                    return false.into();
                }
            }
            true.into()
        }
    }
    let mut hwnd = 0usize;
    unsafe {
        let _ = EnumWindows(
            Some(visit),
            windows::Win32::Foundation::LPARAM(&mut hwnd as *mut usize as isize),
        );
    }
    hwnd
}
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}
fn registry(path: &str, view: REG_SAM_FLAGS) -> Option<String> {
    unsafe {
        let mut key = HKEY::default();
        let path = wide(path);
        RegOpenKeyExW(
            HKEY_CLASSES_ROOT,
            PCWSTR(path.as_ptr()),
            Some(0),
            KEY_READ | view,
            &mut key,
        )
        .ok()
        .ok()?;
        let mut buf = [0u16; 1024];
        let mut size = 2048;
        let result = RegQueryValueExW(
            key,
            PCWSTR::null(),
            None,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut size),
        );
        let _ = RegCloseKey(key);
        result.ok().ok()?;
        Some(String::from_utf16_lossy(
            &buf[..(size as usize / 2).saturating_sub(1).min(buf.len())],
        ))
    }
}
fn candidates(path: &Path) -> Vec<GUID> {
    let ext = format!(
        ".{}",
        path.extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase()
    );
    let key = "shellex\\{8895b1c6-b41f-4c1c-a562-0d564250836f}";
    let mut result = Vec::new();
    for view in [KEY_WOW64_64KEY, KEY_WOW64_32KEY] {
        let mut keys = vec![format!("{ext}\\{key}")];
        if let Some(prog) = registry(&ext, view) {
            keys.push(format!("{prog}\\{key}"));
        }
        keys.push(format!("SystemFileAssociations\\{ext}\\{key}"));
        for k in keys {
            if let Some(s) = registry(&k, view) {
                if let Ok(g) = GUID::try_from(s.trim_matches(['{', '}'])) {
                    if !result.contains(&g) {
                        result.push(g);
                    }
                }
            }
        }
    }
    result
}
unsafe fn initialize(handler: &IPreviewHandler, path: &str) -> Result<(), String> {
    unsafe {
        let p = wide(path);
        if let Ok(init) = handler.cast::<IInitializeWithStream>() {
            if let Ok(stream) = SHCreateStreamOnFileEx(
                PCWSTR(p.as_ptr()),
                (STGM_READ | STGM_SHARE_DENY_NONE).0,
                0,
                false,
                None,
            ) {
                if init.Initialize(&stream, STGM_READ.0).is_ok() {
                    return Ok(());
                }
            }
        }
        if let Ok(init) = handler.cast::<IInitializeWithFile>() {
            if init.Initialize(PCWSTR(p.as_ptr()), STGM_READ.0).is_ok() {
                return Ok(());
            }
        }
        Err("处理器不接受只读 Stream/File 初始化".into())
    }
}
/// Helper entry: COM object, HWND and every COM call stay on this STA until Unload.
pub fn host_main(args: &[String]) {
    let result = unsafe { host_setup(args) };
    if let Err(e) = result {
        let _ = serde_json::to_writer(std::io::stdout(), &Err::<(), String>(e));
        println!();
    }
}
unsafe fn host_setup(args: &[String]) -> Result<(), String> {
    unsafe {
        let _ = windows::Win32::UI::HiDpi::SetProcessDpiAwarenessContext(
            windows::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        );
        CoInitializeEx(None, COINIT_APARTMENTTHREADED)
            .ok()
            .map_err(|e| e.to_string())?;
        struct Com;
        impl Drop for Com {
            fn drop(&mut self) {
                unsafe {
                    CoUninitialize();
                }
            }
        }
        let _com = Com;
        let path = args.get(2).ok_or("缺少文档路径")?;
        let owner = HWND(
            args.get(3)
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(0) as *mut _,
        );
        if !IsWindow(Some(owner)).as_bool() {
            return Err("主窗口已关闭".into());
        }
        let clsids = candidates(Path::new(path));
        if clsids.is_empty() {
            return Err("该格式未注册可用的系统预览处理器；保留文字摘录".into());
        }
        let mut selected = None;
        let mut errors = Vec::new();
        for clsid in clsids {
            for ctx in [
                CLSCTX_LOCAL_SERVER | CLSCTX_ACTIVATE_32_BIT_SERVER,
                CLSCTX_LOCAL_SERVER | CLSCTX_ACTIVATE_64_BIT_SERVER,
                CLSCTX_INPROC_SERVER | CLSCTX_ACTIVATE_64_BIT_SERVER,
            ] {
                match CoCreateInstance::<_, IPreviewHandler>(&clsid, None, ctx) {
                    Ok(h) => match initialize(&h, path) {
                        Ok(()) => {
                            selected = Some(h);
                            break;
                        }
                        Err(e) => errors.push(e),
                    },
                    Err(e) => errors.push(e.to_string()),
                }
            }
            if selected.is_some() {
                break;
            }
        }
        let handler = selected.ok_or_else(|| {
            format!(
                "已注册的系统处理器无法启用：{}",
                errors.last().cloned().unwrap_or_default()
            )
        })?;
        let class = wide("STATIC");
        let host = CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            PCWSTR(class.as_ptr()),
            PCWSTR::null(),
            WS_POPUP | WS_CLIPCHILDREN,
            0,
            0,
            600,
            800,
            Some(owner),
            None,
            None,
            None,
        )
        .map_err(|e| e.to_string())?;
        struct Host(HWND);
        impl Drop for Host {
            fn drop(&mut self) {
                unsafe {
                    let _ = DestroyWindow(self.0);
                }
            }
        }
        let _host = Host(host);
        let initial = RECT {
            left: 0,
            top: 0,
            right: 600,
            bottom: 800,
        };
        if let Err(e) = handler
            .SetWindow(host, &initial)
            .and_then(|_| handler.DoPreview())
        {
            let _ = handler.Unload();
            return Err(format!("系统处理器拒绝预览：{e}"));
        }
        serde_json::to_writer(std::io::stdout(), &Ok::<(), String>(()))
            .map_err(|e| e.to_string())?;
        println!();
        let _ = std::io::stdout().flush();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for line in std::io::stdin().lock().lines() {
                let Ok(line) = line else {
                    break;
                };
                let Ok(p) = serde_json::from_str::<Position>(&line) else {
                    break;
                };
                if tx.send(Some(p)).is_err() {
                    return;
                }
            }
            let _ = tx.send(None);
        });
        let mut p = Position::default();
        let mut last_rect = [i32::MIN; 4];
        let mut last_size = [0, 0];
        let mut shown = false;
        'pump: loop {
            loop {
                match rx.try_recv() {
                    Ok(Some(next)) => p = next,
                    Ok(None) | Err(mpsc::TryRecvError::Disconnected) => break 'pump,
                    Err(_) => break,
                }
            }
            if !IsWindow(Some(owner)).as_bool() {
                break;
            }
            let visible =
                p.visible && !IsIconic(owner).as_bool() && IsWindowVisible(owner).as_bool();
            if visible {
                let [x, y, w, h] = p.rect;
                let mut pt = POINT { x, y };
                let _ = ClientToScreen(owner, &mut pt);
                let r = [pt.x, pt.y, w.max(1), h.max(1)];
                if r != last_rect {
                    let _ = SetWindowPos(
                        host,
                        None,
                        r[0],
                        r[1],
                        r[2],
                        r[3],
                        SWP_NOACTIVATE | SWP_NOZORDER,
                    );
                    last_rect = r;
                }
                if [w, h] != last_size {
                    let rect = RECT {
                        left: 0,
                        top: 0,
                        right: w.max(1),
                        bottom: h.max(1),
                    };
                    let _ = handler.SetRect(&rect);
                    last_size = [w, h];
                }
            }
            if visible != shown {
                let _ = ShowWindow(host, if visible { SW_SHOW } else { SW_HIDE });
                shown = visible;
            }
            let mut msg = MSG::default();
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                if msg.message == WM_QUIT {
                    break 'pump;
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            std::thread::sleep(Duration::from_millis(16));
        }
        let _ = ShowWindow(host, SW_HIDE);
        let _ = handler.Unload();
        Ok(())
    }
}

#[cfg(test)]
mod surface_visibility_tests {
    use super::*;
    fn surface() -> Surface {
        Surface {
            position: Mutex::new(Position::default()),
            suppressed: AtomicBool::new(false),
            cancel: Arc::new(AtomicBool::new(false)),
            alive: AtomicBool::new(true),
        }
    }
    fn bounds() -> gpui_kit::Bounds<gpui_kit::Pixels> {
        gpui_kit::Bounds {
            origin: gpui_kit::point(gpui_kit::px(10.), gpui_kit::px(20.)),
            size: gpui_kit::size(gpui_kit::px(100.), gpui_kit::px(200.)),
        }
    }
    #[test]
    fn late_canvas_place_cannot_show_suppressed_office_surface() {
        let s = surface();
        s.place(bounds(), 2.);
        assert!(s.position.lock().unwrap().visible);
        s.set_suppressed(true);
        s.place(bounds(), 2.);
        assert!(!s.position.lock().unwrap().visible);
    }
    #[test]
    fn closing_menu_does_not_auto_show_an_unmounted_preview() {
        let s = surface();
        s.place(bounds(), 1.);
        s.set_suppressed(true);
        s.set_suppressed(false);
        assert!(!s.position.lock().unwrap().visible);
        s.place(bounds(), 1.);
        assert!(s.position.lock().unwrap().visible);
    }
    #[test]
    fn cancelled_or_dead_worker_cannot_be_revealed_by_a_late_frame() {
        let s = surface();
        s.cancel.store(true, Ordering::Release);
        s.set_suppressed(false);
        s.place(bounds(), 2.);
        assert!(!s.position.lock().unwrap().visible);
        s.cancel.store(false, Ordering::Release);
        s.alive.store(false, Ordering::Release);
        s.place(bounds(), 2.);
        assert!(!s.position.lock().unwrap().visible);
    }
    #[test]
    fn dropping_client_hides_before_cancelling_its_worker() {
        let s = Arc::new(surface());
        s.place(bounds(), 2.);
        let client = NativeClient { surface: s.clone() };
        drop(client);
        assert!(!s.position.lock().unwrap().visible);
        assert!(s.cancel.load(Ordering::Acquire));
        s.set_suppressed(false);
        s.place(bounds(), 2.);
        assert!(!s.position.lock().unwrap().visible);
    }
}

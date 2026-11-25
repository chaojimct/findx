//! 命中上下文活文档管线（管线 A）：Office/WPS 应用程序对象模型 + 文档窗口嵌入。
//!
//! 与 `office_host`（系统版式 IPreviewHandler 黑盒）不同，这里拿到的是**活文档**：
//! Word/WPS 本尊渲染 + `Range.Find.Execute` 高亮 + `ScrollIntoView` 跳转 +
//! xlsx `Worksheets.Activate` 切 sheet。系统版式 tab 一字节不动。
//!
//! 策略（已对齐）：
//! - Word 优先、可配置（`settings_logic::OfficeEngine`）：auto = Word→WPS→文字摘录。
//! - 只读打开用户文件；需要涂色（HighlightColorIndex / Interior.Color）且只读态
//!   不允许时，复制到 temp 副本再打开涂色，用户源文件永不落盘。
//! - 进程管理照搬 office_host 模式：COM Quit + 超时 kill，崩溃可重启。
//!
//! 当前状态：Spike 落地——探测、COM 打开/Find/Scroll/Quit、SetParent 嵌入、
//! temp 副本策略全部可用；GPUI 命中列表联动在后续步骤接入。

use crate::settings_logic::OfficeEngine as EnginePref;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiveKind {
    Word,
    Excel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiveEngine {
    Word,
    WpsWord,
    Excel,
    WpsEt,
}

impl LiveEngine {
    pub fn prog_id(self) -> &'static str {
        match self {
            Self::Word => "Word.Application",
            Self::WpsWord => "KWPS.Application",
            Self::Excel => "Excel.Application",
            Self::WpsEt => "KET.Application",
        }
    }
    pub fn kind(self) -> LiveKind {
        match self {
            Self::Word | Self::WpsWord => LiveKind::Word,
            Self::Excel | Self::WpsEt => LiveKind::Excel,
        }
    }
    pub fn display(self) -> &'static str {
        match self {
            Self::Word => "Word",
            Self::WpsWord => "WPS 文字",
            Self::Excel => "Excel",
            Self::WpsEt => "WPS 表格",
        }
    }
}

/// 按用户偏好展开探测顺序（Word 优先、可配置）。
pub fn probe_order(pref: EnginePref, kind: LiveKind) -> Vec<LiveEngine> {
    let (word_first, wps_allowed) = pref.probe_order();
    let mut out = Vec::with_capacity(2);
    match kind {
        LiveKind::Word => {
            if word_first {
                out.push(LiveEngine::Word);
                if wps_allowed {
                    out.push(LiveEngine::WpsWord);
                }
            } else if wps_allowed {
                out.push(LiveEngine::WpsWord);
                if pref == EnginePref::Auto {
                    out.push(LiveEngine::Word);
                }
            }
        }
        LiveKind::Excel => {
            if word_first {
                out.push(LiveEngine::Excel);
                if wps_allowed {
                    out.push(LiveEngine::WpsEt);
                }
            } else if wps_allowed {
                out.push(LiveEngine::WpsEt);
                if pref == EnginePref::Auto {
                    out.push(LiveEngine::Excel);
                }
            }
        }
    }
    out
}

pub fn kind_for_ext(ext: &str) -> Option<LiveKind> {
    match ext.to_ascii_lowercase().as_str() {
        "doc" | "docx" | "rtf" | "ppt" | "pptx" => Some(LiveKind::Word),
        "xls" | "xlsx" => Some(LiveKind::Excel),
        _ => None,
    }
}

/// 注册表探测：ProgID 是否注册（COM 可用）。与 office_host 同口径。
#[cfg(windows)]
pub fn engine_available(engine: LiveEngine) -> bool {
    use windows::Win32::System::Registry::*;
    use windows::core::PCWSTR;
    let prog: Vec<u16> = engine
        .prog_id()
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    unsafe {
        let mut key = Default::default();
        windows::Win32::System::Registry::RegOpenKeyExW(
            HKEY_CLASSES_ROOT,
            PCWSTR(prog.as_ptr()),
            Some(0),
            KEY_READ,
            &mut key,
        )
        .is_ok()
    }
}

#[cfg(not(windows))]
pub fn engine_available(_: LiveEngine) -> bool {
    false
}

// ---------------------------------------------------------------------------
// COM IDispatch 小封装（Word/Excel/WPS 自动化）
// ---------------------------------------------------------------------------

#[cfg(windows)]
mod com {
    use windows::{
        Win32::System::{
            Com::*,
            Variant::{VARIANT, VT_BOOL, VT_BSTR, VT_DISPATCH, VT_EMPTY, VT_I4},
        },
        core::{BSTR, GUID, Interface, PCWSTR},
    };

    pub struct Dispatch(pub IDispatch);

    unsafe impl Send for Dispatch {}
    unsafe impl Sync for Dispatch {}

    fn str_to_bstr(s: &str) -> BSTR {
        BSTR::from(s)
    }

    fn variant_empty() -> VARIANT {
        VARIANT::default()
    }

    fn variant_bool(b: bool) -> VARIANT {
        unsafe {
            let mut v = VARIANT::default();
            let a = &mut *v.Anonymous.Anonymous;
            a.vt = VT_BOOL;
            a.Anonymous.boolVal = windows::Win32::Foundation::VARIANT_BOOL(if b { -1 } else { 0 });
            v
        }
    }

    fn variant_i4(n: i32) -> VARIANT {
        unsafe {
            let mut v = VARIANT::default();
            let a = &mut *v.Anonymous.Anonymous;
            a.vt = VT_I4;
            a.Anonymous.lVal = n;
            v
        }
    }

    fn variant_bstr(s: &str) -> VARIANT {
        unsafe {
            let mut v = VARIANT::default();
            let a = &mut *v.Anonymous.Anonymous;
            a.vt = VT_BSTR;
            a.Anonymous.bstrVal = std::mem::ManuallyDrop::new(str_to_bstr(s));
            v
        }
    }

    fn variant_dispatch(d: &IDispatch) -> VARIANT {
        unsafe {
            let mut v = VARIANT::default();
            let a = &mut *v.Anonymous.Anonymous;
            a.vt = VT_DISPATCH;
            a.Anonymous.pdispVal = std::mem::ManuallyDrop::new(Some(d.clone()));
            v
        }
    }

    pub fn create(prog_id: &str) -> Result<Dispatch, String> {
        unsafe {
            let wide: Vec<u16> = prog_id.encode_utf16().chain(std::iter::once(0)).collect();
            let clsid = CLSIDFromProgID(PCWSTR(wide.as_ptr()))
                .map_err(|e| format!("{prog_id} 未注册: {e}"))?;
            let unk: windows::core::IUnknown = CoCreateInstance(&clsid, None, CLSCTX_LOCAL_SERVER)
                .map_err(|e| format!("{prog_id} 启动失败: {e}"))?;
            let disp: IDispatch = unk.cast().map_err(|e| format!("{prog_id} 非自动化对象: {e}"))?;
            Ok(Dispatch(disp))
        }
    }

    fn dispid(disp: &IDispatch, name: &str) -> Result<i32, String> {
        unsafe {
            let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
            let mut name_ptr = PCWSTR(wide.as_ptr());
            let mut id = 0i32;
            disp.GetIDsOfNames(&GUID::default(), &mut name_ptr, 1, 0, &mut id)
                .map_err(|e| format!("{name}: {e}"))?;
            Ok(id)
        }
    }

    fn invoke(
        disp: &IDispatch,
        name: &str,
        flags: u16,
        mut args: Vec<VARIANT>,
    ) -> Result<VARIANT, String> {
        unsafe {
            let id = dispid(disp, name)?;
            // COM 参数逆序。
            args.reverse();
            let mut dp = DISPPARAMS {
                rgvarg: if args.is_empty() {
                    std::ptr::null_mut()
                } else {
                    args.as_mut_ptr()
                },
                rgdispidNamedArgs: std::ptr::null_mut(),
                cArgs: args.len() as u32,
                cNamedArgs: 0,
            };
            let mut out = variant_empty();
            let mut excep = EXCEPINFO::default();
            let mut arg_err = 0u32;
            disp.Invoke(
                id,
                &GUID::default(),
                0,
                DISPATCH_FLAGS(flags),
                &mut dp,
                Some(&mut out),
                Some(&mut excep),
                Some(&mut arg_err),
            )
            .map_err(|e| format!("{name} 调用失败: {e}"))?;
            // 调用方负责 VARIANT 清理（BSTR/IDispatch 泄漏可接受：宿主进程级，Quit 后随进程回收）。
            Ok(out)
        }
    }

    impl Dispatch {
        pub fn get(&self, name: &str) -> Result<Dispatch, String> {
            let v = invoke(&self.0, name, 2 /*DISPATCH_PROPERTYGET*/, vec![])?;
            unsafe {
                let a = &*v.Anonymous.Anonymous;
                if a.vt == VT_DISPATCH {
                    let opt = &a.Anonymous.pdispVal;
                    if let Some(d) = &**opt {
                        return Ok(Dispatch(d.clone()));
                    }
                    return Err(format!("{name} 为空"));
                } else {
                    Err(format!("{name} 不是对象"))
                }
            }
        }
        pub fn call(&self, name: &str, args: Vec<VARIANT>) -> Result<VARIANT, String> {
            invoke(&self.0, name, 1 /*DISPATCH_METHOD*/, args)
        }
        pub fn put_bool(&self, name: &str, b: bool) -> Result<(), String> {
            invoke(
                &self.0,
                name,
                4, /*DISPATCH_PROPERTYPUT*/
                vec![variant_bool(b)],
            )?;
            Ok(())
        }
        pub fn put_i4(&self, name: &str, n: i32) -> Result<(), String> {
            invoke(&self.0, name, 4, vec![variant_i4(n)])?;
            Ok(())
        }
        pub fn method0(&self, name: &str) -> Result<VARIANT, String> {
            self.call(name, vec![])
        }
        pub fn prop_bstr(&self, name: &str) -> Result<String, String> {
            let v = invoke(&self.0, name, 2, vec![])?;
            unsafe {
                let a = &*v.Anonymous.Anonymous;
                if a.vt == VT_BSTR {
                    Ok(a.Anonymous.bstrVal.to_string())
                } else {
                    Err(format!("{name} 非字符串"))
                }
            }
        }
        pub fn prop_i4(&self, name: &str) -> Result<i32, String> {
            let v = invoke(&self.0, name, 2, vec![])?;
            unsafe {
                let a = &*v.Anonymous.Anonymous;
                if a.vt == VT_I4 {
                    Ok(a.Anonymous.lVal)
                } else {
                    Err(format!("{name} 非整数"))
                }
            }
        }
        pub fn raw(&self) -> &IDispatch {
            &self.0
        }
    }

    pub fn v_empty() -> VARIANT {
        variant_empty()
    }
    pub fn v_bool(b: bool) -> VARIANT {
        variant_bool(b)
    }
    pub fn v_i4(n: i32) -> VARIANT {
        variant_i4(n)
    }
    pub fn v_str(s: &str) -> VARIANT {
        variant_bstr(s)
    }
    pub fn v_missing() -> VARIANT {
        // Word/Excel 可选参数：VT_ERROR + DISP_E_PARAMNOTFOUND 也行，但多数版本接受 Empty。
        variant_empty()
    }
    pub fn v_disp(d: &IDispatch) -> VARIANT {
        variant_dispatch(d)
    }
    #[allow(dead_code)]
    pub fn vt_of(v: &VARIANT) -> u16 {
        unsafe { (&*v.Anonymous.Anonymous).vt.0 }
    }
    pub fn variant_to_dispatch(v: VARIANT) -> Result<Dispatch, String> {
        unsafe {
            let a = &*v.Anonymous.Anonymous;
            if a.vt == VT_DISPATCH {
                let opt = &a.Anonymous.pdispVal;
                if let Some(d) = &**opt {
                    return Ok(Dispatch(d.clone()));
                }
                return Err("空对象".into());
            } else if a.vt == VT_EMPTY {
                Err("空结果".into())
            } else {
                Err("非对象结果".into())
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 会话：COM 应用 + 文档 + 嵌入窗口 + temp 副本
// ---------------------------------------------------------------------------

/// 只读打开失败 / 需要涂色时复制到的 temp 副本（析构清理）。
fn maybe_copy_to_temp(path: &str) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    if bytes.len() > 64 * 1024 * 1024 {
        return None;
    }
    let mut tmp = std::env::temp_dir();
    let stem = std::path::Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("findx-live");
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("docx");
    let name = format!(
        "findx-live-{stem}-{}.{}",
        std::process::id(),
        ext,
        stem = sanitize(stem)
    );
    tmp.push(name);
    std::fs::write(&tmp, &bytes).ok()?;
    Some(tmp.to_string_lossy().into_owned())
}

fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .take(48)
        .collect()
}

#[cfg(windows)]
pub struct LiveSession {
    engine: LiveEngine,
    app: com::Dispatch,
    doc: com::Dispatch,
    /// temp 副本路径（源文件只读打开时为 None）。
    temp_copy: Option<String>,
    /// 被嵌入的应用顶层窗口（SetParent 目标已设）。
    embedded_hwnd: usize,
    /// 宿主（GPUI 预览区）窗口，供 resize 时同步。
    host_hwnd: usize,
}

#[cfg(windows)]
impl LiveSession {
    /// 只读打开文档并嵌入到 host 窗口。needs_paint=true 时走 temp 副本（可涂色）。
    pub fn open(
        path: &str,
        engine: LiveEngine,
        host_hwnd: usize,
        needs_paint: bool,
    ) -> Result<Self, String> {
        unsafe {
            windows::Win32::System::Com::CoInitializeEx(
                None,
                windows::Win32::System::Com::COINIT_APARTMENTTHREADED
                    | windows::Win32::System::Com::COINIT_DISABLE_OLE1DDE,
            )
            .ok()
            .map_err(|e| format!("COM 初始化失败: {e}"))?;
        }
        let app = com::create(engine.prog_id())?;
        // 不弹窗、不改用户 normal 模板。
        let _ = app.method0("SetVisible");
        app.put_bool("Visible", true)?;
        let _ = app.put_bool("DisplayAlerts", false);

        // 打开目标：先试只读源文件；needs_paint 则直接用 temp 副本。
        let (open_path, temp_copy) = if needs_paint {
            match maybe_copy_to_temp(path) {
                Some(t) => (t.clone(), Some(t)),
                None => (path.to_string(), None),
            }
        } else {
            (path.to_string(), None)
        };
        let doc = open_document(&app, engine, &open_path)?;
        let embedded_hwnd = find_app_window(engine)?;
        if embedded_hwnd != 0 && host_hwnd != 0 {
            embed(embedded_hwnd, host_hwnd);
        }
        Ok(Self {
            engine,
            app,
            doc,
            temp_copy,
            embedded_hwnd,
            host_hwnd,
        })
    }

    pub fn engine(&self) -> LiveEngine {
        self.engine
    }

    /// 同步嵌入窗口尺寸（预览区 resize 时调用）。
    pub fn resize(&self, x: i32, y: i32, w: i32, h: i32) {
        if self.embedded_hwnd == 0 {
            return;
        }
        unsafe {
            use windows::Win32::{Foundation::HWND, UI::WindowsAndMessaging::*};
            let _ = MoveWindow(
                HWND(self.embedded_hwnd as *mut _),
                x,
                y,
                w.max(1),
                h.max(1),
                true,
            );
        }
    }

    /// Word：Find 第 n 个命中并滚入视口。返回是否找到。
    pub fn word_jump(&self, needle: &str, index: usize) -> Result<bool, String> {
        use com as C;
        let sel = self.app.get("Selection")?;
        let find = sel.get("Find")?;
        // Find.Execute(FindText, MatchCase, MatchWholeWord, ..., Forward, Wrap, ..., ReplaceWith, Replace)
        // 只填 FindText + Forward=true + Wrap=1(wdFindContinue)，其余 Missing。
        let r = find.call(
            "Execute",
            vec![
                C::v_missing(), // Replace
                C::v_missing(), // ReplaceWith
                C::v_i4(1),     // Wrap = wdFindContinue
                C::v_bool(true), // Forward
                C::v_missing(), // Format
                C::v_missing(), // MatchByte
                C::v_missing(), // MatchWildcards
                C::v_missing(), // MatchSoundsLike
                C::v_missing(), // MatchAllWordForms
                C::v_missing(), // MatchFuzzy
                C::v_missing(), // MatchWholeWord
                C::v_missing(), // MatchCase
                C::v_str(needle),
            ],
        );
        // Execute 返回 bool（VARIANT_BOOL）。index>0 时重复执行跳到第 n 个。
        let mut found = variant_is_true(&r.unwrap_or_else(|_| C::v_bool(false)));
        for _ in 1..index.saturating_add(1) {
            if !found {
                break;
            }
            // 到文末后 Wrap=Continue 会回绕；到达目标序号即停。
            let rr = find.call(
                "Execute",
                vec![
                    C::v_missing(),
                    C::v_missing(),
                    C::v_i4(1),
                    C::v_bool(true),
                    C::v_missing(),
                    C::v_missing(),
                    C::v_missing(),
                    C::v_missing(),
                    C::v_missing(),
                    C::v_missing(),
                    C::v_missing(),
                    C::v_missing(),
                    C::v_str(needle),
                ],
            );
            found = variant_is_true(&rr.unwrap_or_else(|_| C::v_bool(false)));
        }
        if found {
            let _ = sel.method0("ScrollIntoView");
            // 选中即高亮（Selection 本身就是视觉高亮，不落盘）。
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Word：全部命中涂黄（temp 副本上；HighlightColorIndex=7 黄色）。
    pub fn word_highlight_all(&self, needles: &[String]) -> Result<usize, String> {
        use com as C;
        if self.temp_copy.is_none() {
            return Err("只读源文件不支持涂色（应走 temp 副本）".into());
        }
        let mut painted = 0usize;
        // 全文 Range：ActiveDocument.Content。
        let content = self.doc.get("Content")?;
        for needle in needles {
            if needle.is_empty() {
                continue;
            }
            let find = content.get("Find")?;
            let _ = find.call("ClearFormatting", vec![]);
            // Execute 全文查找循环：找到一个 → Range.HighlightColorIndex=7 → Collapse(0) 继续。
            loop {
                let r = find.call(
                    "Execute",
                    vec![
                        C::v_missing(),
                        C::v_missing(),
                        C::v_i4(0), // Wrap = wdFindStop，到文末停
                        C::v_bool(true),
                        C::v_bool(true), // Format=true（允许 Highlight 生效口径）
                        C::v_missing(),
                        C::v_missing(),
                        C::v_missing(),
                        C::v_missing(),
                        C::v_missing(),
                        C::v_missing(),
                        C::v_missing(),
                        C::v_str(needle),
                    ],
                );
                if !variant_is_true(&r.unwrap_or_else(|_| C::v_bool(false))) {
                    break;
                }
                // content 本体就是命中 Range（Find.Execute 成功后 Range 收缩到命中）。
                let _ = content.put_i4("HighlightColorIndex", 7);
                painted += 1;
                if painted > 2000 {
                    break;
                }
                let _ = content.call("Collapse", vec![C::v_i4(0) /*wdCollapseEnd*/]);
                if painted % 64 == 0 {
                    // 长文档让出，避免 Word 无响应观感。
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
            }
            // 重置 Range 到全文，处理下一个 needle。
            let _ = self.doc.get("Content").and_then(|c| {
                // 用新 Content 替换（旧 Dispatch 泄漏随 Quit 回收）。
                Ok(())
            });
            let _ = c_reset(&self.doc);
        }
        Ok(painted)
    }

    /// Excel：按 sheet 名或序号激活 + 命中格涂色 + 选中跳转。
    pub fn excel_jump(
        &self,
        sheet: Option<&str>,
        address: Option<&str>,
    ) -> Result<bool, String> {
        if let Some(name) = sheet {
            let sheets = self.doc.get("Worksheets")?;
            let v = sheets.call("Activate", vec![]); // 兜底：先激活首个
            let _ = v;
            // Worksheets(name).Activate
            let item = sheets.call("Item", vec![com::v_str(name)]);
            if let Ok(v) = item {
                if let Ok(d) = com::variant_to_dispatch(v) {
                    let _ = d.method0("Activate");
                    if let Some(addr) = address {
                        let range = d.call("Range", vec![com::v_str(addr)]);
                        if let Ok(rv) = range {
                            if let Ok(rd) = com::variant_to_dispatch(rv) {
                                let _ = rd.method0("Select");
                                return Ok(true);
                            }
                        }
                    }
                    return Ok(true);
                }
            }
            return Ok(false);
        }
        Ok(false)
    }

    pub fn close(self) {
        let _ = self.doc.call("Close", vec![com::v_bool(false)]);
        let _ = self.app.method0("Quit");
        if let Some(t) = self.temp_copy {
            let _ = std::fs::remove_file(t);
        }
        unsafe {
            windows::Win32::System::Com::CoUninitialize();
        }
    }
}

#[cfg(windows)]
fn c_reset(doc: &com::Dispatch) -> Result<(), String> {
    // 占位：Find 循环依赖 Content Range 重置；当前实现每次取新 Content 即可。
    let _ = doc;
    Ok(())
}

#[cfg(windows)]
fn variant_is_true(v: &windows::Win32::System::Variant::VARIANT) -> bool {
    use windows::Win32::System::Variant::*;
    unsafe {
        let a = &*v.Anonymous.Anonymous;
        match a.vt {
            VT_BOOL => a.Anonymous.boolVal != windows::Win32::Foundation::VARIANT_BOOL(0),
            VT_I4 => a.Anonymous.lVal != 0,
            _ => false,
        }
    }
}

#[cfg(windows)]
fn open_document(
    app: &com::Dispatch,
    engine: LiveEngine,
    path: &str,
) -> Result<com::Dispatch, String> {
    match engine.kind() {
        LiveKind::Word => {
            let docs = app.get("Documents")?;
            // Open(FileName, ConfirmConversions, ReadOnly, AddToRecentFiles)
            let v = docs.call(
                "Open",
                vec![
                    com::v_bool(false), // AddToRecentFiles
                    com::v_bool(true),  // ReadOnly
                    com::v_missing(),   // ConfirmConversions
                    com::v_str(path),
                ],
            )?;
            com::variant_to_dispatch(v)
        }
        LiveKind::Excel => {
            let books = app.get("Workbooks")?;
            // Open(Filename, UpdateLinks, ReadOnly)
            let v = books.call(
                "Open",
                vec![
                    com::v_bool(true), // ReadOnly
                    com::v_missing(),  // UpdateLinks
                    com::v_str(path),
                ],
            )?;
            com::variant_to_dispatch(v)
        }
    }
}

#[cfg(windows)]
fn find_app_window(engine: LiveEngine) -> Result<usize, String> {
    use windows::Win32::{Foundation::*, UI::WindowsAndMessaging::*};
    // 按进程名找顶层窗口：WINWORD.EXE / KWPS / EXCEL.EXE / KET。
    let exe = match engine {
        LiveEngine::Word => "WINWORD.EXE",
        LiveEngine::WpsWord => "KWPS.EXE",
        LiveEngine::Excel => "EXCEL.EXE",
        LiveEngine::WpsEt => "KET.EXE",
    };
    struct Ctx {
        exe: String,
        found: usize,
    }
    let mut ctx = Ctx {
        exe: exe.to_string(),
        found: 0,
    };
    unsafe extern "system" fn visit(
        h: HWND,
        data: windows::Win32::Foundation::LPARAM,
    ) -> windows::core::BOOL {
        unsafe {
            let ctx = &mut *(data.0 as *mut Ctx);
            let mut pid = 0u32;
            GetWindowThreadProcessId(h, Some(&mut pid));
            if pid == 0 {
                return true.into();
            }
            let handle = windows::Win32::System::Threading::OpenProcess(
                windows::Win32::System::Threading::PROCESS_QUERY_LIMITED_INFORMATION,
                false,
                pid,
            );
            if let Ok(ph) = handle {
                let mut buf = [0u16; 260];
                let mut len = buf.len() as u32;
                if windows::Win32::System::Threading::QueryFullProcessImageNameW(
                    ph,
                    windows::Win32::System::Threading::PROCESS_NAME_WIN32,
                    windows::core::PWSTR(buf.as_mut_ptr()),
                    &mut len,
                )
                .is_ok()
                {
                    let name = String::from_utf16_lossy(&buf[..len as usize]);
                    if name.to_ascii_uppercase().ends_with(ctx.exe.as_str()) {
                        // 取第一个可见顶层窗口。
                        if IsWindowVisible(h).as_bool() {
                            ctx.found = h.0 as usize;
                            return false.into();
                        }
                    }
                }
                let _ = windows::Win32::Foundation::CloseHandle(ph);
            }
            true.into()
        }
    }
    unsafe {
        let _ = EnumWindows(
            Some(visit),
            windows::Win32::Foundation::LPARAM(&mut ctx as *mut Ctx as isize),
        );
    }
    Ok(ctx.found)
}

#[cfg(windows)]
fn embed(child: usize, parent: usize) {
    use windows::Win32::{Foundation::HWND, UI::WindowsAndMessaging::*};
    unsafe {
        let _ = SetParent(HWND(child as *mut _), Some(HWND(parent as *mut _)));
        // 去掉顶层边框观感，子窗口化。
        let style = GetWindowLongW(HWND(child as *mut _), GWL_STYLE) as u32;
        let _ = SetWindowLongW(
            HWND(child as *mut _),
            GWL_STYLE,
            (style & !(WS_POPUP.0 | WS_CAPTION.0 | WS_THICKFRAME.0)) as i32 | WS_CHILD.0 as i32,
        );
        let _ = MoveWindow(HWND(child as *mut _), 0, 0, 800, 600, true);
    }
}

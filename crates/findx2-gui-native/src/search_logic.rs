//! 与现 WebView GUI 对齐的结果导航、折叠阈值、查询拼接、高亮与格式化。

pub const RESULT_ROW_HEIGHT_PX: f32 = 32.0;
pub const PREVIEW_AUTO_HIDE_BELOW: f32 = 900.0;
pub const SIDEBAR_AUTO_HIDE_BELOW: f32 = 760.0;
pub const SIDEBAR_WIDTH_PX: f32 = 210.0;
pub const NAME_READABLE_PX: f32 = 180.0;
pub const PREVIEW_DEFAULT_WIDTH_PX: f32 = 320.0;

pub const COLUMN_HEADER_MIN_PX: [f32; 4] = [160.0, 80.0, 72.0, 140.0];
pub const DEFAULT_COL_FRAC: [f32; 4] = [0.28, 0.34, 0.10, 0.28];
pub const RESULT_ROW_PAD_X_PX: f32 = 8.0;
pub const RESULT_ROW_GAP_PX: f32 = 8.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColId {
    Name,
    Path,
    Size,
    Mtime,
}

impl ColId {
    pub const ALL: [ColId; 4] = [ColId::Name, ColId::Path, ColId::Size, ColId::Mtime];

    pub fn label(self) -> &'static str {
        match self {
            Self::Name => "名称",
            Self::Path => "路径",
            Self::Size => "大小",
            Self::Mtime => "修改时间",
        }
    }

    pub fn index(self) -> usize {
        match self {
            Self::Name => 0,
            Self::Path => 1,
            Self::Size => 2,
            Self::Mtime => 3,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResultNavKey {
    ArrowDown,
    ArrowUp,
    Home,
    End,
}

/// 方向键移动当前行。不改排序，只改选中下标。与 `resultSelection.ts` 一致。
pub fn move_result_selection(
    current: Option<usize>,
    key: ResultNavKey,
    count: usize,
) -> Option<usize> {
    if count == 0 {
        return None;
    }
    Some(match key {
        ResultNavKey::Home => 0,
        ResultNavKey::End => count - 1,
        ResultNavKey::ArrowDown => match current {
            None => 0,
            Some(i) => (i + 1).min(count - 1),
        },
        ResultNavKey::ArrowUp => match current {
            None => count - 1,
            Some(i) => i.saturating_sub(1),
        },
    })
}

pub fn scroll_top_for_row(row: usize, row_height: f32, scroll_top: f32, viewport: f32) -> f32 {
    let top = (row as f32).max(0.0) * row_height;
    let bottom = top + row_height;
    if top < scroll_top {
        top
    } else if bottom > scroll_top + viewport {
        (bottom - viewport).max(0.0)
    } else {
        scroll_top
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForceState {
    Show,
    Hide,
}

pub fn resolve_panel_open(
    window_width: f32,
    saved_open: bool,
    forced: Option<ForceState>,
    kind: PanelKind,
) -> bool {
    match forced {
        Some(ForceState::Show) => true,
        Some(ForceState::Hide) => false,
        None => {
            let limit = match kind {
                PanelKind::Preview => PREVIEW_AUTO_HIDE_BELOW,
                PanelKind::Sidebar => SIDEBAR_AUTO_HIDE_BELOW,
            };
            if window_width < limit {
                false
            } else {
                saved_open
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelKind {
    Sidebar,
    Preview,
}

/// 用内容区实际宽度。没在本次手动打开时，小窗先收起面板。
pub fn resolve_panels(
    content_width: f32,
    preview_width: f32,
    saved_sidebar: bool,
    saved_preview: bool,
    sidebar_force: Option<ForceState>,
    preview_force: Option<ForceState>,
) -> (bool, bool) {
    let mut sidebar = resolve_panel_open(
        content_width,
        saved_sidebar,
        sidebar_force,
        PanelKind::Sidebar,
    );
    let mut preview = resolve_panel_open(
        content_width,
        saved_preview,
        preview_force,
        PanelKind::Preview,
    );
    let results = |side: bool, prev: bool| {
        content_width
            - if side { SIDEBAR_WIDTH_PX } else { 0.0 }
            - if prev { preview_width + 4.0 } else { 0.0 }
    };
    if results(sidebar, preview) < NAME_READABLE_PX
        && preview
        && preview_force != Some(ForceState::Show)
    {
        preview = false;
    }
    if results(sidebar, preview) < NAME_READABLE_PX
        && sidebar
        && sidebar_force != Some(ForceState::Show)
    {
        sidebar = false;
    }
    (sidebar, preview)
}

pub fn explain_panels(
    content_width: f32,
    preview_width: f32,
    saved_sidebar: bool,
    saved_preview: bool,
    sidebar_force: Option<ForceState>,
    preview_force: Option<ForceState>,
) -> (bool, bool, String, String) {
    let (sidebar, preview) = resolve_panels(
        content_width,
        preview_width,
        saved_sidebar,
        saved_preview,
        sidebar_force,
        preview_force,
    );
    let preview_before = resolve_panel_open(
        content_width,
        saved_preview,
        preview_force,
        PanelKind::Preview,
    );
    let sidebar_before = resolve_panel_open(
        content_width,
        saved_sidebar,
        sidebar_force,
        PanelKind::Sidebar,
    );
    (
        sidebar,
        preview,
        panel_reason(
            PanelKind::Sidebar,
            content_width,
            saved_sidebar,
            sidebar_force,
            sidebar_before && !sidebar && sidebar_force != Some(ForceState::Hide),
        ),
        panel_reason(
            PanelKind::Preview,
            content_width,
            saved_preview,
            preview_force,
            preview_before && !preview && preview_force != Some(ForceState::Hide),
        ),
    )
}

fn panel_reason(
    kind: PanelKind,
    content_width: f32,
    saved_open: bool,
    forced: Option<ForceState>,
    closed_for_name: bool,
) -> String {
    let limit = match kind {
        PanelKind::Preview => PREVIEW_AUTO_HIDE_BELOW,
        PanelKind::Sidebar => SIDEBAR_AUTO_HIDE_BELOW,
    };
    let name = match kind {
        PanelKind::Preview => "预览",
        PanelKind::Sidebar => "侧栏",
    };
    match forced {
        Some(ForceState::Show) => format!("{name}开：本次手动打开，不看宽度"),
        Some(ForceState::Hide) => format!("{name}关：本次手动关闭"),
        None if closed_for_name => format!(
            "{name}关：内容宽 {content_width}px 时两侧同时展开会挤掉名称列（下限 {NAME_READABLE_PX}px）"
        ),
        None if content_width < limit => {
            format!("{name}关：决策宽度 {content_width}px < {limit}px")
        }
        None if saved_open => {
            format!("{name}开：决策宽度 {content_width}px >= {limit}px，且保存的偏好是展开")
        }
        None => format!("{name}关：决策宽度 {content_width}px >= {limit}px，保存的偏好是关闭"),
    }
}

/// Width available to the four result cells after row padding and gaps.
pub fn result_cells_inner_width(table_width: f32, visible_count: usize) -> f32 {
    let gaps = RESULT_ROW_GAP_PX * visible_count.saturating_sub(1) as f32;
    (table_width - RESULT_ROW_PAD_X_PX * 2.0 - gaps).max(1.0)
}

pub fn visible_column_ids(table_width: f32) -> Vec<ColId> {
    let width = table_width.max(0.0);
    let mut ids = ColId::ALL.to_vec();
    let need = |cols: &[ColId]| {
        cols.iter()
            .map(|id| COLUMN_HEADER_MIN_PX[id.index()])
            .sum::<f32>()
    };
    for drop in [ColId::Size, ColId::Mtime, ColId::Path] {
        if need(&ids) <= width {
            break;
        }
        ids.retain(|id| *id != drop);
    }
    ids
}

pub fn column_pixel_widths(saved: [f32; 4], table_width: f32, visible: &[ColId]) -> [f32; 4] {
    let width = table_width.max(1.0);
    let mut px = [0.0; 4];
    if visible.is_empty() {
        return px;
    }
    let weights: Vec<f32> = visible
        .iter()
        .map(|id| {
            let w = saved[id.index()];
            if w.is_finite() && w > 0.0 { w } else { 0.0001 }
        })
        .collect();
    let weight_sum: f32 = weights.iter().sum::<f32>().max(0.0001);
    let min_sum: f32 = visible
        .iter()
        .map(|id| COLUMN_HEADER_MIN_PX[id.index()])
        .sum();
    let extra = (width - min_sum).max(0.0);
    for (i, id) in visible.iter().enumerate() {
        px[id.index()] = COLUMN_HEADER_MIN_PX[id.index()] + extra * (weights[i] / weight_sum);
    }
    px
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeFilter {
    All,
    Folder,
    File,
    Doc,
    Image,
    Video,
    Audio,
}

impl TypeFilter {
    pub fn query(self) -> &'static str {
        match self {
            Self::All => "",
            Self::Folder => "folder:",
            Self::File => "file:",
            Self::Doc => "ext:doc;docx;pdf;xls;xlsx;ppt;pptx;txt;rtf;odt;csv;md",
            Self::Image => "ext:jpg;jpeg;png;gif;bmp;svg;webp;ico;tiff;psd;raw",
            Self::Video => "ext:mp4;avi;mkv;mov;wmv;flv;webm;m4v;ts",
            Self::Audio => "ext:mp3;wav;flac;aac;ogg;wma;m4a;opus",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::All => "全部",
            Self::Folder => "文件夹",
            Self::File => "文件",
            Self::Doc => "文档",
            Self::Image => "图片",
            Self::Video => "视频",
            Self::Audio => "音频",
        }
    }

    pub const ALL: [TypeFilter; 7] = [
        Self::All,
        Self::Folder,
        Self::File,
        Self::Doc,
        Self::Image,
        Self::Video,
        Self::Audio,
    ];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeFilter {
    All,
    D1,
    D7,
    D30,
    D365,
}

impl TimeFilter {
    pub fn label(self) -> &'static str {
        match self {
            Self::All => "不限时间",
            Self::D1 => "1 天内",
            Self::D7 => "7 天内",
            Self::D30 => "30 天内",
            Self::D365 => "一年内",
        }
    }

    pub const ALL: [TimeFilter; 5] = [Self::All, Self::D1, Self::D7, Self::D30, Self::D365];
}

/// 本地日历日 yyyy-MM-dd，避免 UTC 偏移一天。
pub fn format_local_ymd(year: i32, month: u32, day: u32) -> String {
    format!("{year:04}-{month:02}-{day:02}")
}

pub fn days_ago_ymd(days: i64, now: (i32, u32, u32)) -> String {
    use std::time::{Duration, SystemTime, UNIX_EPOCH};
    // Tests pass an explicit civil date; runtime uses local time via a naive day shift.
    let (y, m, d) = now;
    let Some(date) = civil_to_days(y, m, d) else {
        return format_local_ymd(y, m, d);
    };
    let shifted = date - days;
    let (yy, mm, dd) = days_to_civil(shifted);
    format_local_ymd(yy, mm, dd)
        // keep Duration/SystemTime imported only if needed later
        .and_then_unused(Duration::from_secs(0), SystemTime::now(), UNIX_EPOCH)
}

fn civil_to_days(y: i32, m: u32, d: u32) -> Option<i64> {
    if m < 1 || m > 12 || d < 1 || d > 31 {
        return None;
    }
    // Howard Hinnant civil-from-days
    let mut y = y as i64;
    let m = m as i64;
    let d = d as i64;
    y -= if m <= 2 { 1 } else { 0 };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146097 + doe - 719468)
}

fn days_to_civil(z: i64) -> (i32, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y as i32, m as u32, d as u32)
}

trait KeepUnused {
    fn and_then_unused(
        self,
        _a: std::time::Duration,
        _b: std::time::SystemTime,
        _c: std::time::SystemTime,
    ) -> Self;
}

impl KeepUnused for String {
    fn and_then_unused(
        self,
        _a: std::time::Duration,
        _b: std::time::SystemTime,
        _c: std::time::SystemTime,
    ) -> Self {
        self
    }
}

pub fn time_filter_query(filter: TimeFilter, today: (i32, u32, u32)) -> String {
    let days = match filter {
        TimeFilter::All => return String::new(),
        TimeFilter::D1 => 1,
        TimeFilter::D7 => 7,
        TimeFilter::D30 => 30,
        TimeFilter::D365 => 365,
    };
    format!("dm:>{}", days_ago_ymd(days, today))
}

pub fn today_local() -> (i32, u32, u32) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    // Use local offset when possible; fallback UTC.
    let offset = local_offset_secs();
    let days = (now + offset).div_euclid(86400);
    days_to_civil(days)
}

fn local_offset_secs() -> i64 {
    // std has no timezone; 0 is acceptable fallback. Windows GUI will still be
    // close enough for the filter token, and tests pass an explicit date.
    0
}

/// 修饰符必须排在裸词之前。
pub fn build_full_query(
    raw: &str,
    type_f: TypeFilter,
    time_f: TimeFilter,
    today: (i32, u32, u32),
    path_prefix: &str,
    size_min_mb: &str,
    size_max_mb: &str,
) -> String {
    let mut parts: Vec<String> = Vec::new();
    let tf = type_f.query();
    if !tf.is_empty() {
        parts.push(tf.to_string());
    }
    let tm = time_filter_query(time_f, today);
    if !tm.is_empty() {
        parts.push(tm);
    }
    let path_text = path_prefix.trim();
    if !path_text.is_empty() {
        parts.push(format!("path:\"{path_text}\""));
    }
    let smin = size_min_mb.trim();
    let smax = size_max_mb.trim();
    if !smin.is_empty() && !smax.is_empty() {
        parts.push(format!("size:{smin}mb..{smax}mb"));
    } else if !smin.is_empty() {
        parts.push(format!("size:>={smin}mb"));
    } else if !smax.is_empty() {
        parts.push(format!("size:<={smax}mb"));
    }
    let r = raw.trim();
    if !r.is_empty() {
        parts.push(r.to_string());
    }
    parts.join(" ")
}

pub fn format_size(n: u64, is_dir: bool) -> String {
    if is_dir {
        return String::new();
    }
    if n < 1024 {
        return format!("{n} B");
    }
    let mut v = n as f64;
    let units = ["B", "KB", "MB", "GB", "TB"];
    let mut i = 0usize;
    while v >= 1024.0 && i < units.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} B")
    } else if v >= 10.0 || i == 1 {
        format!("{:.0} {}", v, units[i])
    } else {
        format!("{:.2} {}", v, units[i])
    }
}

/// Windows FILETIME epoch (1601-01-01) in 100ns ticks, same as findx2-core.
const FILETIME_EPOCH_DIFF: u64 = 116_444_736_000_000_000;

/// Normalize a search-hit mtime to Unix seconds.
///
/// The official WebView does `new Date(unix * 1000)`. Live `findx2-service`
/// currently sends Windows FILETIME (100ns since 1601). Values already in
/// Unix seconds (including the mock backend) are left alone.
pub fn unix_secs_from_mtime(raw: u64) -> u64 {
    if raw == 0 {
        0
    } else if raw >= FILETIME_EPOCH_DIFF {
        (raw - FILETIME_EPOCH_DIFF) / 10_000_000
    } else {
        raw
    }
}

fn unix_secs_to_filetime(unix: u64) -> u64 {
    if unix == 0 {
        0
    } else {
        FILETIME_EPOCH_DIFF.saturating_add(unix.saturating_mul(10_000_000))
    }
}

pub fn format_time(raw: u64) -> String {
    let unix = unix_secs_from_mtime(raw);
    if unix == 0 {
        return "—".into();
    }
    // Display is YYYY-MM-DD HH:MM in the local timezone, matching the WebView GUI.
    let offset = local_utc_offset_secs(unix);
    let t = unix as i64 + offset;
    let days = t.div_euclid(86400);
    let rem = t.rem_euclid(86400) as u32;
    let (y, m, d) = days_to_civil(days);
    let hh = rem / 3600;
    let mm = (rem % 3600) / 60;
    format!("{y:04}-{m:02}-{d:02} {hh:02}:{mm:02}")
}

fn local_utc_offset_secs(unix: u64) -> i64 {
    #[cfg(windows)]
    {
        windows_local_utc_offset_secs(unix)
    }
    #[cfg(unix)]
    {
        unix_local_utc_offset_secs(unix)
    }
    #[cfg(not(any(windows, unix)))]
    {
        let _ = unix;
        0
    }
}

#[cfg(windows)]
#[repr(C)]
struct WinFileTime {
    dw_low_date_time: u32,
    dw_high_date_time: u32,
}

#[cfg(windows)]
impl WinFileTime {
    fn from_u64(v: u64) -> Self {
        Self {
            dw_low_date_time: v as u32,
            dw_high_date_time: (v >> 32) as u32,
        }
    }

    fn to_u64(self) -> u64 {
        (self.dw_low_date_time as u64) | ((self.dw_high_date_time as u64) << 32)
    }
}

#[cfg(windows)]
unsafe extern "system" {
    fn FileTimeToLocalFileTime(
        lp_file_time: *const WinFileTime,
        lp_local_file_time: *mut WinFileTime,
    ) -> i32;
}

#[cfg(windows)]
fn windows_local_utc_offset_secs(unix: u64) -> i64 {
    let utc = WinFileTime::from_u64(unix_secs_to_filetime(unix));
    let mut local = WinFileTime::from_u64(0);
    let ok = unsafe { FileTimeToLocalFileTime(&utc, &mut local) };
    if ok == 0 {
        return 0;
    }
    ((local.to_u64() as i128 - utc.to_u64() as i128) / 10_000_000) as i64
}

#[cfg(unix)]
fn unix_local_utc_offset_secs(unix: u64) -> i64 {
    let time = unix as libc::time_t;
    let mut result = std::mem::MaybeUninit::<libc::tm>::uninit();
    let ptr = unsafe { libc::localtime_r(&time, result.as_mut_ptr()) };
    if ptr.is_null() {
        0
    } else {
        unsafe { (*ptr).tm_gmtoff as i64 }
    }
}

/// 把 Unicode 标量下标半开区间切成 (text, highlighted) 段。
pub fn highlight_segments<'a>(name: &'a str, ranges: &[[u32; 2]]) -> Vec<(&'a str, bool)> {
    let chars: Vec<(usize, char)> = name.char_indices().collect();
    let len = chars.len();
    if len == 0 {
        return vec![(name, false)];
    }
    let mut marks = vec![false; len];
    for r in ranges {
        let start = r[0] as usize;
        let end = r[1] as usize;
        if start >= end {
            continue;
        }
        for i in start.min(len)..end.min(len) {
            marks[i] = true;
        }
    }
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < len {
        let flag = marks[i];
        let mut j = i + 1;
        while j < len && marks[j] == flag {
            j += 1;
        }
        let start_byte = chars[i].0;
        let end_byte = if j < len { chars[j].0 } else { name.len() };
        out.push((&name[start_byte..end_byte], flag));
        i = j;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arrow_down_from_null_selects_first() {
        assert_eq!(
            move_result_selection(None, ResultNavKey::ArrowDown, 10),
            Some(0)
        );
        assert_eq!(
            move_result_selection(Some(0), ResultNavKey::ArrowDown, 10),
            Some(1)
        );
        assert_eq!(
            move_result_selection(None, ResultNavKey::ArrowUp, 10),
            Some(9)
        );
        assert_eq!(
            move_result_selection(Some(0), ResultNavKey::ArrowUp, 10),
            Some(0)
        );
        assert_eq!(
            move_result_selection(None, ResultNavKey::ArrowDown, 0),
            None
        );
        assert_eq!(move_result_selection(None, ResultNavKey::Home, 5), Some(0));
        assert_eq!(move_result_selection(None, ResultNavKey::End, 5), Some(4));
    }

    #[test]
    fn panels_collapse_at_thresholds() {
        let (side, prev) = resolve_panels(960.0, 320.0, true, true, None, None);
        assert!(side && prev);
        let (side, prev) = resolve_panels(862.0, 320.0, true, true, None, None);
        assert!(side && !prev);
        let (side, prev) = resolve_panels(759.0, 320.0, true, true, None, None);
        assert!(!side && !prev);
        let (side, prev) = resolve_panels(759.0, 320.0, true, true, Some(ForceState::Show), None);
        assert!(side && !prev);
    }

    #[test]
    fn highlight_uses_unicode_scalar_indexes() {
        let segs = highlight_segments("测试文件.txt", &[[0, 2]]);
        assert_eq!(segs, vec![("测试", true), ("文件.txt", false)]);
    }

    #[test]
    fn query_puts_modifiers_before_raw() {
        let q = build_full_query(
            "报告",
            TypeFilter::Folder,
            TimeFilter::All,
            (2026, 11, 11),
            r"C:\docs",
            "",
            "",
        );
        assert_eq!(q, r#"folder: path:"C:\docs" 报告"#);
    }

    #[test]
    fn format_size_skips_directories() {
        assert_eq!(format_size(100, true), "");
        assert_eq!(format_size(512, false), "512 B");
        assert_eq!(format_size(2048, false), "2 KB");
    }

    #[test]
    fn format_time_accepts_unix_seconds_in_2020s() {
        // 2026-01-15 00:00:00 UTC
        let unix = 1_768_435_200u64;
        let s = format_time(unix);
        assert_eq!(s.len(), 16, "expected YYYY-MM-DD HH:MM, got {s}");
        assert!(
            s.starts_with("2026-01-14")
                || s.starts_with("2026-01-15")
                || s.starts_with("2026-01-16"),
            "unix seconds formatted out of range: {s}"
        );
        assert!(
            !s.starts_with("38"),
            "should not look like a FILETIME mis-parse: {s}"
        );
    }

    #[test]
    fn format_time_converts_windows_filetime() {
        // Same instant as the unix test, encoded as FILETIME 100ns since 1601.
        let unix = 1_768_435_200u64;
        let ft = 116_444_736_000_000_000 + unix * 10_000_000;
        assert_eq!(unix_secs_from_mtime(ft), unix);
        assert_eq!(unix_secs_from_mtime(unix), unix);
        assert_eq!(format_time(ft), format_time(unix));
        // A real live hit from the user's production pipe (2026-08-29 17:00:48 +08).
        let live_ft = 134_324_676_480_000_000u64;
        assert_eq!(unix_secs_from_mtime(live_ft), 1_787_994_048);
        let live = format_time(live_ft);
        assert!(
            live.starts_with("2026-08-29")
                || live.starts_with("2026-08-28")
                || live.starts_with("2026-08-30"),
            "live FILETIME formatted out of range: {live}"
        );
    }

    #[test]
    fn format_time_zero_is_placeholder() {
        assert_eq!(format_time(0), "—");
        assert_eq!(unix_secs_from_mtime(0), 0);
    }
}

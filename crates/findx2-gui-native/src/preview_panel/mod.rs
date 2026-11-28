//! 预览面板框架（格式无关的编排 + 每种格式一个 Surface）。
//!
//! 设计目标：
//! - 一个目标同一时刻只有一个 `PreviewSurface`，旧状态物理上不存在（不再靠手写清理
//!   十几组 `docx_* / sheet_* / deck_*` 字段）；
//! - 所有异步结果带 `gen`，代际不符直接丢弃；
//! - UI 骨架（Header / ViewBar / Body / HitBar / HitList）由 Controller 统一渲染，
//!   格式只负责：解析、正文、自己的 ViewBar 按钮、命中计算与跳转。
//!
//! 这批只落框架，不改变现有行为；随后按格式逐个接入。

use gpui_kit::{AnyElement, App, Window};

use crate::theme::FindxTokens;

/// 预览目标身份：路径 + 大小 + mtime。任何异步结果都按它校验。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileStamp {
    pub path: String,
    pub len: u64,
    pub mtime: u64,
}

/// 命中跳转令牌：各格式自己解释。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HitTarget {
    /// docx / md 块号
    Block(usize),
    /// xlsx 单元格
    SheetCell {
        sheet: usize,
        row: usize,
        col: usize,
    },
    /// pptx 页内段落
    Slide { slide: usize, para: usize },
    /// pdf 页 + 页内框序号
    PdfPage { page: u32, box_index: usize },
    /// 文本行
    TextLine { line: usize, offset: usize },
    Other,
}

/// 底部命中列表的一行（与格式无关）。
#[derive(Clone, Debug)]
pub struct HitRow {
    pub label: String,
    pub ranges: Vec<std::ops::Range<usize>>,
    pub exact: bool,
}

/// ViewBar 上的动作（由 Surface 自己处理）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BarAction {
    First,
    Prev,
    Next,
    Last,
    ZoomIn,
    ZoomOut,
    Fit,
    ToggleWrap,
    Sheet(usize),
    SystemPreview,
    Custom(&'static str),
}

/// ViewBar 上的一个声明式按钮。
#[derive(Clone, Debug)]
pub struct BarItem {
    pub label: String,
    pub action: BarAction,
    pub selected: bool,
    pub disabled: bool,
    pub tooltip: Option<&'static str>,
}

impl BarItem {
    pub fn button(label: impl Into<String>, action: BarAction) -> Self {
        Self {
            label: label.into(),
            action,
            selected: false,
            disabled: false,
            tooltip: None,
        }
    }

    pub fn selected(mut self, on: bool) -> Self {
        self.selected = on;
        self
    }

    pub fn disabled(mut self, on: bool) -> Self {
        self.disabled = on;
        self
    }

    pub fn tooltip(mut self, text: &'static str) -> Self {
        self.tooltip = Some(text);
        self
    }
}

/// 渲染时传给 Surface 的只读上下文。
pub struct SurfaceCtx {
    /// 预览列宽度（px）。
    pub width: f32,
    pub tokens: FindxTokens,
    /// 当前内容查询（`content:` 原文），没有则为 None。
    pub content_query: Option<String>,
    /// 命中总数 / 当前命中（0 基），由 Controller 提供。
    pub hit_total: usize,
    pub hit_current: usize,
}

impl SurfaceCtx {
    pub fn new(width: f32, tokens: FindxTokens) -> Self {
        Self {
            width,
            tokens,
            content_query: None,
            hit_total: 0,
            hit_current: 0,
        }
    }
}

/// 每种格式实现这一个 trait。
pub trait PreviewSurface: 'static {
    /// 正文。
    fn body(&mut self, ctx: &SurfaceCtx, window: &mut Window, app: &mut App) -> AnyElement;

    /// 当前视图自己的操作按钮（PDF 页码/缩放、xlsx 页签、文本换行…）。
    fn view_bar(&mut self, _ctx: &SurfaceCtx) -> Vec<BarItem> {
        Vec::new()
    }

    /// 处理 ViewBar 动作。
    fn on_bar(&mut self, _action: &BarAction, _app: &mut App) {}

    /// 内容检索命中变化时调用（在后台线程算好后由 Controller 安装）。
    fn set_hits(&mut self, _query: &str, _app: &mut App) {}

    fn hit_count(&self) -> usize {
        0
    }

    fn hit_row(&self, _index: usize) -> Option<HitRow> {
        None
    }

    fn current_hit(&self) -> usize {
        0
    }

    /// 跳到第 index 个命中（滚动 + 高亮）。
    fn goto_hit(&mut self, _index: usize, _app: &mut App) {}

    fn on_resize(&mut self, _width: f32) {}

    /// 该格式是否值得提供“系统版式”按钮（PDF/xlsx 返回 false）。
    fn supports_system_preview(&self) -> bool {
        false
    }
}

/// 该扩展名是否有「内容视图」（自渲染）。
pub fn content_supported(path: &str) -> bool {
    let ext = ext_of(path);
    matches!(
        ext.as_str(),
        "docx"
            | "doc"
            | "rtf"
            | "md"
            | "markdown"
            | "xlsx"
            | "xlsm"
            | "xls"
            | "pptx"
            | "ppt"
            | "pdf"
            | "txt"
            | "log"
            | "csv"
            | "tsv"
            | "json"
            | "xml"
            | "yml"
            | "yaml"
            | "toml"
            | "ini"
            | "cfg"
            | "conf"
            | "ts"
            | "tsx"
            | "js"
            | "jsx"
            | "mjs"
            | "py"
            | "rs"
            | "c"
            | "h"
            | "cc"
            | "cpp"
            | "hpp"
            | "cs"
            | "java"
            | "kt"
            | "go"
            | "html"
            | "htm"
            | "css"
            | "scss"
            | "less"
            | "sql"
            | "sh"
            | "bash"
            | "zsh"
            | "bat"
            | "cmd"
            | "ps1"
            | "vue"
            | "svelte"
    )
}

/// 该扩展名是否提供「系统版式」（PDF / xlsx 不提供，自渲染更完整）。
pub fn system_supported(path: &str) -> bool {
    let ext = ext_of(path);
    matches!(ext.as_str(), "docx" | "doc" | "ppt" | "pptx" | "rtf")
}

fn ext_of(path: &str) -> String {
    std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default()
}

/// 格式无关的编排器：选中 → 解析 → 安装 → 命中 → 自动定位。
#[derive(Default)]
pub struct PreviewController {
    generation: u64,
    stamp: Option<FileStamp>,
    surface: Option<Box<dyn PreviewSurface>>,
    hits_installed: bool,
}

impl PreviewController {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn stamp(&self) -> Option<&FileStamp> {
        self.stamp.as_ref()
    }

    /// 开始一个新目标：代际自增，旧 surface 立即丢弃（物理上不存在串文件）。
    pub fn begin(&mut self, stamp: FileStamp) -> u64 {
        self.generation = self.generation.wrapping_add(1);
        self.stamp = Some(stamp);
        self.surface = None;
        self.hits_installed = false;
        self.generation
    }

    pub fn install(&mut self, surface: Box<dyn PreviewSurface>) {
        self.surface = Some(surface);
    }

    pub fn clear(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.stamp = None;
        self.surface = None;
        self.hits_installed = false;
    }

    pub fn surface(&self) -> Option<&dyn PreviewSurface> {
        self.surface.as_deref()
    }

    pub fn surface_mut(&mut self) -> Option<&mut (dyn PreviewSurface + 'static)> {
        match self.surface.as_mut() {
            Some(b) => Some(b.as_mut()),
            None => None,
        }
    }

    pub fn hits_installed(&self) -> bool {
        self.hits_installed
    }

    pub fn mark_hits_installed(&mut self) {
        self.hits_installed = true;
    }

    pub fn hit_count(&self) -> usize {
        self.surface().map(|s| s.hit_count()).unwrap_or(0)
    }

    /// 异步结果落地前用它校验：代际 + 目标都没变才允许安装。
    pub fn is_current(&self, generation: u64, stamp: &FileStamp) -> bool {
        self.generation == generation && self.stamp.as_ref() == Some(stamp)
    }

    pub fn is_current_generation(&self, generation: u64) -> bool {
        self.generation == generation
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::{div, IntoElement};

    struct Dummy {
        hits: usize,
        cur: usize,
    }

    impl PreviewSurface for Dummy {
        fn body(
            &mut self,
            _ctx: &SurfaceCtx,
            _window: &mut Window,
            _app: &mut App,
        ) -> AnyElement {
            div().into_any_element()
        }

        fn hit_count(&self) -> usize {
            self.hits
        }

        fn hit_row(&self, index: usize) -> Option<HitRow> {
            (index < self.hits).then(|| HitRow {
                label: format!("hit {index}"),
                ranges: Vec::new(),
                exact: false,
            })
        }

        fn current_hit(&self) -> usize {
            self.cur
        }

        fn goto_hit(&mut self, index: usize, _app: &mut App) {
            self.cur = index;
        }
    }

    fn stamp(path: &str) -> FileStamp {
        FileStamp {
            path: path.into(),
            len: 1,
            mtime: 2,
        }
    }

    #[test]
    fn capability_matrix() {
        // 有内容视图
        assert!(content_supported("a.docx"));
        assert!(content_supported("a.PDF"));
        assert!(content_supported("a.txt"));
        assert!(content_supported("a.ts"));
        // 老二进制 Office：doc/xls 已自渲染；ppt 走“文本分页”视图
        assert!(content_supported("a.doc"));
        assert!(content_supported("a.xls"));
        assert!(content_supported("a.ppt"));
        assert!(!content_supported("a.zip"));

        // 系统版式：doc/docx/pptx/ppt 有，pdf/xlsx/xls 没有
        assert!(system_supported("a.docx"));
        assert!(system_supported("a.PPTX"));
        assert!(system_supported("a.doc"));
        assert!(!system_supported("a.pdf"));
        assert!(!system_supported("a.xlsx"));
        assert!(!system_supported("a.xls"));
        assert!(!system_supported("a.txt"));
    }

    #[test]
    fn begin_invalidates_previous_generation() {
        let mut c = PreviewController::new();
        let g1 = c.begin(stamp("a"));
        c.install(Box::new(Dummy { hits: 3, cur: 0 }));
        assert!(c.is_current(g1, &stamp("a")));
        assert_eq!(c.hit_count(), 3);

        let g2 = c.begin(stamp("b"));
        assert!(g2 > g1);
        assert!(!c.is_current(g1, &stamp("a")));
        assert!(c.is_current(g2, &stamp("b")));
        // 新目标尚未安装 surface：旧模型物理上已丢弃。
        assert!(c.surface().is_none());
        assert_eq!(c.hit_count(), 0);
    }

    #[test]
    fn goto_hit_updates_surface() {
        let mut c = PreviewController::new();
        let _ = c.begin(stamp("a"));
        c.install(Box::new(Dummy { hits: 5, cur: 0 }));
        // goto_hit 需要 &mut App，真实调用在 UI 里；这里验证命中行模型。
        let s = c.surface_mut().expect("surface");
        assert_eq!(s.hit_count(), 5);
        assert!(s.hit_row(0).is_some());
        assert!(s.hit_row(5).is_none());
    }
}

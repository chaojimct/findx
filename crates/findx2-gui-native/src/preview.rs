//! 选中项的内容预览。文件系统 I/O 与解码只允许在后台任务中调用。
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui_kit::RenderImage;
use image::imageops::FilterType;
use image::{GenericImageView, ImageReader, RgbaImage};

use crate::search_logic::{format_size, format_time};

pub const MAX_IMAGE_BYTES: u64 = 24 * 1024 * 1024;
pub const MAX_IMAGE_EDGE: u32 = 2048;
pub const MAX_TEXT_BYTES: usize = 256 * 1024;
pub const MAX_TEXT_LINES: usize = 2000;
pub const MAX_DIR_ENTRIES: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviewClass {
    Empty,
    Directory,
    Image,
    Text,
    Binary,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviewKind {
    Empty,
    Loading,
    Image,
    NativeOffice,
    Text,
    Directory,
    Unsupported,
    Error,
}

#[derive(Clone, Debug)]
pub struct PreviewMeta {
    pub name: String,
    pub path: String,
    pub size: u64,
    pub mtime: u64,
    pub is_directory: bool,
    pub kind_label: String,
}

#[derive(Clone, Debug)]
pub struct DirEntryPreview {
    pub name: String,
    pub is_dir: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct PdfMeta {
    /// Page represented by this raster, not the page merely requested by the toolbar.
    pub page: u32,
    pub count: u32,
    pub width: f32,
    pub height: f32,
}

pub struct PreviewState {
    #[cfg(windows)]
    pub native: Option<crate::office_host::NativeClient>,
    pub pdf: Option<PdfMeta>,
    /// 自渲染 PDF 页的命中框（点坐标，左上原点；与 `pdf` 同页同文件才展示）。
    pub pdf_highlights: Vec<[f32; 4]>,
    pub pdf_hl_page: Option<u32>,
    pub pdf_hl_path: Option<String>,
    pub generation: u64,
    pub meta: Option<PreviewMeta>,
    pub kind: PreviewKind,
    pub image: Option<Arc<RenderImage>>,
    pub image_width: u32,
    pub image_height: u32,
    pub image_note: Option<String>,
    pub text: Option<String>,
    pub text_encoding: Option<String>,
    pub truncated: bool,
    pub dir_entries: Vec<DirEntryPreview>,
    pub dir_total: usize,
    pub message: Option<String>,
}

impl PreviewState {
    pub fn pdf_frame_matches(&self, path: &str, page: u32) -> bool {
        self.meta.as_ref().is_some_and(|m| m.path == path)
            && self.pdf.is_some_and(|pdf| pdf.page == page)
    }

    pub fn empty() -> Self {
        Self {
            #[cfg(windows)]
            native: None,
            pdf: None,
            pdf_highlights: Vec::new(),
            pdf_hl_page: None,
            pdf_hl_path: None,
            generation: 0,
            meta: None,
            kind: PreviewKind::Empty,
            image: None,
            image_width: 0,
            image_height: 0,
            image_note: None,
            text: None,
            text_encoding: None,
            truncated: false,
            dir_entries: Vec::new(),
            dir_total: 0,
            message: None,
        }
    }

    pub fn loading(generation: u64, meta: PreviewMeta) -> Self {
        Self {
            #[cfg(windows)]
            native: None,
            pdf: None,
            pdf_highlights: Vec::new(),
            pdf_hl_page: None,
            pdf_hl_path: None,
            generation,
            meta: Some(meta),
            kind: PreviewKind::Loading,
            image: None,
            image_width: 0,
            image_height: 0,
            image_note: None,
            text: None,
            text_encoding: None,
            truncated: false,
            dir_entries: Vec::new(),
            dir_total: 0,
            message: Some("正在加载预览…".into()),
        }
    }
}

/// Both query/selection epoch and the payload epoch must still match.
pub fn is_current_generation(current: u64, requested: u64, loaded: u64) -> bool {
    current == requested && requested == loaded
}

pub fn meta_from_hit(
    name: &str,
    path: &str,
    size: u64,
    mtime: u64,
    is_directory: bool,
) -> PreviewMeta {
    PreviewMeta {
        name: name.to_string(),
        path: path.to_string(),
        size,
        mtime,
        is_directory,
        kind_label: class_label(classify_path(path, is_directory)),
    }
}

pub fn class_label(class: PreviewClass) -> String {
    match class {
        PreviewClass::Empty => String::new(),
        PreviewClass::Directory => "文件夹".into(),
        PreviewClass::Image => "图片".into(),
        PreviewClass::Text => "文本".into(),
        PreviewClass::Binary => "文件".into(),
    }
}

pub fn classify_path(path: &str, is_directory: bool) -> PreviewClass {
    if path.trim().is_empty() {
        return PreviewClass::Empty;
    }
    if is_directory {
        return PreviewClass::Directory;
    }
    let ext = Path::new(path)
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if is_image_ext(&ext) {
        PreviewClass::Image
    } else if is_text_ext(&ext) {
        PreviewClass::Text
    } else if is_known_binary_ext(&ext) {
        PreviewClass::Binary
    } else {
        // Extensionless and unknown extensions are decided from a bounded prefix.
        PreviewClass::Binary
    }
}

pub fn needs_content_sniff(ext: &str) -> bool {
    !(is_image_ext(ext) || is_text_ext(ext) || is_known_binary_ext(ext))
}

pub fn is_known_binary_ext(ext: &str) -> bool {
    matches!(
        ext,
        "exe"
            | "dll"
            | "sys"
            | "msi"
            | "com"
            | "scr"
            | "ocx"
            | "cpl"
            | "drv"
            | "zip"
            | "7z"
            | "rar"
            | "tar"
            | "gz"
            | "bz2"
            | "xz"
            | "tgz"
            | "cab"
            | "iso"
            | "img"
            | "mp3"
            | "mp4"
            | "avi"
            | "mkv"
            | "mov"
            | "flac"
            | "wav"
            | "wma"
            | "wmv"
            | "aac"
            | "ogg"
            | "webm"
            | "m4a"
            | "m4v"
            | "mpg"
            | "mpeg"
            | "db"
            | "sqlite"
            | "sqlite3"
            | "mdb"
            | "accdb"
            | "pdf"
            | "doc"
            | "xls"
            | "ppt"
            | "docx"
            | "xlsx"
            | "pptx"
            | "rtf"
            | "ttf"
            | "otf"
            | "woff"
            | "woff2"
            | "class"
            | "pyc"
            | "o"
            | "obj"
            | "lib"
            | "so"
            | "dylib"
            | "wasm"
            | "lnk"
            | "pdb"
            | "ilk"
            | "bin"
    )
}

pub const SNIFF_BYTES: usize = 8192;

/// Empty files and readable text prefixes are text. NUL and dense control bytes are binary.
/// UTF-16 BOM is text even though it contains NUL.
pub fn sniff_text_prefix(bytes: &[u8]) -> bool {
    if bytes.is_empty() {
        return true;
    }
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF])
        || bytes.starts_with(&[0xFF, 0xFE])
        || bytes.starts_with(&[0xFE, 0xFF])
    {
        return true;
    }
    if looks_binary(bytes) {
        return false;
    }
    let sample = &bytes[..bytes.len().min(1024)];
    let controls = sample
        .iter()
        .filter(|b| **b < 0x20 && !matches!(**b, b'\t' | b'\n' | b'\r' | 0x0c))
        .count();
    if sample.len() >= 8 && controls * 20 > sample.len() {
        return false;
    }
    if std::str::from_utf8(sample).is_ok() {
        return true;
    }
    let (_, _, had_errors) = encoding_rs::GBK.decode(sample);
    !had_errors
}

pub fn is_image_ext(ext: &str) -> bool {
    matches!(
        ext,
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "ico" | "tif" | "tiff"
    )
}

pub fn is_text_ext(ext: &str) -> bool {
    matches!(
        ext,
        "txt"
            | "md"
            | "markdown"
            | "json"
            | "jsonc"
            | "rs"
            | "toml"
            | "xml"
            | "html"
            | "htm"
            | "css"
            | "scss"
            | "js"
            | "jsx"
            | "ts"
            | "tsx"
            | "py"
            | "go"
            | "java"
            | "kt"
            | "c"
            | "h"
            | "cpp"
            | "hpp"
            | "cc"
            | "cs"
            | "sh"
            | "bash"
            | "zsh"
            | "bat"
            | "cmd"
            | "ps1"
            | "yaml"
            | "yml"
            | "ini"
            | "cfg"
            | "conf"
            | "log"
            | "csv"
            | "tsv"
            | "svg"
            | "gitignore"
            | "gitattributes"
            | "env"
            | "lock"
            | "sql"
            | "rb"
            | "php"
            | "lua"
            | "r"
            | "swift"
            | "gradle"
            | "properties"
            | "editorconfig"
            | "dockerfile"
    )
}

pub fn format_meta_line(meta: &PreviewMeta) -> String {
    let mut parts = Vec::new();
    if !meta.kind_label.is_empty() {
        parts.push(meta.kind_label.clone());
    }
    let size = format_size(meta.size, meta.is_directory);
    if !size.is_empty() {
        parts.push(size);
    }
    if meta.mtime != 0 {
        parts.push(format_time(meta.mtime));
    }
    parts.join("  ")
}

pub fn io_error_message(err: &std::io::Error, path: &str) -> String {
    match err.kind() {
        std::io::ErrorKind::NotFound => format!("找不到文件：{path}"),
        std::io::ErrorKind::PermissionDenied => format!("没有权限读取：{path}"),
        _ => format!("无法读取预览：{err}"),
    }
}

/// 在后台线程加载预览。不要从 UI 线程调用。
pub fn load_preview(
    generation: u64,
    name: &str,
    path: &str,
    size: u64,
    mtime: u64,
    is_directory: bool,
) -> PreviewState {
    // Sync wrapper for tests; the UI always awaits the async version.
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("preview test runtime")
        .block_on(load_preview_with_options(
            generation,
            name,
            path,
            size,
            mtime,
            is_directory,
            crate::preview_worker::PdfOptions::default(),
            false,
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        ))
}

pub async fn load_preview_with_options(
    generation: u64,
    name: &str,
    path: &str,
    size: u64,
    mtime: u64,
    is_directory: bool,
    options: crate::preview_worker::PdfOptions,
    prefer_native: bool,
    cancel: crate::preview_worker::Cancel,
) -> PreviewState {
    let class = classify_path(path, is_directory);
    let mut state = PreviewState::loading(
        generation,
        meta_from_hit(name, path, size, mtime, is_directory),
    );
    if path.trim().is_empty() {
        state.kind = PreviewKind::Empty;
        state.message = None;
        return state;
    }
    let path_buf = PathBuf::from(path);
    let ext = path_buf
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let office = matches!(
        ext.as_str(),
        "docx" | "xlsx" | "pptx" | "rtf" | "doc" | "xls" | "ppt"
    );
    let mut native_error = None;
    if !is_directory && office && prefer_native {
        #[cfg(windows)]
        match crate::office_host::start(path, cancel.clone()) {
            Ok(client) => {
                state.native = Some(client);
                state.kind = PreviewKind::NativeOffice;
                state.message = Some("系统预览处理器，实际版式由已安装应用提供".into());
                return state;
            }
            Err(e) => native_error = Some(e),
        }
    }
    // PDF优先走已安装的系统预览处理器(与Office文档同一条路):
    // 处理器进程常驻、文档只解析一次、原生滚动;不再为每页起新进程、
    // 重解析全文。WinRT光栅只在没有可用处理器时兜底。
    if !is_directory && ext == "pdf" && prefer_native {
        #[cfg(windows)]
        match crate::office_host::start(path, cancel.clone()) {
            Ok(client) => {
                state.native = Some(client);
                state.kind = PreviewKind::NativeOffice;
                state.message = Some("系统预览处理器已接管，可直接滚动浏览".into());
                return state;
            }
            Err(e) => native_error = Some(e),
        }
    }
    if matches!(ext.as_str(), "doc" | "xls" | "ppt") {
        state.kind = PreviewKind::Unsupported;
        state.message = Some(native_error.unwrap_or_else(|| {
            "旧二进制 Office 格式需要可用的系统预览处理器；没有文字摘录后备".into()
        }));
        return state;
    }
    if !is_directory && crate::document_preview::supported(&ext) {
        let request = if ext == "pdf" {
            crate::preview_worker::Request::Pdf {
                path: path.to_owned(),
                page: options.page,
                edge: if options.fit_width {
                    1600
                } else {
                    (1600. * options.zoom).clamp(400., 3072.) as u32
                },
            }
        } else {
            crate::preview_worker::Request::Text {
                path: path.to_owned(),
                ext: ext.clone(),
            }
        };
        match crate::preview_worker::run(request, cancel, std::time::Duration::from_secs(10)).await {
            Ok(crate::preview_worker::Output::Pdf {
                png,
                count,
                width,
                height,
            }) => match decode_image_bytes(&png, png.len() as u64) {
                Ok(image) => {
                    apply_image(&mut state, image);
                    // The raster is a derived PNG, not the source document's file size.
                    if let Some(meta) = state.meta.as_mut() {
                        meta.size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(size);
                    }
                    state.pdf = Some(PdfMeta {
                        page: options.page.min(count.saturating_sub(1)),
                        count,
                        width,
                        height,
                    });
                    state.image_note = Some(format!(
                        "PDF 第 {} / {count} 页",
                        options.page.min(count.saturating_sub(1)) + 1
                    ));
                }
                Err(e) => apply_error(&mut state, e),
            },
            Ok(crate::preview_worker::Output::Text(text)) => {
                state.kind = PreviewKind::Text;
                state.text = Some(if text.trim().is_empty() {
                    "未提取到文本（可能只有图片）".into()
                } else {
                    text
                });
                state.text_encoding = Some(if ext == "rtf" {
                    "RTF 文本摘录，非版式预览".into()
                } else {
                    "OOXML 文本摘录，非版式预览；不执行宏/外链/公式".into()
                });
                state.message = None;
                if let Some(reason) = native_error {
                    state.text_encoding = Some(format!(
                        "系统版式不可用：{reason}\n已降级为文字摘录（不保留完整排版）"
                    ));
                }
            }
            Err(e) => apply_error(&mut state, e),
        }
        return state;
    }
    match class {
        PreviewClass::Empty => {
            state.kind = PreviewKind::Empty;
            state.message = None;
        }
        PreviewClass::Directory => match load_directory(&path_buf) {
            Ok((entries, total, truncated)) => {
                state.kind = PreviewKind::Directory;
                state.dir_entries = entries;
                state.dir_total = total;
                state.truncated = truncated;
                state.message = None;
            }
            Err(err) => apply_error(&mut state, err),
        },
        PreviewClass::Image => match load_image(&path_buf) {
            Ok(image) => apply_image(&mut state, image),
            Err(err) => apply_error(&mut state, err),
        },
        PreviewClass::Text => match load_text(&path_buf) {
            Ok(text) => apply_text(&mut state, text),
            Err(err) => apply_error(&mut state, err),
        },
        PreviewClass::Binary if needs_content_sniff(&ext) => match sniff_regular_file(&path_buf) {
            Ok(true) => match load_text(&path_buf) {
                Ok(text) => {
                    if let Some(meta) = state.meta.as_mut() {
                        meta.kind_label = "文本".into();
                    }
                    apply_text(&mut state, text);
                }
                Err(err) => apply_error(&mut state, err),
            },
            Ok(false) => {
                state.kind = PreviewKind::Unsupported;
                state.message = Some("暂不支持此格式的内容预览".into());
            }
            Err(err) => apply_error(&mut state, err),
        },
        PreviewClass::Binary => match file_len_or_size(&path_buf, size) {
            Ok(actual) => {
                if let Some(meta) = state.meta.as_mut() {
                    meta.size = actual;
                }
                state.kind = PreviewKind::Unsupported;
                state.message = Some("暂不支持此格式的内容预览".into());
            }
            Err(err) => apply_error(&mut state, err),
        },
    }
    state
}

fn apply_error(state: &mut PreviewState, message: String) {
    state.kind = PreviewKind::Error;
    state.message = Some(message);
    state.image = None;
    state.text = None;
    state.dir_entries.clear();
}

fn apply_image(state: &mut PreviewState, loaded: LoadedImage) {
    if let Some(meta) = state.meta.as_mut() {
        meta.size = loaded.file_len;
    }
    state.kind = PreviewKind::Image;
    state.image = Some(loaded.image);
    state.image_width = loaded.width;
    state.image_height = loaded.height;
    state.image_note = loaded.note;
    state.message = None;
}

fn apply_text(state: &mut PreviewState, loaded: LoadedText) {
    if let Some(meta) = state.meta.as_mut() {
        meta.size = loaded.file_len;
    }
    state.kind = PreviewKind::Text;
    state.text = Some(loaded.text);
    state.text_encoding = Some(loaded.encoding);
    state.truncated = loaded.truncated;
    state.message = None;
}

fn file_len_or_size(path: &Path, fallback: u64) -> Result<u64, String> {
    match fs::metadata(path) {
        Ok(meta) => Ok(meta.len()),
        Err(err) if fallback > 0 && err.kind() != std::io::ErrorKind::NotFound => Ok(fallback),
        Err(err) => Err(io_error_message(&err, &path.display().to_string())),
    }
}

pub(crate) struct LoadedImage {
    image: Arc<RenderImage>,
    width: u32,
    height: u32,
    file_len: u64,
    note: Option<String>,
}

fn load_image(path: &Path) -> Result<LoadedImage, String> {
    let display = path.display().to_string();
    let meta = fs::metadata(path).map_err(|err| io_error_message(&err, &display))?;
    if !meta.is_file() {
        return Err(format!("不是可预览的图片文件：{display}"));
    }
    let file_len = meta.len();
    if file_len > MAX_IMAGE_BYTES {
        let mb = file_len as f64 / (1024.0 * 1024.0);
        return Err(format!(
            "图片过大（{mb:.1} MB），已跳过内容预览，以免占用过多内存。"
        ));
    }
    let bytes = fs::read(path).map_err(|err| io_error_message(&err, &display))?;
    decode_image_bytes(&bytes, file_len)
}

pub(crate) fn decode_image_bytes(bytes: &[u8], file_len: u64) -> Result<LoadedImage, String> {
    let reader = ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|err| format!("无法识别图片格式：{err}"))?;
    let mut decoded = reader
        .decode()
        .map_err(|err| format!("无法解码图片：{err}"))?;
    let (orig_w, orig_h) = decoded.dimensions();
    if orig_w == 0 || orig_h == 0 {
        return Err("图片尺寸无效".into());
    }
    let mut note = None;
    if orig_w.max(orig_h) > MAX_IMAGE_EDGE {
        decoded = decoded.resize(MAX_IMAGE_EDGE, MAX_IMAGE_EDGE, FilterType::Triangle);
        note = Some(format!("已缩小显示（原图 {orig_w}×{orig_h}）"));
    }
    let (width, height) = decoded.dimensions();
    let mut rgba: RgbaImage = decoded.into_rgba8();
    for pixel in rgba.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    let frame = image::Frame::new(rgba);
    let image = Arc::new(RenderImage::new(vec![frame]));
    Ok(LoadedImage {
        image,
        width,
        height,
        file_len,
        note,
    })
}

struct LoadedText {
    text: String,
    encoding: String,
    truncated: bool,
    file_len: u64,
}

fn sniff_regular_file(path: &Path) -> Result<bool, String> {
    let display = path.display().to_string();
    let link = fs::symlink_metadata(path).map_err(|err| io_error_message(&err, &display))?;
    let meta = if link.file_type().is_symlink() {
        let target = fs::metadata(path).map_err(|err| io_error_message(&err, &display))?;
        if !target.is_file() {
            return Err(format!("无法预览此链接目标：{display}"));
        }
        target
    } else if link.is_file() {
        link
    } else {
        return Err(format!("不是可预览的普通文件：{display}"));
    };
    let file_len = meta.len();
    if file_len == 0 {
        return Ok(true);
    }
    let mut file = File::open(path).map_err(|err| io_error_message(&err, &display))?;
    let mut buf = vec![0u8; SNIFF_BYTES.min(file_len as usize)];
    let n = file
        .read(&mut buf)
        .map_err(|err| io_error_message(&err, &display))?;
    buf.truncate(n);
    Ok(sniff_text_prefix(&buf))
}

fn load_text(path: &Path) -> Result<LoadedText, String> {
    let display = path.display().to_string();
    let meta = fs::metadata(path).map_err(|err| io_error_message(&err, &display))?;
    if !meta.is_file() {
        return Err(format!("不是可预览的文本文件：{display}"));
    }
    let file_len = meta.len();
    let mut file = File::open(path).map_err(|err| io_error_message(&err, &display))?;
    let mut buf = Vec::new();
    let to_read = (MAX_TEXT_BYTES + 1).min(file_len as usize);
    buf.resize(to_read, 0);
    let n = file
        .read(&mut buf)
        .map_err(|err| io_error_message(&err, &display))?;
    buf.truncate(n);
    if looks_binary(&buf) {
        return Err("该文件看起来是二进制内容，暂不支持此格式的内容预览".into());
    }
    let mut truncated = file_len as usize > n;
    if !truncated {
        // If we were not at EOF for a growing file, still check.
        if file.seek(SeekFrom::Current(0)).ok() != Some(file_len) && file_len as usize > n {
            truncated = true;
        }
    }
    let (mut text, encoding) = decode_text_bytes(&buf);
    let line_truncated = truncate_lines(&mut text, MAX_TEXT_LINES);
    truncated = truncated || line_truncated || text.len() > MAX_TEXT_BYTES;
    if text.len() > MAX_TEXT_BYTES {
        // keep a valid UTF-8 prefix
        let mut end = MAX_TEXT_BYTES;
        while end > 0 && !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        truncated = true;
    }
    Ok(LoadedText {
        text,
        encoding: encoding.to_string(),
        truncated,
        file_len,
    })
}

pub fn looks_binary(bytes: &[u8]) -> bool {
    if bytes.is_empty() {
        return false;
    }
    if bytes.starts_with(&[0xFF, 0xFE]) || bytes.starts_with(&[0xFE, 0xFF]) {
        return false;
    }
    let sample = &bytes[..bytes.len().min(1024)];
    sample.contains(&0)
}

pub fn decode_text_bytes(bytes: &[u8]) -> (String, &'static str) {
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return (String::from_utf8_lossy(&bytes[3..]).into_owned(), "UTF-8");
    }
    if bytes.starts_with(&[0xFF, 0xFE]) {
        let (cow, _, _) = encoding_rs::UTF_16LE.decode(&bytes[2..]);
        return (cow.into_owned(), "UTF-16 LE");
    }
    if bytes.starts_with(&[0xFE, 0xFF]) {
        let (cow, _, _) = encoding_rs::UTF_16BE.decode(&bytes[2..]);
        return (cow.into_owned(), "UTF-16 BE");
    }
    if let Ok(s) = std::str::from_utf8(bytes) {
        return (s.to_string(), "UTF-8");
    }
    let (cow, _, had_errors) = encoding_rs::GBK.decode(bytes);
    if !had_errors {
        return (cow.into_owned(), "GBK");
    }
    (String::from_utf8_lossy(bytes).into_owned(), "UTF-8（替换）")
}

pub fn truncate_lines(text: &mut String, max_lines: usize) -> bool {
    let mut lines = 0usize;
    let mut end = text.len();
    for (idx, _) in text.match_indices('\n') {
        lines += 1;
        if lines >= max_lines {
            end = idx + 1;
            break;
        }
    }
    if end < text.len() {
        text.truncate(end);
        if !text.ends_with('\n') {
            text.push('\n');
        }
        return true;
    }
    false
}

fn load_directory(path: &Path) -> Result<(Vec<DirEntryPreview>, usize, bool), String> {
    let display = path.display().to_string();
    let mut entries: Vec<DirEntryPreview> = Vec::new();
    let iter = fs::read_dir(path).map_err(|err| io_error_message(&err, &display))?;
    for item in iter {
        let item = match item {
            Ok(item) => item,
            Err(_) => continue,
        };
        let name = item.file_name().to_string_lossy().into_owned();
        let is_dir = item.file_type().map(|t| t.is_dir()).unwrap_or(false);
        entries.push(DirEntryPreview { name, is_dir });
    }
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    let total = entries.len();
    let truncated = total > MAX_DIR_ENTRIES;
    entries.truncate(MAX_DIR_ENTRIES);
    Ok((entries, total, truncated))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn classifies_common_extensions() {
        assert_eq!(classify_path("/tmp/a.png", false), PreviewClass::Image);
        assert_eq!(classify_path("C:\\docs\\a.JPG", false), PreviewClass::Image);
        assert_eq!(classify_path("note.md", false), PreviewClass::Text);
        assert_eq!(classify_path("main.rs", false), PreviewClass::Text);
        assert_eq!(classify_path("song.mp3", false), PreviewClass::Binary);
        assert_eq!(classify_path("C:\\folder", true), PreviewClass::Directory);
        assert_eq!(classify_path("", false), PreviewClass::Empty);
    }

    #[test]
    fn utf8_and_gbk_text_decode() {
        assert_eq!(
            decode_text_bytes("你好".as_bytes()),
            ("你好".into(), "UTF-8")
        );
        let gbk = encoding_rs::GBK.encode("你好世界").0;
        let (text, enc) = decode_text_bytes(&gbk);
        assert_eq!(text, "你好世界");
        assert_eq!(enc, "GBK");
        let mut utf16 = vec![0xFF, 0xFE];
        for u in "Hi".encode_utf16() {
            utf16.extend_from_slice(&u.to_le_bytes());
        }
        let (text, enc) = decode_text_bytes(&utf16);
        assert_eq!(text, "Hi");
        assert_eq!(enc, "UTF-16 LE");
    }

    #[test]
    fn text_line_cap_truncates() {
        let mut text = (0..10).map(|i| format!("L{i}\n")).collect::<String>();
        assert!(truncate_lines(&mut text, 3));
        assert_eq!(text.matches('\n').count(), 3);
        let mut short = "a\nb\n".to_string();
        assert!(!truncate_lines(&mut short, 10));
    }

    #[test]
    fn nul_bytes_look_binary() {
        assert!(looks_binary(b"MZ\0\0"));
        assert!(!looks_binary(b"hello text"));
        assert!(!looks_binary(&[0xFF, 0xFE, 0x61, 0x00]));
    }

    #[test]
    fn extensionless_and_unknown_text_is_sniffed_and_nul_is_rejected() {
        assert!(needs_content_sniff(""));
        assert!(needs_content_sniff("token"));
        assert!(!needs_content_sniff("png"));
        assert!(!needs_content_sniff("exe"));
        assert!(!needs_content_sniff("pdf"));
        assert!(sniff_text_prefix(b""));
        assert!(sniff_text_prefix(b"aau_token"));
        assert!(sniff_text_prefix(&[0xFF, 0xFE, b'a', 0, b'b', 0]));
        assert!(sniff_text_prefix("你好".as_bytes()));
        let gbk = encoding_rs::GBK.encode("配置文件").0;
        assert!(sniff_text_prefix(&gbk));
        assert!(!sniff_text_prefix(b"MZ\0\0binary"));
        let dir = std::env::temp_dir().join("findx-native-sniff-tests");
        let _ = fs::create_dir_all(&dir);
        let token = dir.join("aau_token");
        fs::write(&token, b"public-token").unwrap();
        let state = load_preview(1, "aau_token", &token.to_string_lossy(), 12, 0, false);
        assert_eq!(state.kind, PreviewKind::Text);
        assert_eq!(state.text.as_deref(), Some("public-token"));
        assert_eq!(state.meta.as_ref().map(|m| m.kind_label.as_str()), Some("文本"));
        let empty = dir.join("empty-file");
        fs::write(&empty, b"").unwrap();
        let state = load_preview(1, "empty-file", &empty.to_string_lossy(), 0, 0, false);
        assert_eq!(state.kind, PreviewKind::Text);
        let bin = dir.join("blob.unknown");
        fs::write(&bin, b"abc\0def").unwrap();
        let state = load_preview(1, "blob.unknown", &bin.to_string_lossy(), 7, 0, false);
        assert_eq!(state.kind, PreviewKind::Unsupported);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_text_file_respects_caps() {
        let dir = std::env::temp_dir().join("findx-native-preview-tests");
        let _ = fs::create_dir_all(&dir);
        let path = dir.join("sample.txt");
        let mut file = File::create(&path).unwrap();
        writeln!(file, "first line").unwrap();
        writeln!(file, "second line").unwrap();
        drop(file);
        let loaded = load_text(&path).unwrap();
        assert!(loaded.text.contains("first line"));
        assert_eq!(loaded.encoding, "UTF-8");
        assert!(!loaded.truncated);
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn pdf_frame_identity_includes_the_rendered_page_and_path() {
        let mut state = PreviewState::loading(1, meta_from_hit("f.pdf", r"C:\f.pdf", 10, 0, false));
        state.pdf = Some(PdfMeta {
            page: 0,
            count: 3,
            width: 100.,
            height: 200.,
        });
        assert!(state.pdf_frame_matches(r"C:\f.pdf", 0));
        assert!(!state.pdf_frame_matches(r"C:\f.pdf", 1));
        assert!(!state.pdf_frame_matches(r"C:\other.pdf", 0));
    }
    #[test]
    fn tiny_png_decodes() {
        // 1x1 red PNG
        let png: &[u8] = &[
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00,
            0x00, 0x90, 0x77, 0x53, 0xDE, 0x00, 0x00, 0x00, 0x0C, 0x49, 0x44, 0x41, 0x54, 0x08,
            0xD7, 0x63, 0xF8, 0xCF, 0xC0, 0x00, 0x00, 0x00, 0x03, 0x00, 0x01, 0x00, 0x05, 0xFE,
            0xD4, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
        ];
        // The IDAT above may not be a perfectly valid tiny PNG depending on checksum;
        // generate one with the image crate instead if decode fails.
        let decoded = decode_image_bytes(png, png.len() as u64).or_else(|_| {
            let mut buf = Vec::new();
            image::RgbImage::from_pixel(2, 2, image::Rgb([255, 0, 0]))
                .write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
                .unwrap();
            decode_image_bytes(&buf, buf.len() as u64)
        });
        let loaded = decoded.expect("png should decode");
        assert!(loaded.width >= 1 && loaded.height >= 1);
        assert!(loaded.image.frame_count() >= 1);
    }

    #[test]
    fn directory_listing_is_capped() {
        let dir = std::env::temp_dir().join("findx-native-preview-dir-test");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        for i in 0..8 {
            fs::write(dir.join(format!("f{i}.txt")), b"x").unwrap();
        }
        fs::create_dir(dir.join("sub")).unwrap();
        let (entries, total, truncated) = load_directory(&dir).unwrap();
        assert_eq!(total, 9);
        assert!(!truncated);
        assert!(entries.first().is_some_and(|e| e.is_dir && e.name == "sub"));
        let _ = fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod generation_tests {
    #[test]
    fn selection_stale_payload_is_rejected() {
        assert!(super::is_current_generation(7, 7, 7));
        assert!(!super::is_current_generation(8, 7, 7));
        assert!(!super::is_current_generation(8, 8, 7));
    }
}

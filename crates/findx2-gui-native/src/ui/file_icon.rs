//! All file and directory glyphs use the same bundled Lucide SVG library as controls.
use crate::theme::FindxTokens;
use gpui_kit::{component::Icon, *};
pub(super) fn file_icon(name: &str, directory: bool, t: FindxTokens) -> AnyElement {
    let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    let (asset, color) = if directory {
        ("folder", t.warning)
    } else {
        match ext.as_str() {
            "pdf" => ("file-text", rgb(0xb7615a).into()),
            "doc" | "docx" | "rtf" => ("file-text", rgb(0x5c83aa).into()),
            "xls" | "xlsx" => ("file-spreadsheet", rgb(0x5e967d).into()),
            "ppt" | "pptx" => ("presentation", rgb(0xc08055).into()),
            "png" | "jpg" | "jpeg" | "webp" | "bmp" => ("file-image", rgb(0x8b79a1).into()),
            "zip" | "7z" | "rar" => ("file-archive", rgb(0x9a8b57).into()),
            _ => ("file", t.text_secondary),
        }
    };
    Icon::default()
        .path(format!("icons/{asset}.svg"))
        .size_4()
        .text_color(color)
        .into_any_element()
}

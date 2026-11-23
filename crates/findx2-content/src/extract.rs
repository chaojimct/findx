//! M0 文本抽取：纯文本三件套（txt/md/csv）。设计上限见 `MAX_EXTRACT_BYTES`。
//!
//! - 编码：UTF-8 优先，失败回退 GB18030（encoding_rs），再失败走 lossy。
//! - 超限/毒文件：返回 `Error::Extract`，由调用方计数跳过，绝不 panic。

use std::path::Path;

use crate::{Error, Result};

/// 单文件抽取上限：M0 取 20MB（`content:` 现有慢路径是 100MB 全读；
/// 倒排不需要存全文，此处只为分词，20MB 足够，超限由 M1 分块解决）。
pub const MAX_EXTRACT_BYTES: u64 = 20 * 1024 * 1024;

/// M0 支持的扩展名（小写，无点）。
pub const SUPPORTED_EXTENSIONS: &[&str] = &["txt", "md", "markdown", "csv", "log", "ini", "cfg"];

pub struct ExtractedText {
    /// 分词用正文（已解码）。
    pub text: String,
    /// 抽取字节数（解码前），用于 bench 体积比统计。
    pub raw_bytes: u64,
}

/// 是否值得抽取：扩展名白名单 + 大小预检。目录/超限/无扩展名返回 None（跳过，非错误）。
pub fn should_extract(path: &Path) -> Option<u64> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    if !SUPPORTED_EXTENSIONS.contains(&ext.as_str()) {
        return None;
    }
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > MAX_EXTRACT_BYTES || meta.len() == 0 {
        return None;
    }
    Some(meta.len())
}

pub fn extract_text(path: &Path) -> Result<ExtractedText> {
    let raw = std::fs::read(path).map_err(|e| {
        Error::Io(path.display().to_string(), e)
    })?;
    if raw.len() as u64 > MAX_EXTRACT_BYTES {
        return Err(Error::Extract(
            path.display().to_string(),
            format!("超限 {}B > {}B", raw.len(), MAX_EXTRACT_BYTES),
        ));
    }
    let (text, _) = decode_best_effort(&raw);
    Ok(ExtractedText {
        text,
        raw_bytes: raw.len() as u64,
    })
}

/// UTF-8 → GB18030 → lossy 三级解码（中文盘存量 GBK 文本可搜是 M0 验收项之一）。
fn decode_best_effort(raw: &[u8]) -> (String, &'static str) {
    if let Ok(s) = std::str::from_utf8(raw) {
        return (s.to_owned(), "utf-8");
    }
    let (cow, _, had_errors) = encoding_rs::GB18030.decode(raw);
    if !had_errors {
        return (cow.into_owned(), "gb18030");
    }
    (String::from_utf8_lossy(raw).into_owned(), "lossy")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn tmp_file(name: &str, data: &[u8]) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(name);
        std::fs::File::create(&p).unwrap().write_all(data).unwrap();
        (dir, p)
    }

    #[test]
    fn utf8_roundtrip() {
        let (_d, p) = tmp_file("a.txt", "全文搜索 hello world".as_bytes());
        let e = extract_text(&p).unwrap();
        assert!(e.text.contains("全文搜索"));
    }

    #[test]
    fn gbk_fallback() {
        // "中文" 的 GBK 编码（非合法 UTF-8），验证二级解码。
        let (_d, p) = tmp_file("b.txt", &[0xD6, 0xD0, 0xCE, 0xC4]);
        let e = extract_text(&p).unwrap();
        assert_eq!(e.text, "中文");
    }

    #[test]
    fn unsupported_ext_skipped() {
        let (_d, p) = tmp_file("c.pdf", b"%PDF fake");
        assert!(should_extract(&p).is_none());
    }
}

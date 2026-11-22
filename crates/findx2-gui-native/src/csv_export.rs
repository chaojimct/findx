//! Search-result CSV export.
//!
//! Pure formatting only: the caller owns the file dialog and the write, so this
//! module stays testable without a window, a service, or a filesystem.
//!
//! Two decisions are deliberate:
//! - UTF-8 BOM, because Excel on Windows otherwise decodes Chinese paths as
//!   ANSI and shows mojibake even though the bytes are valid UTF-8.
//! - CRLF record separator and quote-on-demand, per RFC 4180. Fields that
//!   contain a comma, quote, CR or LF are quoted and inner quotes are doubled.
//!   Paths routinely contain commas (`C:\Users\a,b\`), so this is not optional.

use findx2_ipc::SearchHitDto;

/// Byte-order mark Excel needs to guess UTF-8 instead of the ANSI code page.
pub const BOM: char = '\u{feff}';

/// Column order is stable: spreadsheets and scripts key off it.
pub const HEADER: [&str; 6] = ["名称", "路径", "类型", "大小", "字节数", "修改时间"];

/// `2026-11-22 16-30-05` — sorts lexicographically and is a legal file name.
pub fn suggested_file_name(now_unix_secs: u64) -> String {
    format!(
        "findx2-结果-{}.csv",
        crate::search_logic::format_time(now_unix_secs).replace([':', ' '], "-")
    )
}

/// Wall clock for the suggested name.
pub fn now_unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Where the save dialog starts. Never creates anything; the platform prompt
/// and the final write own the filesystem side.
pub fn default_directory() -> std::path::PathBuf {
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(std::path::PathBuf::from);
    match home {
        Some(home) => {
            let downloads = home.join("Downloads");
            if downloads.is_dir() { downloads } else { home }
        }
        None => std::path::PathBuf::from("."),
    }
}

/// Quote a field only when CSV requires it, doubling inner quotes.
pub fn escape(field: &str) -> String {
    let needs_quotes = field
        .chars()
        .any(|c| matches!(c, ',' | '"' | '\r' | '\n'));
    if !needs_quotes {
        return field.to_string();
    }
    let mut out = String::with_capacity(field.len() + 2);
    out.push('"');
    for c in field.chars() {
        if c == '"' {
            out.push('"');
        }
        out.push(c);
    }
    out.push('"');
    out
}

/// One record (without the trailing CRLF).
pub fn record(hit: &SearchHitDto) -> String {
    let kind = if hit.is_directory { "目录" } else { "文件" };
    [
        escape(&hit.name),
        escape(&hit.path),
        kind.to_string(),
        crate::search_logic::format_size(hit.size, hit.is_directory),
        hit.size.to_string(),
        if hit.mtime == 0 {
            "未记录".to_string()
        } else {
            escape(&crate::search_logic::format_time(hit.mtime))
        },
    ]
    .join(",")
}

/// Whole document: BOM, header, one record per hit, CRLF line endings.
/// An empty hit list still yields BOM + header so the file opens as a table.
pub fn render(hits: &[SearchHitDto]) -> String {
    let mut out = String::with_capacity(64 + hits.len() * 96);
    out.push(BOM);
    out.push_str(&HEADER.join(","));
    out.push_str("\r\n");
    for hit in hits {
        out.push_str(&record(hit));
        out.push_str("\r\n");
    }
    out
}

/// Status-bar wording after a successful write. Counts the rows actually
/// exported so a filtered result set cannot be mistaken for the whole index.
pub fn done_message(rows: usize, path: &std::path::Path) -> String {
    format!("已导出 {rows} 条到 {}", path.display())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(name: &str, path: &str, size: u64, mtime: u64, dir: bool) -> SearchHitDto {
        SearchHitDto {
            entry_idx: 0,
            name: name.into(),
            path: path.into(),
            size,
            mtime,
            is_directory: dir,
            name_highlight: vec![],
        }
    }

    #[test]
    fn plain_fields_are_not_quoted() {
        assert_eq!(escape("report.pdf"), "report.pdf");
        assert_eq!(escape("C:\\Users\\me\\a.txt"), "C:\\Users\\me\\a.txt");
        assert_eq!(escape(""), "");
    }

    #[test]
    fn separators_and_quotes_force_quoting() {
        assert_eq!(escape("a,b"), "\"a,b\"");
        assert_eq!(escape("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(escape("line1\nline2"), "\"line1\nline2\"");
        assert_eq!(escape("cr\rhere"), "\"cr\rhere\"");
        // A lone quote still needs quoting, otherwise the reader desyncs.
        assert_eq!(escape("\""), "\"\"\"\"");
    }

    #[test]
    fn document_starts_with_bom_then_header_and_uses_crlf() {
        let doc = render(&[]);
        assert_eq!(doc, format!("{BOM}{}\r\n", HEADER.join(",")));
        assert!(!doc.contains("\n\n"));
    }

    #[test]
    fn records_keep_column_order_and_types() {
        let doc = render(&[
            hit("a,b.txt", "C:\\x\\a,b.txt", 2048, 1_700_000_000, false),
            hit("folder", "C:\\x\\folder", 0, 0, true),
        ]);
        let mut lines = doc.split("\r\n").collect::<Vec<_>>();
        assert_eq!(lines.pop(), Some("")); // trailing CRLF
        let rows = lines;
        assert_eq!(rows.len(), 3);
        assert!(rows[0].ends_with(&HEADER.join(",")));
        assert!(rows[1].starts_with("\"a,b.txt\",\"C:\\x\\a,b.txt\",文件,"));
        assert!(rows[1].contains(",2048,"));
        // Unknown mtime is spelled out, never 1970. Directory size stays empty
        // (the index has no byte size for a directory), same as the result table.
        assert!(rows[2].ends_with("目录,,0,未记录"));
        assert!(!rows[2].contains("1970"));
    }

    #[test]
    fn suggested_name_is_a_legal_file_name() {
        let name = suggested_file_name(1_700_000_000);
        assert!(name.starts_with("findx2-结果-"));
        assert!(name.ends_with(".csv"));
        for bad in [':', ' ', '/', '\\', '*', '?', '"', '<', '>', '|'] {
            assert!(!name.contains(bad), "{bad} in {name}");
        }
    }

    #[test]
    fn done_message_counts_exported_rows_not_the_index() {
        let msg = done_message(3, std::path::Path::new("C:\\out.csv"));
        assert!(msg.starts_with("已导出 3 条"));
        assert!(msg.contains("out.csv"));
    }
}

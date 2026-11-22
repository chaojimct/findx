//! Redacted diagnostics bundle.
//!
//! A support report has to be safe to paste into a public issue. Everything
//! here is therefore additive to the existing read-only status view: the same
//! facts, minus the parts that identify the machine or the person.
//!
//! Redaction is deliberately a pure function of (text, home directory), so the
//! rules are unit-tested instead of being trusted:
//!
//! - the caller's home directory becomes `%USERPROFILE%`
//! - any Windows/macOS/Linux user directory segment becomes `用户`
//! - e-mail addresses become `<邮箱>`
//!
//! Nothing else is rewritten. Index paths, drive letters, counts and WAL
//! numbers stay exact, because a support report that hides the numbers is
//! useless.

/// One labelled block of the report.
pub struct Section {
    pub title: String,
    pub lines: Vec<String>,
}

/// Render sections in order with a fixed header. No timestamps are generated
/// here; the caller passes one so the same inputs always produce the same
/// bytes (and tests can assert on them).
pub fn render(generated_at: &str, header: &[String], sections: &[Section]) -> String {
    let mut out = String::with_capacity(1024);
    out.push_str("FindX2 诊断信息\r\n");
    out.push_str(&format!("生成时间: {generated_at}\r\n"));
    if !header.is_empty() {
        out.push_str("\r\n[环境]\r\n");
        for line in header {
            out.push_str(line);
            out.push_str("\r\n");
        }
    }
    for section in sections {
        out.push_str(&format!("\r\n[{}]\r\n", section.title));
        if section.lines.is_empty() {
            out.push_str("(无)\r\n");
            continue;
        }
        for line in &section.lines {
            out.push_str(line);
            out.push_str("\r\n");
        }
    }
    out
}

/// Apply every redaction rule, in the order that avoids re-masking.
pub fn redact(text: &str, home: Option<&str>) -> String {
    let mut out = text.to_string();
    if let Some(home) = home {
        let home = home.trim_end_matches(['\\', '/']);
        if home.len() > 3 {
            out = replace_ascii_ci(&out, home, "%USERPROFILE%");
        }
    }
    let out = mask_user_dirs(&out);
    mask_emails(&out)
}

/// ASCII case-insensitive replace, used for drive letters (`c:\Users` vs
/// `C:\Users`) while leaving every other byte untouched.
fn replace_ascii_ci(haystack: &str, needle: &str, with: &str) -> String {
    let hay = haystack.as_bytes();
    let pat = needle.as_bytes();
    if pat.is_empty() || pat.len() > hay.len() {
        return haystack.to_string();
    }
    let mut out = String::with_capacity(haystack.len());
    let mut i = 0usize;
    while i < hay.len() {
        if i + pat.len() <= hay.len() && hay[i..i + pat.len()].eq_ignore_ascii_case(pat) {
            out.push_str(with);
            i += pat.len();
        } else {
            // Copy one whole UTF-8 scalar so a non-ASCII path cannot be split.
            let ch = haystack[i..].chars().next().unwrap_or('\u{fffd}');
            out.push(ch);
            i += ch.len_utf8();
        }
    }
    out
}

/// `C:\Users\mact\x` → `C:\Users\用户\x`, `home/alice` → `home/用户`.
fn mask_user_dirs(text: &str) -> String {
    const MARKERS: [&str; 3] = ["\\users\\", "/users/", "/home/"];
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0usize;
    while i < text.len() {
        let mut hit = None;
        for marker in MARKERS {
            if i + marker.len() <= text.len()
                && bytes[i..i + marker.len()].eq_ignore_ascii_case(marker.as_bytes())
            {
                hit = Some(marker);
                break;
            }
        }
        let Some(marker) = hit else {
            let ch = text[i..].chars().next().unwrap_or('\u{fffd}');
            out.push(ch);
            i += ch.len_utf8();
            continue;
        };
        let start = i + marker.len();
        out.push_str(&text[i..start]);
        // A trailing separator or the end of the text means there is no name.
        let end = text[start..]
            .find(['\\', '/', ' ', '\t', '\r', '\n', '"', '\'', ')', ']', ',', ';'])
            .map(|off| start + off)
            .unwrap_or(text.len());
        if end > start {
            out.push_str("用户");
        }
        i = end;
    }
    out
}

/// Replace `local@domain.tld` spans with `<邮箱>`.
fn mask_emails(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let is_local = |c: char| c.is_ascii_alphanumeric() || "._%+-".contains(c);
    let is_domain = |c: char| c.is_ascii_alphanumeric() || ".-".contains(c);
    let mut out = String::with_capacity(text.len());
    let mut i = 0usize;
    while i < chars.len() {
        if chars[i] != '@' {
            out.push(chars[i]);
            i += 1;
            continue;
        }
        // Walk back over the local part and forward over the domain.
        let mut start = i;
        while start > 0 && is_local(chars[start - 1]) {
            start -= 1;
        }
        let mut end = i + 1;
        while end < chars.len() && is_domain(chars[end]) {
            end += 1;
        }
        let local_ok = start < i;
        let domain_ok = end > i + 1 && chars[i + 1..end].contains(&'.');
        if !(local_ok && domain_ok) {
            out.push('@');
            i += 1;
            continue;
        }
        // The local part was already emitted while scanning; trim it back.
        let emitted = out.chars().count();
        let keep = emitted - (i - start);
        out = out.chars().take(keep).collect();
        out.push_str("<邮箱>");
        i = end;
    }
    out
}

/// Environment facts worth keeping: they explain behaviour without naming the
/// machine. Deliberately excludes hostname and user name.
pub fn environment(version: &str) -> Vec<String> {
    vec![
        format!("客户端: {version}"),
        format!("系统: {} ({})", std::env::consts::OS, std::env::consts::ARCH),
        format!("构建: {}", if cfg!(debug_assertions) { "debug" } else { "release" }),
    ]
}

/// Home directory, if the platform exposes one.
pub fn home_dir() -> Option<String> {
    std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .ok()
        .filter(|h| !h.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_prefix_is_replaced_case_insensitively() {
        let out = redact(r"failed to open c:\users\Mact\dev\a.txt", Some(r"C:\Users\Mact"));
        assert!(!out.to_ascii_lowercase().contains("mact"), "{out}");
        assert!(out.contains("%USERPROFILE%\\dev\\a.txt"), "{out}");
    }

    #[test]
    fn user_directory_segments_are_masked_without_touching_the_rest() {
        assert_eq!(
            redact(r"C:\Users\alice\AppData\Roaming\FindX2\settings.json", None),
            r"C:\Users\用户\AppData\Roaming\FindX2\settings.json"
        );
        assert_eq!(redact("/home/bob/.config/findx2.json", None), "/home/用户/.config/findx2.json");
        assert_eq!(redact("/Users/carol/Library/x", None), "/Users/用户/Library/x");
        // The marker itself is never a name.
        assert_eq!(redact(r"C:\Users\", None), r"C:\Users\");
        assert_eq!(redact("users\\dave\\log", None), "users\\dave\\log");
    }

    #[test]
    fn index_numbers_and_drive_letters_survive() {
        let text = "索引 9,912,345 项 · 健康 · C:\\ProgramData\\FindX\\index.bin.v6 · WAL 3.0MB";
        assert_eq!(redact(text, Some(r"C:\Users\Mact")), text);
    }

    #[test]
    fn emails_are_masked_only_when_they_look_like_emails() {
        assert_eq!(redact("contact a.b+c@example.com now", None), "contact <邮箱> now");
        assert_eq!(redact("key@value", None), "key@value");
        assert_eq!(redact("a@b", None), "a@b");
        // Idempotent: the placeholder contains no '@'.
        let once = redact("x@example.org", None);
        assert_eq!(redact(&once, None), once);
    }

    #[test]
    fn render_keeps_sections_in_order_and_marks_empty_ones() {
        let doc = render(
            "2026-11-22 16:30:05",
            &["客户端: 2.5.10".to_string()],
            &[
                Section { title: "索引状态".into(), lines: vec!["索引 1 项".into()] },
                Section { title: "服务日志".into(), lines: vec![] },
            ],
        );
        assert!(doc.starts_with("FindX2 诊断信息\r\n"));
        let index_at = doc.find("[索引状态]").unwrap();
        let log_at = doc.find("[服务日志]").unwrap();
        assert!(index_at < log_at);
        assert!(doc.contains("(无)\r\n"));
        // CRLF everywhere; a lone LF would break Windows paste targets.
        assert!(!doc.replace("\r\n", "").contains('\n'));
    }

    #[test]
    fn environment_never_names_the_machine() {
        let lines = environment("2.5.10");
        let joined = lines.join("\n");
        for forbidden in std::env::var("USERNAME").ok().into_iter().chain(home_dir()) {
            if forbidden.len() > 2 {
                assert!(!joined.contains(&forbidden), "{joined}");
            }
        }
        assert!(joined.contains("2.5.10"));
    }
}

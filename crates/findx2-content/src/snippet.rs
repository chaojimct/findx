//! 按需 snippet（schema v2）：body 不再 STORED，snippet 在展示时回读原文生成。
//!
//! 流程：doc 存 path（STORED 不变）→ `extract_text(path)` 回读 → 按查询词
//! 切窗口（命中前后各约 120 字）→ 高亮 → HTML。结果按
//! `(path, mtime, query)` 缓存（FIFO 上限 500 条，snippet 短文本，常驻可忽略）。
//!
//! 回读失败（删除/改名/云占位/权限）→ 返回 None，调用方回退空串，行本身不受影响。
//! 大文件：extract 已有 20MB 上限；窗口搜索是字节扫描，毫秒级。

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

use crate::{extract_text, Error, Result};

const CACHE_CAP: usize = 500;
const WINDOW_CHARS: usize = 120;

/// query → 分词（与索引同一套 HybridTokenizer，保证切分一致）。
pub fn query_terms(query_text: &str, tok: &mut crate::index::HybridTokenizer) -> Vec<String> {
    use tantivy::tokenizer::{TokenStream, Tokenizer};
    let mut stream = tok.token_stream(query_text);
    let mut out = Vec::new();
    while stream.advance() {
        let t = stream.token().text.clone();
        if !t.is_empty() {
            out.push(t);
        }
    }
    out
}

/// HTML 转义（snippet 其余部分），命中片段包 `<b>`（与 tantivy to_html 同格式）。
fn escape(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            _ => o.push(c),
        }
    }
    o
}

/// 在正文里找任一词的最早命中字节位（ASCII 大小写不敏感；ascii-fold 不改变
/// 字节长度，偏移在原文中直接有效）。
fn earliest_hit(folded: &str, terms: &[String]) -> Option<usize> {
    let mut best: Option<usize> = None;
    for t in terms {
        let needle = t.to_ascii_lowercase();
        if needle.is_empty() {
            continue;
        }
        if let Some(pos) = folded.find(needle.as_str()) {
            best = Some(match best {
                Some(b) if b <= pos => b,
                _ => pos,
            });
        }
    }
    best
}

/// 命中点前后各 WINDOW_CHARS 个字符的字节窗口（含命中词本身）。
fn char_window(text: &str, pos: usize, term_len: usize) -> (usize, usize) {
    let prefix: Vec<usize> = text[..pos].char_indices().map(|(i, _)| i).collect();
    let start = if prefix.len() > WINDOW_CHARS {
        prefix[prefix.len() - WINDOW_CHARS]
    } else {
        0
    };
    let rest = &text[pos..];
    let mut end_rel = rest.len().max(term_len);
    let mut count = 0usize;
    for (i, _) in rest.char_indices() {
        if count >= WINDOW_CHARS.max(term_len) {
            end_rel = i.max(term_len);
            break;
        }
        count += 1;
    }
    (start, (pos + end_rel).min(text.len()))
}

/// 生成 snippet HTML；无命中返回 None（调用方回退空串）。
pub fn make_snippet(text: &str, terms: &[String]) -> Option<String> {
    if terms.is_empty() || text.is_empty() {
        return None;
    }
    let folded = text.to_ascii_lowercase();
    let pos = earliest_hit(&folded, terms)?;
    let term_len = terms
        .iter()
        .map(|t| t.len())
        .max()
        .unwrap_or(0)
        .min(64);
    let (start, mut end) = char_window(text, pos, term_len);
    end = end.min(text.len());
    if start >= end {
        return None;
    }
    let window = &text[start..end];
    let wfold = &folded[start..end];
    // 收集窗口内全部命中（按字节序），重叠取最长。
    let mut hits: Vec<(usize, usize)> = Vec::new();
    for t in terms {
        let needle = t.to_ascii_lowercase();
        if needle.is_empty() {
            continue;
        }
        let mut from = 0;
        while let Some(p) = wfold[from..].find(needle.as_str()) {
            let a = from + p;
            hits.push((a, a + needle.len()));
            from = a + needle.len().max(1);
        }
    }
    if hits.is_empty() {
        return None;
    }
    hits.sort();
    let mut out = String::new();
    if start > 0 {
        out.push_str("…");
    }
    let mut cur = 0;
    for (a, b) in hits {
        let b = b.min(window.len());
        let a = a.max(cur);
        if a < cur || a >= b {
            continue;
        }
        out.push_str(&escape(&window[cur..a]));
        out.push_str("<b>");
        out.push_str(&escape(&window[a..b]));
        out.push_str("</b>");
        cur = b;
    }
    out.push_str(&escape(&window[cur..]));
    if end < text.len() {
        out.push_str("…");
    }
    Some(out)
}

struct SnippetCache {
    map: HashMap<(String, u64, String), String>,
    order: VecDeque<(String, u64, String)>,
}

impl SnippetCache {
    fn get(&self, k: &(String, u64, String)) -> Option<String> {
        self.map.get(k).cloned()
    }

    fn put(&mut self, k: (String, u64, String), v: String) {
        if self.map.contains_key(&k) {
            return;
        }
        if self.map.len() >= CACHE_CAP {
            if let Some(old) = self.order.pop_front() {
                self.map.remove(&old);
            }
        }
        self.order.push_back(k.clone());
        self.map.insert(k, v);
    }
}

pub struct SnippetPool {
    cache: Mutex<SnippetCache>,
}

impl SnippetPool {
    pub fn new() -> Self {
        Self {
            cache: Mutex::new(SnippetCache {
                map: HashMap::new(),
                order: VecDeque::new(),
            }),
        }
    }

    /// 回读原文生成 snippet；失败/无命中返回 None（行不受影响）。
    pub fn for_doc(
        &self,
        path: &str,
        mtime: u64,
        query_text: &str,
        terms: &[String],
    ) -> Option<String> {
        let key = (path.to_string(), mtime, query_text.to_string());
        if let Ok(c) = self.cache.lock() {
            if let Some(h) = c.get(&key) {
                return Some(h);
            }
        }
        let text = extract_text(std::path::Path::new(path)).ok()?.text;
        let html = make_snippet(&text, terms)?;
        if let Ok(mut c) = self.cache.lock() {
            c.put(key, html.clone());
        }
        Some(html)
    }
}

impl Default for SnippetPool {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::HybridTokenizer;

    fn terms_of(q: &str) -> Vec<String> {
        let mut tok = HybridTokenizer::new();
        query_terms(q, &mut tok)
    }

    #[test]
    fn terms_split_like_index() {
        let t = terms_of("全文搜索 hello");
        assert!(t.iter().any(|x| x.contains("全文") || x.contains("搜索")));
        assert!(t.iter().any(|x| x == "hello"));
    }

    #[test]
    fn snippet_window_and_highlight() {
        let pad1 = "很长很长很长很长很长很长很长很长很长很长很长很长很长很长很长很长很长很长很长很长很长很长很长很长很长很长很长很长很长很长";
        let pad = format!("{pad1}{pad1}{pad1}{pad1}");
        let text = format!("前言{pad}。核心是全文搜索功能很好用。后记{pad}。");
        let terms = terms_of("全文搜索");
        let html = make_snippet(&text, &terms).expect("应命中");
        assert!(html.contains("<b>"), "{html}");
        assert!(html.contains("全文"), "{html}");
        // 窗口截断标记（前后都有省略号）。
        assert!(html.starts_with("…") && html.ends_with("…"), "{html}");
    }

    #[test]
    fn snippet_no_hit_is_none() {
        assert!(make_snippet("hello world", &terms_of("全文搜索")).is_none());
        assert!(make_snippet("", &terms_of("hello")).is_none());
        assert!(make_snippet("hello", &Vec::new()).is_none());
    }

    #[test]
    fn snippet_escapes_html() {
        let text = "a <script>alert(1)</script> 全文搜索 b";
        let html = make_snippet(text, &terms_of("全文搜索")).unwrap();
        assert!(!html.contains("<script>"), "{html}");
        assert!(html.contains("&lt;script&gt;"), "{html}");
    }
}

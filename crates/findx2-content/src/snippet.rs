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

use crate::extract_text;

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

/// 头部预览（拼音查询等命中词不在原文时用；截前 ~240 字）。
pub fn head_snippet(text: &str) -> Option<String> {
    let mut end = 0usize;
    let mut count = 0usize;
    for (i, _) in text.char_indices() {
        if count >= WINDOW_CHARS {
            break;
        }
        end = i;
        count += 1;
    }
    if end == 0 {
        return None;
    }
    // end 停在最后一个字符起点，向前取到该字符末尾。
    let last_len = text[end..].chars().next().map(|c| c.len_utf8()).unwrap_or(0);
    let end = (end + last_len).min(text.len());
    let mut out = escape(&text[..end]);
    if end < text.len() {
        out.push_str("…");
    }
    Some(out)
}
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
    render_window(text, &folded, start, end, terms)
}

/// 全文全部非重叠命中（字节区间，文档序；与 make_snippet 同一收集规则，
/// 重叠取排序首个）。上一个/下一个翻页用：窗口 k 围绕第 k 个命中展开。
pub fn match_spans(text: &str, terms: &[String]) -> Vec<(usize, usize)> {
    if terms.is_empty() || text.is_empty() {
        return Vec::new();
    }
    let folded = text.to_ascii_lowercase();
    let mut hits: Vec<(usize, usize)> = Vec::new();
    for t in terms {
        let needle = t.to_ascii_lowercase();
        if needle.is_empty() {
            continue;
        }
        let mut from = 0;
        while let Some(p) = folded[from..].find(needle.as_str()) {
            let a = from + p;
            hits.push((a, a + needle.len()));
            from = a + needle.len().max(1);
        }
    }
    if hits.is_empty() {
        return Vec::new();
    }
    hits.sort();
    let mut out = Vec::with_capacity(hits.len());
    let mut cur = 0usize;
    for (a, b) in hits {
        if a < cur || a >= b {
            continue;
        }
        out.push((a, b.min(text.len())));
        cur = b;
    }
    out
}

/// 第 k 个匹配点的上下文窗口 HTML + 全文共多少个匹配点（上一个/下一个用）。
/// k 越界钳到最后一个；无匹配返回 None。
pub fn make_snippet_windowed(
    text: &str,
    terms: &[String],
    window_idx: usize,
) -> Option<(String, usize)> {
    let spans = match_spans(text, terms);
    if spans.is_empty() {
        return None;
    }
    let k = window_idx.min(spans.len() - 1);
    let (pos, term_end) = spans[k];
    let term_len = term_end.saturating_sub(pos).min(64);
    let folded = text.to_ascii_lowercase();
    let (start, mut end) = char_window(text, pos, term_len);
    end = end.min(text.len());
    if start >= end {
        return None;
    }
    render_window(text, &folded, start, end, terms).map(|html| (html, spans.len()))
}

/// 窗口内全部命中高亮成 HTML（make_snippet 与 windowed 共用；调用方保证字节边界合法）。
fn render_window(
    text: &str,
    folded: &str,
    start: usize,
    end: usize,
    terms: &[String],
) -> Option<String> {
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
    map: HashMap<(String, u64, String, usize), (String, usize)>,
    order: VecDeque<(String, u64, String, usize)>,
}

impl SnippetCache {
    fn get(&self, k: &(String, u64, String, usize)) -> Option<(String, usize)> {
        self.map.get(k).cloned()
    }

    fn put(&mut self, k: (String, u64, String, usize), v: (String, usize)) {
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
        self.for_doc_windowed(path, mtime, query_text, terms, 0)
            .map(|(html, _)| html)
    }

    /// 第 window_idx 个匹配点的上下文窗口 + 全文共多少个匹配点（上一个/下一个用）。
    /// 窗口越界钳到最后一个；缓存键带窗口号。
    pub fn for_doc_windowed(
        &self,
        path: &str,
        mtime: u64,
        query_text: &str,
        terms: &[String],
        window_idx: usize,
    ) -> Option<(String, usize)> {
        let key = (
            path.to_string(),
            mtime,
            query_text.to_string(),
            window_idx,
        );
        if let Ok(c) = self.cache.lock() {
            if let Some(hit) = c.get(&key) {
                return Some(hit);
            }
        }
        let text = extract_text(std::path::Path::new(path)).ok()?.text;
        let (html, total) = match make_snippet_windowed(&text, terms, window_idx) {
            Some(t) => t,
            None if query_text.is_ascii() && window_idx == 0 => (head_snippet(&text)?, 1),
            None => return None,
        };
        if let Ok(mut c) = self.cache.lock() {
            c.put(key, (html.clone(), total));
        }
        Some((html, total))
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

    #[test]
    fn match_spans_collects_doc_order_and_dedups_overlap() {
        let terms = vec!["qw".to_string(), "quanw".to_string()];
        let spans = match_spans("xxqwyyquanwenz", &terms);
        assert_eq!(spans.len(), 2, "{spans:?}");
        assert!(spans[0].0 < spans[1].0);
        // 重叠取排序首个（与 make_snippet 窗口内规则一致）。
        let terms2 = vec!["全文".to_string(), "全文搜索".to_string()];
        let spans2 = match_spans("全文搜索好", &terms2);
        assert_eq!(spans2.len(), 1, "{spans2:?}");
    }

    #[test]
    fn windowed_snippet_pages_through_matches() {
        // 两处匹配间隔拉开（>120 字窗口），保证窗口内容不同。
        let pad: String = "垫".repeat(150);
        let text = format!("前马春天后{pad}左马春天右");
        let terms = terms_of("马春天");
        assert!(!terms.is_empty());
        let spans = match_spans(&text, &terms);
        assert!(spans.len() >= 2, "{spans:?}");
        let total = spans.len();
        let (h0, total0) = make_snippet_windowed(&text, &terms, 0).unwrap();
        let (hlast, total1) =
            make_snippet_windowed(&text, &terms, total - 1).unwrap();
        assert_eq!((total0, total1), (total, total));
        assert_ne!(h0, hlast, "首尾窗口应切到不同上下文");
        assert!(h0.contains("<b>"), "{h0}");
        assert!(hlast.contains("<b>"), "{hlast}");
        // 越界钳到最后一个。
        let (hclamp, _) = make_snippet_windowed(&text, &terms, total + 99).unwrap();
        assert_eq!(hclamp, hlast);
        // 无匹配返回 None。
        assert!(make_snippet_windowed("hello world", &terms, 0).is_none());
    }
}

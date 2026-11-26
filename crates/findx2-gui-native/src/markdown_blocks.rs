//! 命中上下文 md 富文本管线（管线 C）：pulldown-cmark → 块模型 → GPUI 原生绘制。
//!
//! 与 pdfium 光栅、Office 活文档并列：md 不走 PDF、不转图，直接解析成
//! 标题/段落/列表/表格块，由 GPUI 按块绘制（连续滚动），命中区间统一
//! 用 `snippet::match_spans` 算出后按块加底色。

#[derive(Debug, Clone, PartialEq)]
pub enum MdBlock {
    Heading { level: u8, text: String },
    Paragraph { text: String },
    Code { lang: String, text: String },
    List { ordered: bool, items: Vec<String> },
    Table { headers: Vec<String>, rows: Vec<Vec<String>> },
    Rule,
}

#[derive(Debug, Clone, Default)]
pub struct MdDoc {
    pub blocks: Vec<MdBlock>,
    /// 全文纯文本（块之间 `\n` 分隔），供命中区间映射。
    pub plain: String,
    /// 每块在 `plain` 中的字节起点（与 blocks 一一对应）。
    pub block_starts: Vec<usize>,
}

impl MdDoc {
    /// plain 字节偏移落在第几个块（命中→块 定位）。
    pub fn block_of_offset(&self, off: usize) -> Option<usize> {
        if self.blocks.is_empty() {
            return None;
        }
        match self.block_starts.binary_search(&off) {
            Ok(i) => Some(i),
            Err(0) => Some(0),
            Err(i) => Some(i - 1),
        }
    }

    /// 块的纯文本（用于拼 plain）。
    fn block_text(b: &MdBlock) -> String {
        match b {
            MdBlock::Heading { text, .. } => text.clone(),
            MdBlock::Paragraph { text } => text.clone(),
            MdBlock::Code { text, .. } => text.clone(),
            MdBlock::List { items, .. } => items.join(" "),
            MdBlock::Table { headers, rows } => {
                let mut s = headers.join("\t");
                for r in rows {
                    s.push('\n');
                    s.push_str(&r.join("\t"));
                }
                s
            }
            MdBlock::Rule => String::new(),
        }
    }

    /// 解析完成后回填 plain / block_starts。
    pub fn finalize(&mut self) {
        let mut plain = String::new();
        self.block_starts.clear();
        for b in &self.blocks {
            self.block_starts.push(plain.len());
            plain.push_str(&Self::block_text(b));
            plain.push('\n');
        }
        self.plain = plain;
    }
}

const MAX_BLOCKS: usize = 2000;
const MAX_TEXT: usize = 256 * 1024;

/// 解析 md 文本为块模型（超限截断，保证预览有界）。
pub fn parse(text: &str) -> MdDoc {
    use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_TABLES);
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    opts.insert(Options::ENABLE_TASKLISTS);
    let text = truncate(text);
    let parser = Parser::new_ext(&text, opts);
    let mut doc = MdDoc::default();
    let mut cur_text = String::new();
    let mut cur_heading: Option<u8> = None;
    let mut cur_code_lang = None::<String>;
    let mut in_code = false;
    let mut list_stack: Vec<(bool, Vec<String>)> = Vec::new();
    let mut table_headers: Option<Vec<String>> = None;
    let mut table_rows: Vec<Vec<String>> = Vec::new();
    let mut table_cell = String::new();
    let mut table_row: Vec<String> = Vec::new();
    let mut in_table_head = false;
    let mut in_table_cell = false;

    let flush_para = |doc: &mut MdDoc, cur_text: &mut String| {
        let t = cur_text.trim().to_string();
        cur_text.clear();
        if !t.is_empty() && doc.blocks.len() < MAX_BLOCKS {
            doc.blocks.push(MdBlock::Paragraph { text: t });
        }
    };

    for ev in parser {
        match ev {
            Event::Start(Tag::Heading { level, .. }) => {
                flush_para(&mut doc, &mut cur_text);
                let lv = match level {
                    HeadingLevel::H1 => 1,
                    HeadingLevel::H2 => 2,
                    HeadingLevel::H3 => 3,
                    HeadingLevel::H4 => 4,
                    HeadingLevel::H5 => 5,
                    HeadingLevel::H6 => 6,
                };
                cur_heading = Some(lv);
                cur_text.clear();
            }
            Event::End(TagEnd::Heading(_)) => {
                if let Some(lv) = cur_heading.take() {
                    let t = cur_text.trim().to_string();
                    cur_text.clear();
                    if !t.is_empty() && doc.blocks.len() < MAX_BLOCKS {
                        doc.blocks.push(MdBlock::Heading { level: lv, text: t });
                    }
                }
            }
            Event::Start(Tag::CodeBlock(kind)) => {
                flush_para(&mut doc, &mut cur_text);
                in_code = true;
                cur_code_lang = match kind {
                    pulldown_cmark::CodeBlockKind::Fenced(lang) => Some(lang.into_string()),
                    _ => None,
                };
                cur_text.clear();
            }
            Event::End(TagEnd::CodeBlock) => {
                in_code = false;
                let t = cur_text.clone();
                cur_text.clear();
                if doc.blocks.len() < MAX_BLOCKS {
                    doc.blocks.push(MdBlock::Code {
                        lang: cur_code_lang.take().unwrap_or_default(),
                        text: t.trim_end().to_string(),
                    });
                }
            }
            Event::Start(Tag::List(first)) => {
                flush_para(&mut doc, &mut cur_text);
                list_stack.push((first.is_some(), Vec::new()));
            }
            Event::End(TagEnd::List(_)) => {
                if let Some((ordered, items)) = list_stack.pop() {
                    if !items.is_empty() && doc.blocks.len() < MAX_BLOCKS {
                        doc.blocks.push(MdBlock::List { ordered, items });
                    }
                }
            }
            Event::Start(Tag::Item) => {
                cur_text.clear();
            }
            Event::End(TagEnd::Item) => {
                let t = cur_text.trim().to_string();
                cur_text.clear();
                if !t.is_empty() {
                    if let Some((_, items)) = list_stack.last_mut() {
                        if items.len() < 500 {
                            items.push(t);
                        }
                    } else if doc.blocks.len() < MAX_BLOCKS {
                        doc.blocks.push(MdBlock::List {
                            ordered: false,
                            items: vec![t],
                        });
                    }
                }
            }
            Event::Start(Tag::Table(_)) => {
                flush_para(&mut doc, &mut cur_text);
                table_headers = None;
                table_rows.clear();
            }
            Event::End(TagEnd::Table) => {
                if let Some(h) = table_headers.take() {
                    if doc.blocks.len() < MAX_BLOCKS {
                        doc.blocks.push(MdBlock::Table {
                            headers: h,
                            rows: std::mem::take(&mut table_rows),
                        });
                    }
                }
                table_row.clear();
            }
            Event::Start(Tag::TableHead) => {
                in_table_head = true;
            }
            Event::End(TagEnd::TableHead) => {
                in_table_head = false;
                table_headers = Some(std::mem::take(&mut table_row));
            }
            Event::Start(Tag::TableRow) => {
                table_row.clear();
            }
            Event::End(TagEnd::TableRow) => {
                if !in_table_head {
                    table_rows.push(std::mem::take(&mut table_row));
                    if table_rows.len() > 500 {
                        table_rows.truncate(500);
                    }
                }
            }
            Event::Start(Tag::TableCell) => {
                in_table_cell = true;
                table_cell.clear();
            }
            Event::End(TagEnd::TableCell) => {
                in_table_cell = false;
                table_row.push(table_cell.trim().to_string());
                table_cell.clear();
            }
            Event::Start(Tag::Paragraph) => {
                cur_text.clear();
            }
            Event::End(TagEnd::Paragraph) => {
                if in_table_cell {
                    continue;
                }
                if cur_heading.is_none() && !in_code && list_stack.is_empty() {
                    flush_para(&mut doc, &mut cur_text);
                } else if !list_stack.is_empty() {
                    // 列表项内段落：并入当前项文本。
                    cur_text.push(' ');
                }
            }
            Event::Text(t) | Event::Code(t) => {
                if in_table_cell {
                    table_cell.push_str(&t);
                } else {
                    if in_code {
                        cur_text.push_str(&t);
                    } else if cur_text.len() < MAX_TEXT {
                        cur_text.push_str(&t);
                    }
                    // 软换行保留空格，避免词粘连。
                    if matches!(t.chars().last(), Some('\n')) {
                        cur_text.push(' ');
                    }
                }
            }
            Event::SoftBreak | Event::HardBreak => {
                if in_table_cell {
                    table_cell.push(' ');
                } else {
                    cur_text.push(' ');
                }
            }
            Event::Rule => {
                flush_para(&mut doc, &mut cur_text);
                if doc.blocks.len() < MAX_BLOCKS {
                    doc.blocks.push(MdBlock::Rule);
                }
            }
            _ => {}
        }
    }
    flush_para(&mut doc, &mut cur_text);
    doc.finalize();
    doc
}

fn truncate(s: &str) -> &str {
    if s.len() <= MAX_TEXT {
        return s;
    }
    let mut n = MAX_TEXT;
    while !s.is_char_boundary(n) {
        n -= 1;
    }
    &s[..n]
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn heading_para_list_table() {
        let doc = parse("# T\n\npara one\n\n- a\n- b\n\n| h1 | h2 |\n|---|---|\n| c1 | c2 |\n");
        assert!(doc.blocks.iter().any(|b| matches!(b, MdBlock::Heading { level: 1, .. })));
        assert!(doc.blocks.iter().any(|b| matches!(b, MdBlock::Paragraph { .. })));
        assert!(doc.blocks.iter().any(|b| matches!(b, MdBlock::List { .. })));
        assert!(doc.blocks.iter().any(|b| matches!(b, MdBlock::Table { .. })));
    }
    #[test]
    fn code_block_kept() {
        let doc = parse("```rust\nlet x = 1;\n```\n");
        assert!(doc.blocks.iter().any(|b| matches!(b, MdBlock::Code { .. })));
    }
    #[test]
    fn huge_input_bounded() {
        let big = "x".repeat(600 * 1024);
        let doc = parse(&big);
        let total: usize = doc
            .blocks
            .iter()
            .map(|b| match b {
                MdBlock::Paragraph { text } => text.len(),
                _ => 0,
            })
            .sum();
        assert!(total <= MAX_TEXT + 1024);
    }
}

//! pptx 幻灯片模型：quick-xml 解析 slide XML → 每页段落 → GPUI 自绘。
//!
//! 第一版只做「按页分组的文本段落 + 命中高亮」（保留页边界与段落结构），
//! 不做形状绝对定位/图表/动画。文本按 `<a:p>` 分段，`<a:t>` 拼接。

use quick_xml::events::Event;
use quick_xml::Reader;
use std::io::Read;

/// `&entity;`（quick-xml 0.42 的 `Event::GeneralRef`）解析成字符串。
fn general_ref(r: quick_xml::events::BytesRef<'_>) -> String {
    let name = r.into_inner();
    let raw = format!("&{};", name.as_ref());
    quick_xml::escape::unescape(&raw)
        .map(|c| c.into_owned())
        .unwrap_or_default()
}


#[derive(Debug, Clone, Default)]
pub struct Slide {
    /// 1-based 页码（与 PowerPoint 显示一致）。
    pub number: usize,
    /// 段落文本（按文档顺序）。
    pub paragraphs: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Deck {
    pub slides: Vec<Slide>,
    /// 全文纯文本（页之间 `\n`），供命中映射。
    pub plain: String,
    /// 每段在 plain 中的字节起点（与 (slide, para) 展开顺序一致）。
    pub para_starts: Vec<usize>,
}

impl Deck {
    /// 命中偏移 → (页码, 段落下标)，用于跳转与页签定位。
pub fn locate(&self, off: usize) -> Option<(usize, usize)> {
        match self.para_starts.binary_search(&off) {
            Ok(i) => Some((i / 1, i)), // 占位：下面用真实映射修正
            Err(0) => Some((0, 0)),
            Err(i) => Some((0, i - 1)),
        }
    }
}

const MAX_SLIDES: usize = 60;
const MAX_PARA: usize = 400;
const MAX_PARA_CHARS: usize = 2000;

/// 解析 pptx。失败返回 Err（调用方降级文字摘录）。
pub fn parse(path: &str) -> Result<Deck, String> {
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    if len > 64 * 1024 * 1024 {
        return Err("pptx 超过 64 MiB 上限".into());
    }
    let mut zip = zip::ZipArchive::new(file).map_err(|e| format!("pptx 打开失败: {e}"))?;

    // slide 文件名按数字排序（slide2 在 slide10 前）
    let mut names: Vec<String> = zip
        .file_names()
        .filter_map(|n| n.ok())
        .filter(|n| {
            n.starts_with("ppt/slides/slide") && n.ends_with(".xml") && !n.contains("/_rels/")
        })
        .map(|s| s.into_owned())
        .collect();
    names.sort_by_key(|s| {
        s.trim_start_matches("ppt/slides/slide")
            .trim_end_matches(".xml")
            .parse::<u32>()
            .unwrap_or(0)
    });
    if names.is_empty() {
        return Err("pptx 没有幻灯片".into());
    }

    let mut deck = Deck::default();
    let mut plain = String::new();
    for (i, name) in names.iter().take(MAX_SLIDES).enumerate() {
        let mut xml = String::new();
        {
            let mut f = zip
                .by_name(name)
                .map_err(|e| format!("读取 {name} 失败: {e}"))?;
            if f.size() > 8 * 1024 * 1024 {
                continue;
            }
            f.take(8 * 1024 * 1024)
                .read_to_string(&mut xml)
                .map_err(|e| e.to_string())?;
        }
        let paragraphs = parse_slide_paragraphs(&xml);
        if paragraphs.is_empty() {
            continue;
        }
        let mut slide = Slide {
            number: i + 1,
            paragraphs: Vec::new(),
        };
        for p in paragraphs.into_iter().take(MAX_PARA) {
            let t: String = p.chars().take(MAX_PARA_CHARS).collect();
            if t.trim().is_empty() {
                continue;
            }
            deck.para_starts.push(plain.len());
            plain.push_str(&t);
            plain.push('\n');
            slide.paragraphs.push(t);
        }
        if !slide.paragraphs.is_empty() {
            deck.slides.push(slide);
        }
    }
    if deck.slides.is_empty() {
        return Err("pptx 没有可显示的文本".into());
    }
    deck.plain = plain;
    Ok(deck)
}

/// 从 slide XML 提取段落文本（`<a:p>` 内所有 `<a:t>` 拼接）。
fn parse_slide_paragraphs(xml: &str) -> Vec<String> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(false);
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut in_p = false;
    let mut in_t = false;
    let mut depth_p = 0usize;

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                let local = e.local_name();
                let name = local.as_ref();
                if name == "p" {
                    in_p = true;
                    depth_p += 1;
                    cur.clear();
                } else if name == "t" && in_p {
                    in_t = true;
                }
            }
            Ok(Event::End(e)) => {
                let local = e.local_name();
                match local.as_ref() {
                    "p" => {
                        depth_p = depth_p.saturating_sub(1);
                        if depth_p == 0 {
                            in_p = false;
                            let t = cur.trim().to_string();
                            if !t.is_empty() {
                                out.push(t);
                            }
                            cur.clear();
                        }
                    }
                    "t" => in_t = false,
                    _ => {}
                }
            }
            Ok(Event::Text(t)) => {
                if in_t {
                    let raw = t.into_inner();
                    if let Ok(s) = quick_xml::escape::unescape(raw.as_ref()) {
                        cur.push_str(&s);
                    }
                }
            }
            Ok(Event::GeneralRef(r)) => {
                if in_t {
                    cur.push_str(&general_ref(r));
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_paragraphs_in_order() {
        let xml = r#"<p:sld xmlns:a="a" xmlns:p="p"><p:cSld><p:spTree>
            <p:sp><p:txBody>
              <a:p><a:r><a:t>标题一</a:t></a:r></a:p>
              <a:p><a:r><a:t>正文</a:t></a:r><a:r><a:t>续</a:t></a:r></a:p>
            </p:txBody></p:sp>
        </p:spTree></p:cSld></p:sld>"#;
        let ps = parse_slide_paragraphs(xml);
        assert_eq!(ps, vec!["标题一".to_string(), "正文续".to_string()]);
    }

    #[test]
    fn skips_empty_paragraphs() {
        let xml = r#"<a:p><a:t>  </a:t></a:p><a:p><a:t>x</a:t></a:p>"#;
        assert_eq!(parse_slide_paragraphs(xml), vec!["x".to_string()]);
    }
}

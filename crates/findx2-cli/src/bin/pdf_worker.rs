//! pdf-worker：PDF 文本抽取独立进程（被内容索引进程池调用）。
//!
//! 为什么独立进程：pdfium 原生库在多线程进程内并发使用会挂起整个进程
//!（实测：16 worker 首批并发直接 0 CPU 卡死；串行 init 也救不回来）。
//! 独立单线程进程天然免疫：崩/挂只影响单个文件，父进程超时 kill + 重启
//! worker 即可。AnyTXT 的 ATDHelper* 也是同构设计。
//!
//! 协议（stdin/stdout，二进制帧）：
//! - 父 → 子：每行一个 UTF-8 路径（`\n` 分隔）；stdin EOF 则退出。
//! - 子 → 父：u64 LE 长度 + 文本字节；长度为 `u64::MAX` 表示本文件失败。
//! - 子进程启动即绑定 pdfium（exe 旁 `pdfium.dll`，备选系统库）；绑定失败
//!   直接 exit(1)，父进程降级走纯 Rust 兜底。

use std::io::{BufRead, BufReader, Read, Write};

fn main() {
    let mut args = std::env::args();
    let _exe = args.next();
    if args.next().as_deref() == Some("--boxes") {
        if let Err(e) = boxes_run() {
            eprintln!("pdf-worker boxes fatal: {e}");
            std::process::exit(1);
        }
        return;
    }
    if let Err(e) = run() {
        eprintln!("pdf-worker fatal: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let pdfium = bind().ok_or_else(|| "pdfium 不可用".to_string())?;
    let stdin = std::io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let mut line = Vec::new();
    loop {
        line.clear();
        let n = reader
            .read_until(b'\n', &mut line)
            .map_err(|e| format!("stdin 读取失败: {e}"))?;
        if n == 0 {
            break; // EOF：父进程结束任务，退出。
        }
        while line.last() == Some(&b'\n') || line.last() == Some(&b'\r') {
            line.pop();
        }
        let text = extract_one(&pdfium, &line);
        match text {
            Some(t) => {
                let b = t.into_bytes();
                out.write_all(&(b.len() as u64).to_le_bytes())
                    .map_err(|e| format!("stdout 写入失败: {e}"))?;
                out.write_all(&b)
                    .map_err(|e| format!("stdout 写入失败: {e}"))?;
            }
            None => {
                out.write_all(&u64::MAX.to_le_bytes())
                    .map_err(|e| format!("stdout 写入失败: {e}"))?;
            }
        }
        out.flush()
            .map_err(|e| format!("stdout 刷新失败: {e}"))?;
    }
    Ok(())
}

/// 命中框模式（预览高亮用，常驻单进程，串行服务）：
/// argv `--boxes`；stdin 每行一个 JSON job
/// `{"path":"...","page":0,"needles":["..."]}`；
/// stdout 每 job 回 `u64 LE 长度 + JSON`
/// `{"rects":[[x0,y0,x1,y1]...],"page_w":w,"page_h":h}`（点坐标，左上原点），
/// 长度 `u64::MAX` 表示本 job 失败；stdin EOF 则退出。
fn boxes_run() -> Result<(), String> {
    use std::io::BufRead;
    let pdfium = bind().ok_or_else(|| "pdfium 不可用".to_string())?;
    let stdin = std::io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let mut line = Vec::new();
    loop {
        line.clear();
        let n = reader
            .read_until(b'\n', &mut line)
            .map_err(|e| format!("stdin 读取失败: {e}"))?;
        if n == 0 {
            break;
        }
        let reply: Option<Vec<u8>> = match serde_json::from_slice::<BoxesJob>(&line) {
            Ok(job) => boxes_one(&pdfium, &job).and_then(|r| serde_json::to_vec(&r).ok()),
            Err(_) => None,
        };
        match reply {
            Some(b) => {
                out.write_all(&(b.len() as u64).to_le_bytes())
                    .map_err(|e| format!("stdout 写入失败: {e}"))?;
                out.write_all(&b)
                    .map_err(|e| format!("stdout 写入失败: {e}"))?;
            }
            None => {
                out.write_all(&u64::MAX.to_le_bytes())
                    .map_err(|e| format!("stdout 写入失败: {e}"))?;
            }
        }
        out.flush()
            .map_err(|e| format!("stdout 刷新失败: {e}"))?;
    }
    Ok(())
}

#[derive(serde::Deserialize)]
struct BoxesJob {
    path: String,
    page: u32,
    needles: Vec<String>,
}

#[derive(serde::Serialize)]
struct BoxesReply {
    rects: Vec<[f32; 4]>,
    page_w: f32,
    page_h: f32,
}

/// 单页命中框：逐字重建文本（保证字↔框 1:1，不依赖 `all()` 的 rect 抽取顺序），
/// 命中 spans 复用 `snippet::match_spans`（字面优先 + 拼音对齐，与摘要高亮同口径），
/// 合并重叠命中后取并框；单页上限 2000 框（交互 sanity）。
fn boxes_one(
    pdfium: &pdfium_render::prelude::Pdfium,
    job: &BoxesJob,
) -> Option<BoxesReply> {
    use pdfium_render::prelude::*;
    if job.page > 100_000 || job.needles.iter().all(|s| s.is_empty()) {
        return None;
    }
    let document = pdfium.load_pdf_from_file(&job.path, None).ok()?;
    let page = document.pages().get(job.page as PdfPageIndex).ok()?;
    let (page_w, page_h) = (page.width().value, page.height().value);
    if !(page_w > 0.0 && page_h > 0.0) {
        return None;
    }
    let text = page.text().ok()?;
    let n = text.len() as usize;
    let chars = text.chars();
    let mut s = String::with_capacity(n);
    let mut boxes: Vec<Option<[f32; 4]>> = Vec::with_capacity(n);
    for i in 0..n {
        let c = chars.get(i as PdfPageTextCharIndex).ok()?;
        s.push(c.unicode_char().unwrap_or('\u{FFFD}'));
        let r = match c.loose_bounds() {
            Ok(b) => Some([b.left().value, page_h - b.top().value, b.right().value, page_h - b.bottom().value]),
            Err(_) => None,
        };
        boxes.push(r);
    }
    // 命中 spans 与「命中上下文」同一套口径（`snippet::match_spans`）：
    // 字面优先；纯拼音查询（如 `machuntian`）按拼音对齐到原文汉字（“马春天”），
    // 否则 PDF 页内永远找不到框——用户实测「全文模式 + 拼音词」高亮缺失的根因。
    let mut spans = findx2_content::snippet::match_spans(&s, &job.needles);
    if spans.is_empty() {
        return Some(BoxesReply { rects: Vec::new(), page_w, page_h });
    }
    // 字节偏移→字下标（char 边界对齐，单遍填充）。
    let mut byte_to_char = vec![0usize; s.len() + 1];
    let mut total_chars = 0usize;
    for (b, ch) in s.char_indices() {
        byte_to_char[b] = total_chars;
        for k in 1..ch.len_utf8() {
            if b + k <= s.len() {
                byte_to_char[b + k] = total_chars;
            }
        }
        total_chars += 1;
    }
    byte_to_char[s.len()] = total_chars;
    spans.sort_unstable();
    let mut merged: Vec<(usize, usize)> = Vec::with_capacity(spans.len());
    for (a, b) in spans {
        if let Some(last) = merged.last_mut() {
            if a <= last.1 {
                last.1 = last.1.max(b);
                continue;
            }
        }
        merged.push((a, b));
    }
    let mut rects = Vec::new();
    for (a, b) in merged {
        let (ca, cb) = (byte_to_char[a.min(s.len())], byte_to_char[b.min(s.len())]);
        let mut x0 = f32::INFINITY;
        let mut y0 = f32::INFINITY;
        let mut x1 = f32::NEG_INFINITY;
        let mut y1 = f32::NEG_INFINITY;
        for bb in boxes.iter().take(cb).skip(ca).flatten() {
            x0 = x0.min(bb[0]);
            y0 = y0.min(bb[1]);
            x1 = x1.max(bb[2]);
            y1 = y1.max(bb[3]);
        }
        if x1 > x0 && y1 > y0 {
            rects.push([x0, y0, x1, y1]);
        }
        if rects.len() >= 2000 {
            break;
        }
    }
    Some(BoxesReply { rects, page_w, page_h })
}

fn bind() -> Option<pdfium_render::prelude::Pdfium> {
    use pdfium_render::prelude::Pdfium;
    let dll = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("pdfium.dll")));
    if let Some(d) = dll {
        if d.is_file() {
            if let Ok(b) = Pdfium::bind_to_library(&d) {
                return Some(Pdfium::new(b));
            }
        }
    }
    Pdfium::bind_to_system_library()
        .ok()
        .map(Pdfium::new)
}

fn extract_one(pdfium: &pdfium_render::prelude::Pdfium, raw_path: &[u8]) -> Option<String> {
    use pdfium_render::prelude::*;
    let path = std::str::from_utf8(raw_path).ok()?;
    let document = pdfium.load_pdf_from_file(path, None).ok()?;
    let mut out = String::new();
    for page in document.pages().iter() {
        out.push_str(&page.text().ok()?.all());
        out.push('\n');
    }
    if out.trim().is_empty() {
        return None;
    }
    Some(out)
}

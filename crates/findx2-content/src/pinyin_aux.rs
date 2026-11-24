//! 拼音辅索引回填：从主倒排直接重建拼音文本，不重读业务文件。
//!
//! 为什么不用重新抽取：主索引的 postings 里已有（term, positions），
//! 按位置拼回原文 CJK 字序 → 转拼音 → 写辅库。只读倒排（顺序 IO），
//! 不碰业务文件：无云 hydration、无权限失败、无解析开销。
//!
//! 代价：同一份 postings 按 shard 读多遍（页缓存命中后是内存速度）。
//! 以 shard=50000 切分，单 shard 常驻约 <150MB。
//!
//! 保真说明（与增量路径的 restrictions 一致）：
//! - RemoveLongFilter(40) 丢掉的超长 token 不参与（多为 URL/哈希，非中文）；
//! - 标点在分词时已丢弃，分块按固定窗口（见 CHUNK_SYLLABLES），增量路径同算法；
//! - 多音字取第一读音（与增量一致）。

use std::collections::{HashMap, HashSet};

use pinyin::ToPinyin;
use tantivy::schema::{Field, IndexRecordOption};
use tantivy::{DocAddress, DocSet, TERMINATED};
use tantivy::postings::Postings;
use tantivy::schema::Value;

use crate::{open_or_create, ContentIndex, DocMeta, Error, Result};

/// 辅库目录后缀（主库 `index.bin.content` 旁边）。
pub const AUX_INITIALS_SUFFIX: &str = ".pinyin-initials";
pub const AUX_FULL_SUFFIX: &str = ".pinyin-full";

/// initials 分块：每 N 个音节一切（前缀匹配的粒度）。
pub const CHUNK_SYLLABLES: usize = 20;
/// 按 doc 切片的大小（定界内存）。
pub const DOC_SHARD: u32 = 50_000;
/// 全拼单篇上限（字符数；病态大文档截断，构建不爆内存）。
pub const MAX_FULL_CHARS: usize = 500_000;

pub fn aux_dir_for(index_bin: &std::path::Path, suffix: &str) -> std::path::PathBuf {
    let mut dir = crate::content_dir_for(index_bin).as_os_str().to_owned();
    dir.push(suffix);
    std::path::PathBuf::from(dir)
}

#[derive(Debug, Default)]
pub struct AuxStats {
    pub docs: u64,
    pub terms_cjk: u64,
    pub chars: u64,
}

/// 单个 CJK 字符 → (全拼, 首字母)；非 CJK 返回 None。
fn pinyin_of(ch: char) -> Option<(&'static str, char)> {
    let s = ch.to_string();
    let mut it = s.as_str().to_pinyin();
    match it.next() {
        Some(Some(p)) => p.first_letter().chars().next().map(|c| (p.plain(), c)),
        _ => None,
    }
}

fn is_cjk(c: char) -> bool {
    // CJK 统一表意 + 扩展 A~G + 兼容区 + 日文假名/谚文（顺带覆盖）。
    matches!(c,
        '\u{3400}'..='\u{4DBF}' | '\u{4E00}'..='\u{9FFF}' |
        '\u{20000}'..='\u{2EBEF}' | '\u{30000}'..='\u{3134F}' |
        '\u{F900}'..='\u{FAFF}' | '\u{3040}'..='\u{30FF}' |
        '\u{AC00}'..='\u{D7AF}')
}

/// CJK 字序 → initials 文本（每 CHUNK_SYLLABLES 个音节一块，块内首字母直连）。
pub fn initials_text(chars: &[char]) -> String {
    let mut out = String::new();
    let mut count = 0usize;
    for ch in chars {
        let Some((_, first)) = pinyin_of(*ch) else {
            continue;
        };
        if count > 0 && count % CHUNK_SYLLABLES == 0 {
            out.push(' ');
        }
        out.push(first);
        count += 1;
    }
    out
}

/// CJK 字序 → 全拼文本（空格分隔；超限截断）。
pub fn full_text(chars: &[char]) -> String {
    let mut out = String::new();
    let mut count = 0usize;
    for ch in chars {
        if count >= MAX_FULL_CHARS {
            break;
        }
        let Some((plain, _)) = pinyin_of(*ch) else {
            continue;
        };
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(plain);
        count += 1;
    }
    out
}

/// 从主索引倒排回填拼音辅库（initials + full 一次走完）。
/// `progress(scanned_docs, aux_docs)` 返回 false 即中止。
pub fn build_aux_from_index(
    index_bin: &std::path::Path,
    rebuild: bool,
    mut progress: impl FnMut(u64, u64) -> bool,
) -> Result<AuxStats> {
    let main_cdir = crate::content_dir_for(index_bin);
    let ini_dir = aux_dir_for(index_bin, AUX_INITIALS_SUFFIX);
    let full_dir = aux_dir_for(index_bin, AUX_FULL_SUFFIX);
    if rebuild {
        for d in [&ini_dir, &full_dir] {
            if d.exists() {
                std::fs::remove_dir_all(d)
                    .map_err(|e| Error::Io(d.display().to_string(), e))?;
            }
        }
    }
    // 只读打开主库（不建 writer，不抢服务持有的写锁）。
    let main_index = tantivy::Index::open_in_dir(&main_cdir)
        .map_err(|e| Error::Index(format!("打开主内容索引失败: {e}")))?;
    if crate::index::read_schema_version(&main_cdir) != Some(crate::SCHEMA_VERSION) {
        return Err(Error::Index("主内容索引版本不匹配，先重建主库".into()));
    }
    let reader = main_index
        .reader_builder()
        .reload_policy(tantivy::ReloadPolicy::Manual)
        .try_into()
        .map_err(Error::from)?;
    let searcher = reader.searcher();
    let body_field: Field = main_index
        .schema()
        .get_field("body")
        .map_err(|e| Error::Index(format!("主索引缺 body 字段: {e}")))?;
    // 存储字段 ID：与 build_schema() 同构（同版本 schema，ID 一致）。
    let (_, store_fields) = crate::index::build_schema();
    let aux_ini = open_or_create(&ini_dir)?;
    let aux_full = open_or_create(&full_dir)?;
    let mut st = AuxStats {
        docs: 0,
        terms_cjk: 0,
        chars: 0,
    };
    let mut scanned: u64 = 0;
    for (ord, seg) in searcher.segment_readers().iter().enumerate() {
        let segment_ord = ord as u32;
        let alive: HashSet<u32> = seg.doc_ids_alive().collect();
        let inv = seg
            .inverted_index(body_field)
            .map_err(|e| Error::Index(format!("打开倒排失败: {e}")))?;
        let max_doc = seg.max_doc();
        let mut shard_start = 0u32;
        while shard_start < max_doc {
            let shard_end = (shard_start + DOC_SHARD).min(max_doc);
            let n = walk_shard(
                &inv,
                body_field,
                &searcher,
                segment_ord,
                &alive,
                &store_fields,
                shard_start,
                shard_end,
                &aux_ini,
                &aux_full,
                &mut st,
            )?;
            st.docs += n;
            scanned += (shard_end - shard_start) as u64;
            if !progress(scanned, st.docs) {
                return Err(Error::Cancelled);
            }
            shard_start = shard_end;
        }
    }
    for aux in [&aux_ini, &aux_full] {
        let _ = aux.commit();
        // 终局合并尽力而为（小库，很快）；失败不影响使用。
        let _ = aux.optimize();
        let _ = aux.commit();
    }
    Ok(st)
}


/// 单个 shard：收集该 doc 范围内所有 CJK token (pos, term)，重建字序，写辅库。
/// 返回本 shard 写入的 aux doc 数。
#[allow(clippy::too_many_arguments)]
fn walk_shard(
    inv: &tantivy::index::InvertedIndexReader,
    body_field: Field,
    searcher: &tantivy::Searcher,
    segment_ord: u32,
    alive: &HashSet<u32>,
    fields: &crate::index::ContentFields,
    shard_start: u32,
    shard_end: u32,
    aux_ini: &ContentIndex,
    aux_full: &ContentIndex,
    st: &mut AuxStats,
) -> Result<u64> {
    use rayon::prelude::*;
    // term 序表（shard 内复用，避免重复 clone 字符串）。
    let mut terms: Vec<String> = Vec::new();
    // doc → [(byte_pos, term_idx)]。
    let mut per_doc: HashMap<u32, Vec<(u32, u32)>> = HashMap::new();
    let mut stream = inv
        .terms()
        .stream()
        .map_err(|e| Error::Index(format!("term 流打开失败: {e}")))?;
    let mut positions: Vec<u32> = Vec::new();
    while stream.advance() {
        let term_bytes = stream.key();
        // CJK 预检：无 >=0x80 字节则必为纯 ASCII（拼音无关），跳过。
        if !term_bytes.iter().any(|&b| b >= 0x80) {
            continue;
        }
        let term_text = match std::str::from_utf8(term_bytes) {
            Ok(s) => s,
            Err(_) => continue,
        };
        if !term_text.chars().any(is_cjk) {
            continue;
        }
        st.terms_cjk += 1;
        let term = tantivy::Term::from_field_bytes(body_field, term_bytes);
        let Some(mut postings) = inv
            .read_postings(&term, IndexRecordOption::WithFreqsAndPositions)
            .map_err(|e| Error::Index(format!("读 postings 失败: {e}")))?
        else {
            continue;
        };
        let term_idx = terms.len() as u32;
        terms.push(term_text.to_string());
        let mut doc = postings.doc();
        while doc != TERMINATED {
            if doc >= shard_end {
                break;
            }
            if doc >= shard_start && alive.contains(&doc) {
                positions.clear();
                postings.positions(&mut positions);
                let entry = per_doc.entry(doc).or_default();
                for &p in positions.iter() {
                    entry.push((p, term_idx));
                }
            }
            doc = postings.advance();
        }
    }
    if per_doc.is_empty() {
        return Ok(0);
    }
    // 重建字序 → 拼音文本 → 批量写入（并行，aux writer 线程安全）。
    let items: Vec<(DocMeta, String, String)> = per_doc
        .par_iter()
        .filter_map(|(&doc_id, tokpos)| {
            let chars = match rebuild_chars(tokpos, &terms) {
                Some(c) if !c.is_empty() => c,
                _ => return None,
            };
            if chars.is_empty() {
                return None;
            }
            let doc: tantivy::TantivyDocument =
                searcher.doc(DocAddress::new(segment_ord, doc_id)).ok()?;
            let meta = DocMeta {
                frn: doc.get_first(fields.frn).and_then(|v| v.as_u64()).unwrap_or(0),
                volume_id: doc.get_first(fields.volume).and_then(|v| v.as_str()).unwrap_or("").to_owned(),
                path: doc.get_first(fields.path).and_then(|v| v.as_str()).unwrap_or("").to_owned(),
                mtime: doc.get_first(fields.mtime).and_then(|v| v.as_u64()).unwrap_or(0),
                size: doc.get_first(fields.size).and_then(|v| v.as_u64()).unwrap_or(0),
            };
            let ini = initials_text(&chars);
            if ini.is_empty() {
                return None;
            }
            let full = full_text(&chars);
            Some((meta, ini, full))
        })
        .collect();
    st.chars += items.len() as u64;
    let mut n = 0u64;
    for (meta, ini, full) in &items {
        let ok_ini = aux_ini.index_doc(meta, ini).is_ok();
        let ok_full = if full.is_empty() {
            true
        } else {
            aux_full.index_doc(meta, full).is_ok()
        };
        if ok_ini && ok_full {
            n += 1;
        }
    }
    Ok(n)
}

/// 按字节偏移贪心覆盖，还原原文 CJK 字序（重叠 token 取最长）。
fn rebuild_chars(tokpos: &[(u32, u32)], terms: &[String]) -> Option<Vec<char>> {
    if tokpos.is_empty() {
        return None;
    }
    let mut v: Vec<(u32, u32)> = tokpos.to_vec();
    // 同一起点取最长，起点升序。
    v.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)));
    let mut out = String::new();
    let mut covered = 0u32;
    for (pos, ti) in v {
        let t = terms.get(ti as usize)?;
        let blen = t.len() as u32;
        if pos < covered {
            continue;
        }
        // 缺口（被丢弃的标点/超长 token）：直接跳过，不补。
        out.push_str(t);
        covered = pos + blen;
    }
    if out.is_empty() {
        return None;
    }
    Some(out.chars().filter(|c| is_cjk(*c)).collect())
}

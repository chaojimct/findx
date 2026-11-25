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
    pub docs_ini: u64,
    pub docs_full: u64,
}

/// 打开已建好的辅库（不存在/版本不对 → None，不报错；调用方视为未建）。
pub fn open_aux(index_bin: &std::path::Path) -> (Option<ContentIndex>, Option<ContentIndex>) {
    let ini = aux_dir_for(index_bin, AUX_INITIALS_SUFFIX);
    let full = aux_dir_for(index_bin, AUX_FULL_SUFFIX);
    let open_if_compat = |dir: &std::path::PathBuf| -> Option<ContentIndex> {
        if !dir.join("meta.json").is_file() {
            return None;
        }
        if crate::index::read_schema_version(dir) != Some(crate::SCHEMA_VERSION) {
            return None;
        }
        open_or_create(dir).ok()
    };
    (open_if_compat(&ini), open_if_compat(&full))
}

/// 文本 → (initials 全文, 全拼全文)：增量路径共用（与回填同一算法）。
pub fn pinyin_texts(text: &str) -> (String, String) {
    let chars: Vec<char> = text.chars().filter(|c| is_cjk(*c)).collect();
    if chars.is_empty() {
        return (String::new(), String::new());
    }
    (initials_text(&chars), full_text(&chars))
}

/// 单个 CJK 字符 → (全拼, 首字母)；非 CJK 返回 None。
pub(crate) fn pinyin_of(ch: char) -> Option<(&'static str, char)> {
    let s = ch.to_string();
    let mut it = s.as_str().to_pinyin();
    match it.next() {
        Some(Some(p)) => p.first_letter().chars().next().map(|c| (p.plain(), c)),
        _ => None,
    }
}

pub(crate) fn is_cjk(c: char) -> bool {
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

/// 标准无调拼音节集合（含 `lv`/`nv`/`lue`/`nue` 写法；`ü` 按 `v` 归一）。
///
/// 覆盖 GB/T 16159 常用音节 + 口语残留（`dei`/`den`/`kei`/`lo` 等），贪心最长
/// 匹配够用；漏收的生僻拼写走“已切分头 + 尾部前缀”降级，不会零命中。
pub(crate) fn is_pinyin_syllable(s: &str) -> bool {
    matches!(s,
        "a" | "ai" | "an" | "ang" | "ao" | "ba" | "bai" | "ban" | "bang" | "bao"
        | "bei" | "ben" | "beng" | "bi" | "bian" | "biao" | "bie" | "bin" | "bing"
        | "bo" | "bu" | "ca" | "cai" | "can" | "cang" | "cao" | "ce" | "cen" | "ceng"
        | "cha" | "chai" | "chan" | "chang" | "chao" | "che" | "chen" | "cheng"
        | "chi" | "chong" | "chou" | "chu" | "chuai" | "chuan" | "chuang" | "chui"
        | "chun" | "chuo" | "ci" | "cong" | "cou" | "cu" | "cuan" | "cui" | "cun"
        | "cuo" | "da" | "dai" | "dan" | "dang" | "dao" | "de" | "dei" | "den"
        | "deng" | "di" | "dian" | "diao" | "die" | "ding" | "diu" | "dong" | "dou"
        | "du" | "duan" | "dui" | "dun" | "duo" | "e" | "ei" | "en" | "eng" | "er"
        | "fa" | "fan" | "fang" | "fei" | "fen" | "feng" | "fo" | "fou" | "fu"
        | "ga" | "gai" | "gan" | "gang" | "gao" | "ge" | "gei" | "gen" | "geng"
        | "gong" | "gou" | "gu" | "gua" | "guai" | "guan" | "guang" | "gui" | "gun"
        | "guo" | "ha" | "hai" | "han" | "hang" | "hao" | "he" | "hei" | "hen"
        | "heng" | "hong" | "hou" | "hu" | "hua" | "huai" | "huan" | "huang" | "hui"
        | "hun" | "huo" | "ji" | "jia" | "jian" | "jiang" | "jiao" | "jie" | "jin"
        | "jing" | "jiong" | "jiu" | "ju" | "juan" | "jue" | "jun" | "ka" | "kai"
        | "kan" | "kang" | "kao" | "ke" | "kei" | "ken" | "keng" | "kong" | "kou"
        | "ku" | "kua" | "kuai" | "kuan" | "kuang" | "kui" | "kun" | "kuo" | "la"
        | "lai" | "lan" | "lang" | "lao" | "le" | "lei" | "leng" | "li" | "lia"
        | "lian" | "liang" | "liao" | "lie" | "lin" | "ling" | "liu" | "long"
        | "lou" | "lu" | "luan" | "lun" | "luo" | "lue" | "lv" | "ma" | "mai"
        | "man" | "mang" | "mao" | "me" | "mei" | "men" | "meng" | "mi" | "mian"
        | "miao" | "mie" | "min" | "ming" | "miu" | "mo" | "mou" | "mu" | "na"
        | "nai" | "nan" | "nang" | "nao" | "ne" | "nei" | "nen" | "neng" | "ni"
        | "nian" | "niang" | "niao" | "nie" | "nin" | "ning" | "niu" | "nong"
        | "nou" | "nu" | "nuan" | "nun" | "nuo" | "nue" | "nv" | "o" | "ou" | "pa"
        | "pai" | "pan" | "pang" | "pao" | "pei" | "pen" | "peng" | "pi" | "pian"
        | "piao" | "pie" | "pin" | "ping" | "po" | "pou" | "pu" | "qi" | "qia"
        | "qian" | "qiang" | "qiao" | "qie" | "qin" | "qing" | "qiong" | "qiu"
        | "qu" | "quan" | "que" | "qun" | "ran" | "rang" | "rao" | "re" | "ren"
        | "reng" | "ri" | "rong" | "rou" | "ru" | "ruan" | "rui" | "run" | "ruo"
        | "sa" | "sai" | "san" | "sang" | "sao" | "se" | "sen" | "seng" | "sha"
        | "shai" | "shan" | "shang" | "shao" | "she" | "shei" | "shen" | "sheng"
        | "shi" | "shou" | "shu" | "shua" | "shuai" | "shuan" | "shuang" | "shui"
        | "shun" | "shuo" | "si" | "song" | "sou" | "su" | "suan" | "sui" | "sun"
        | "suo" | "ta" | "tai" | "tan" | "tang" | "tao" | "te" | "tei" | "teng"
        | "ti" | "tian" | "tiao" | "tie" | "ting" | "tong" | "tou" | "tu" | "tuan"
        | "tui" | "tun" | "tuo" | "wa" | "wai" | "wan" | "wang" | "wei" | "wen"
        | "weng" | "wo" | "wu" | "xi" | "xia" | "xian" | "xiang" | "xiao" | "xie"
        | "xin" | "xing" | "xiong" | "xiu" | "xu" | "xuan" | "xue" | "xun" | "ya"
        | "yan" | "yang" | "yao" | "ye" | "yi" | "yin" | "ying" | "yo" | "yong"
        | "you" | "yu" | "yuan" | "yue" | "yun" | "za" | "zai" | "zan" | "zang"
        | "zao" | "ze" | "zei" | "zen" | "zeng" | "zha" | "zhai" | "zhan" | "zhang"
        | "zhao" | "zhe" | "zhei" | "zhen" | "zheng" | "zhi" | "zhong" | "zhou"
        | "zhu" | "zhua" | "zhuai" | "zhuan" | "zhuang" | "zhui" | "zhun" | "zhuo"
        | "zi" | "zong" | "zou" | "zu" | "zuan" | "zui" | "zun" | "zuo")
}

/// 连写全拼切音节（如 `quanwen` → 头 `["quan"]` + 尾 `"wen"` 全切；`quanwe` →
/// 头 `["quan"]` + 尾 `"we"` 待补全；`qw` → `None` 走首字母正则）。
///
/// 只接受纯小写字母（调用方保证已小写、无空格）；含其他字符直接 `None`。
/// 返回 `(已切完整音节, 剩余尾巴)`；完整切分时尾巴为空串。
/// 全切失败且一个完整音节都拿不到时返回 `None`（首字母/垃圾输入走原正则路）。
pub fn split_pinyin_query(q: &str) -> Option<(Vec<String>, String)> {
    if q.is_empty() || !q.bytes().all(|b| b.is_ascii_lowercase()) {
        return None;
    }
    let mut heads: Vec<String> = Vec::new();
    let mut pos = 0usize;
    let bytes = q.as_bytes();
    while pos < q.len() {
        let mut matched: Option<&str> = None;
        let max_len = (q.len() - pos).min(6);
        for len in (1..=max_len).rev() {
            let cand = &q[pos..pos + len];
            if is_pinyin_syllable(cand) {
                matched = Some(cand);
                break;
            }
        }
        match matched {
            Some(syl) => {
                heads.push(syl.to_string());
                pos += syl.len();
            }
            None => {
                // 剩下的全是尾巴（用户正在输入的半个音节）。
                let tail = q[pos..].to_string();
                // 尾巴必须是纯字母（已保证）且不能太长（防 `qw` 类首字母串被误切）。
                if heads.is_empty() {
                    return None;
                }
                let _ = bytes;
                return Some((heads, tail));
            }
        }
    }
    if heads.len() < 2 && heads.first().map(|h| h == q).unwrap_or(false) {
        // 单音节原样（如 `ma`）：与原单 token 路等价，交回调用方走原路。
        return None;
    }
    Some((heads, String::new()))
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
        docs_ini: 0,
        docs_full: 0,
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
    // 1) 串行扫 term 字典：只收集 CJK 词（快，纯 FST 遍历）。
    let mut terms: Vec<Vec<u8>> = Vec::new();
    {
        let mut stream = inv
            .terms()
            .stream()
            .map_err(|e| Error::Index(format!("term 流打开失败: {e}")))?;
        while stream.advance() {
            let tb = stream.key();
            // CJK 预检：无 >=0x80 字节则必为纯 ASCII（拼音无关），跳过。
            if !tb.iter().any(|&b| b >= 0x80) {
                continue;
            }
            // 顺手 UTF-8 校验 + 确认含 CJK，避免无效 postings 打开。
            let ok = std::str::from_utf8(tb)
                .map(|t| t.chars().any(is_cjk))
                .unwrap_or(false);
            if ok {
                terms.push(tb.to_vec());
            }
        }
    }
    if terms.is_empty() {
        return Ok(0);
    }
    st.terms_cjk += terms.len() as u64;
    // 2) 并行扫 postings：每个线程攒自己分片内的 (doc,pos,term_idx)，
    // fold/reduce 合并。shard 范围过滤在读时完成（postings 有序，超界即停）。
    let term_strs: Vec<String> = terms
        .iter()
        .map(|t| String::from_utf8_lossy(t).into_owned())
        .collect();
    let per_doc: HashMap<u32, Vec<(u32, u32)>> = terms
        .par_iter()
        .enumerate()
        .fold(HashMap::new, |mut acc: HashMap<u32, Vec<(u32, u32)>>, (ti, tb)| {
            let term = tantivy::Term::from_field_bytes(body_field, tb);
            let postings = match inv.read_postings(&term, IndexRecordOption::WithFreqsAndPositions) {
                Ok(Some(p)) => p,
                _ => return acc,
            };
            let mut postings = postings;
            let mut positions: Vec<u32> = Vec::new();
            let mut doc = postings.doc();
            while doc != TERMINATED {
                if doc >= shard_end {
                    break;
                }
                if doc >= shard_start && alive.contains(&doc) {
                    positions.clear();
                    postings.positions(&mut positions);
                    let entry = acc.entry(doc).or_default();
                    for &p in positions.iter() {
                        entry.push((p, ti as u32));
                    }
                }
                doc = postings.advance();
            }
            acc
        })
        .reduce(HashMap::new, |mut a, b| {
            for (k, mut v) in b {
                a.entry(k).or_default().append(&mut v);
            }
            a
        });
    if per_doc.is_empty() {
        return Ok(0);
    }
    // 3) 重建字序 → 拼音文本 → 写入辅库（并行重建，串行写入）。
    let items: Vec<(DocMeta, String, String)> = per_doc
        .par_iter()
        .filter_map(|(&doc_id, tokpos)| {
            let chars = match rebuild_chars(tokpos, &term_strs) {
                Some(c) if !c.is_empty() => c,
                _ => return None,
            };
            let doc: tantivy::TantivyDocument =
                searcher.doc(DocAddress::new(segment_ord, doc_id)).ok()?;
            let meta = DocMeta {
                frn: doc.get_first(fields.frn).and_then(|v| v.as_u64()).unwrap_or(0),
                volume_id: doc.get_first(fields.volume).and_then(|v| v.as_str()).unwrap_or("").to_owned(),
                path: doc.get_first(fields.path).and_then(|v| v.as_str()).unwrap_or("").to_owned(),
                mtime: doc.get_first(fields.mtime).and_then(|v| v.as_u64()).unwrap_or(0),
                size: doc.get_first(fields.size).and_then(|v| v.as_u64()).unwrap_or(0),
            };
            // 空路径的脏 doc 跳过（不污染辅库）。
            if meta.path.is_empty() {
                return None;
            }
            let ini = initials_text(&chars);
            if ini.is_empty() {
                return None;
            }
            let full = full_text(&chars);
            Some((meta, ini, full))
        })
        .collect();
    st.chars += items.len() as u64;
    let mut n_ini = 0u64;
    let mut n_full = 0u64;
    for (meta, ini, full) in &items {
        if aux_ini.index_doc(meta, ini).is_ok() {
            n_ini += 1;
        }
        if !full.is_empty() && aux_full.index_doc(meta, full).is_ok() {
            n_full += 1;
        }
    }
    st.docs_ini += n_ini;
    st.docs_full += n_full;
    Ok(n_ini)
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

#[cfg(test)]
mod split_tests {
    use super::split_pinyin_query;

    #[test]
    fn concatenated_splits_like_spaced() {
        let (heads, tail) = split_pinyin_query("quanwen").expect("quanwen 应切分");
        assert_eq!(heads, vec!["quan".to_string(), "wen".to_string()]);
        assert!(tail.is_empty());
        let (heads, tail) = split_pinyin_query("machuntian").expect("machuntian 应切分");
        assert_eq!(
            heads,
            vec!["ma".to_string(), "chun".to_string(), "tian".to_string()]
        );
        assert!(tail.is_empty());
    }

    #[test]
    fn partial_tail_degrades_to_prefix() {
        let (heads, tail) = split_pinyin_query("quanw").expect("quanw 应部分切分");
        assert_eq!(heads, vec!["quan".to_string()]);
        assert_eq!(tail, "w");
    }

    #[test]
    fn initials_and_single_syllable_keep_old_path() {
        assert!(split_pinyin_query("qw").is_none(), "qw 是首字母串，不切分");
        assert!(split_pinyin_query("ma").is_none(), "单音节走原单 token 路");
        assert!(split_pinyin_query("").is_none());
        assert!(split_pinyin_query("QuanWen").is_none(), "非全小写不切分");
    }
}

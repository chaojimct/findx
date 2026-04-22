//! Everything 风格查询解析：函数式条件、类型宏、size/dm/dc、attrib、sort 等。

use std::time::SystemTime;

use chrono::{Datelike, Local, NaiveDate};

use crate::Error;

const DEFAULT_LIMIT: u32 = 1000;

/// 与 `sort:` 对应
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortField {
    #[default]
    Name,
    Path,
    Size,
    Modified,
    Created,
}

/// 解析后的查询（AND 组合；`!` 为子串排除）
#[derive(Debug, Clone)]
pub struct ParsedQuery {
    /// 顶层 `|` OR 分支（除首段外的各段解析结果；首段字段在本 struct 其余字段上）
    pub or_branches: Vec<ParsedQuery>,
    /// 多裸词 AND（与 `substring` 二选一优先使用本字段）
    pub name_terms: Vec<String>,
    /// 文件名子串（兼容单关键词）
    pub substring: Option<String>,
    /// 多扩展名（小写、无点），`ext:jpg;png`
    pub ext_list: Vec<String>,
    /// 兼容旧测试与单 `ext:` 首项
    pub ext: Option<String>,
    pub size_min: Option<u64>,
    pub size_max: Option<u64>,
    /// 修改时间 FILETIME
    pub mtime_min: Option<u64>,
    pub mtime_max: Option<u64>,
    /// 创建时间 FILETIME
    pub ctime_min: Option<u64>,
    pub ctime_max: Option<u64>,
    /// `attrib:rh` 中需全部置位的位（映射到 FileEntry 风格位）
    pub attrib_must: u32,
    pub only_files: bool,
    pub only_dirs: bool,
    /// `true` 为区分大小写
    pub case_sensitive: bool,
    pub whole_word: bool,
    pub whole_filename: bool,
    /// 全路径子串（小写比较），`path:`
    pub path_match: Option<String>,
    /// 父目录路径（小写、无盘符或已抽出到 `drive`）；与 Everything 一致时见 `parent_path_substring`
    pub parent_path: Option<String>,
    /// `true` 时 `parent*` 按**父路径子串**过滤（旧行为）；默认 `false`：`parent:` / `infolder:` 为**父路径全等**（仅该文件夹下直接子项，对齐 Everything）
    pub parent_path_substring: bool,
    /// 与 `C:\path` 或 `d:path` 等价的 path_prefix
    pub path_prefix: Option<String>,
    pub drive: Option<char>,
    pub starts_with: Option<String>,
    pub ends_with: Option<String>,
    pub len_min: Option<u32>,
    pub len_max: Option<u32>,
    pub sort_by: SortField,
    pub sort_desc: bool,
    /// 子串必须 **不** 出现
    pub not_substring: Option<String>,
    /// `regex:` 走字节正则
    pub regex_pattern: Option<String>,
    /// 通配
    pub glob_pattern: Option<String>,
    /// 强制拼音；需 `pinyin` feature
    pub pinyin_only: bool,
    pub no_pinyin: bool,
    pub limit: u32,
    /// 历史字段；曾与 `parent:` 组合表示「父路径精确」。现已由 `parent_path_substring` 取代；保留解析以免旧查询报错
    pub no_subfolders: bool,
    /// `size:empty`
    pub size_empty: bool,
    /// `nopath:` 为真时 `path:` 只匹配文件名
    pub nopath: bool,
    /// `wildcards:` 显式通配（与裸 `*` 一致，由搜索层解释）
    pub wildcards: bool,
    /// `nowfn:` 仅匹配文件名（关闭全路径启发式）
    pub nowfn: bool,
    /// 访问时间 FILETIME（当前索引未持久化 atime 时搜索层报错）
    pub atime_min: Option<u64>,
    pub atime_max: Option<u64>,
    /// `depth:` min..max
    pub depth_min: Option<u32>,
    pub depth_max: Option<u32>,
    /// `child:` 精确子项数
    pub child_exact: Option<u32>,
    /// `empty:` 目录是否为空
    pub empty_dir: Option<bool>,
    /// `dupe:` / `sizedupe:` 等简记
    pub dupe_kind: Option<String>,
    /// `content:` 慢路径子串
    pub content_substring: Option<String>,
    /// `utf8content:`
    pub utf8content_substring: Option<String>,
}

impl Default for ParsedQuery {
    fn default() -> Self {
        Self {
            or_branches: Vec::new(),
            name_terms: Vec::new(),
            substring: None,
            ext_list: Vec::new(),
            ext: None,
            size_min: None,
            size_max: None,
            mtime_min: None,
            mtime_max: None,
            ctime_min: None,
            ctime_max: None,
            attrib_must: 0,
            only_files: false,
            only_dirs: false,
            case_sensitive: false,
            whole_word: false,
            whole_filename: false,
            path_match: None,
            parent_path: None,
            parent_path_substring: false,
            path_prefix: None,
            drive: None,
            starts_with: None,
            ends_with: None,
            len_min: None,
            len_max: None,
            sort_by: SortField::default(),
            sort_desc: false,
            not_substring: None,
            regex_pattern: None,
            glob_pattern: None,
            pinyin_only: false,
            no_pinyin: false,
            limit: DEFAULT_LIMIT,
            no_subfolders: false,
            size_empty: false,
            nopath: false,
            wildcards: false,
            nowfn: false,
            atime_min: None,
            atime_max: None,
            depth_min: None,
            depth_max: None,
            child_exact: None,
            empty_dir: None,
            dupe_kind: None,
            content_substring: None,
            utf8content_substring: None,
        }
    }
}

pub struct QueryParser;

impl QueryParser {
    pub fn parse(input: &str) -> std::result::Result<ParsedQuery, Error> {
        let segs = split_top_level_bar(input.trim());
        if segs.len() == 1 {
            return Self::parse_single(&segs[0]);
        }
        let mut it = segs.into_iter();
        let first = it.next().unwrap();
        let mut main = Self::parse_single(&first)?;
        for s in it {
            main.or_branches.push(Self::parse_single(&s)?);
        }
        Ok(main)
    }

    fn parse_single(input: &str) -> std::result::Result<ParsedQuery, Error> {
        let mut q = ParsedQuery::default();
        let mut rest = input.trim();
        while !rest.is_empty() {
            rest = rest.trim_start();
            if rest.is_empty() {
                break;
            }
            // 驱动器 `d:` / `d:\path`
            if let Some(d) = rest.chars().next() {
                if d.is_ascii_alphabetic() && rest.len() >= 2 {
                    let b1 = rest.as_bytes()[1];
                    if b1 == b':' {
                        let letter = d.to_ascii_uppercase();
                        q.drive = Some(letter);
                        let after = &rest[2..];
                        if after.starts_with('\\') || after.starts_with('/') {
                            let (path_part, remain) = split_path_token(&rest[2..]);
                            let norm = path_part.replace('/', "\\");
                            if !norm.is_empty() {
                                q.path_prefix = Some(norm.to_ascii_lowercase());
                            }
                            rest = remain;
                            continue;
                        }
                        rest = &rest[2..];
                        continue;
                    }
                }
            }

            if let Some((key, val, remain)) = try_parse_func(rest)? {
                apply_func(&mut q, key, val)?;
                rest = remain;
                continue;
            }

            let (token, remain) = next_token_smart(rest);
            rest = remain;
            let token = token.trim();
            if token.is_empty() {
                break;
            }

            // `!keyword`
            if let Some(inner) = token.strip_prefix('!').map(str::trim) {
                if !inner.is_empty() {
                    q.not_substring = Some(norm_case(&q, inner.to_string()));
                }
                continue;
            }

            if let Some((base, suf)) = token.rsplit_once(';') {
                match suf {
                    "py" => {
                        q.pinyin_only = true;
                        if !base.is_empty() {
                            q.substring = Some(norm_case(&q, base.to_string()));
                        }
                    }
                    "en" | "np" => {
                        q.no_pinyin = true;
                        if !base.is_empty() {
                            q.substring = Some(norm_case(&q, base.to_string()));
                        }
                    }
                    _ => {
                        q.substring = Some(norm_case(&q, token.to_string()));
                    }
                }
            } else {
                if token.contains('*') || token.contains('?') {
                    q.glob_pattern = Some(token.to_string());
                } else {
                    let tnorm = norm_case(&q, token.to_string());
                    q.name_terms.push(tnorm.clone());
                    if q.substring.is_none() {
                        q.substring = Some(tnorm);
                    }
                }
            }
        }

        if q.ext_list.is_empty() {
            if let Some(ref e) = q.ext {
                q.ext_list.push(e.clone());
            }
        } else if q.ext.is_none() {
            q.ext = q.ext_list.first().cloned();
        }

        Ok(q)
    }
}

fn norm_case(q: &ParsedQuery, s: String) -> String {
    if q.case_sensitive {
        s
    } else {
        s.to_ascii_lowercase()
    }
}

fn apply_func(q: &mut ParsedQuery, key: &str, val: &str) -> std::result::Result<(), Error> {
    let k = key.to_ascii_lowercase();
    match k.as_str() {
        "ext" => {
            q.ext_list = val
                .split(';')
                .map(|x| x.trim().trim_start_matches('.').to_ascii_lowercase())
                .filter(|x| !x.is_empty())
                .collect();
            q.ext = q.ext_list.first().cloned();
        }
        "audio" => q.ext_list = type_macro_audio(),
        "video" => q.ext_list = type_macro_video(),
        "pic" | "image" => q.ext_list = type_macro_pic(),
        "doc" => q.ext_list = type_macro_doc(),
        "exe" => q.ext_list = vec!["exe".into(), "msi".into(), "bat".into(), "cmd".into()],
        "zip" | "archive" => q.ext_list = type_macro_zip(),
        "size" => parse_size_full(q, val)?,
        "dm" | "datemodified" => parse_dm_full(q, val)?,
        "dc" | "datecreated" => parse_dc_full(q, val)?,
        "count" => {
            if let Ok(n) = val.trim().parse::<u32>() {
                q.limit = n;
            }
        }
        "regex" => {
            q.regex_pattern = Some(val.to_string());
        }
        "path" => {
            q.path_match = Some(val.trim().to_ascii_lowercase());
        }
        "parent" | "infolder" => {
            let mut p = val.replace('/', "\\").to_ascii_lowercase();
            if p.len() >= 2 {
                let b = p.as_bytes();
                if b[0].is_ascii_alphabetic() && b[1] == b':' {
                    q.drive = Some(p.as_bytes()[0].to_ascii_uppercase() as char);
                    p = p[2..].trim_start_matches('\\').to_string();
                }
            }
            q.parent_path = Some(p.trim_matches('\\').to_string());
            q.parent_path_substring = false;
        }
        "parentcontains" => {
            let mut p = val.replace('/', "\\").to_ascii_lowercase();
            if p.len() >= 2 {
                let b = p.as_bytes();
                if b[0].is_ascii_alphabetic() && b[1] == b':' {
                    q.drive = Some(p.as_bytes()[0].to_ascii_uppercase() as char);
                    p = p[2..].trim_start_matches('\\').to_string();
                }
            }
            q.parent_path = Some(p.trim_matches('\\').to_string());
            q.parent_path_substring = true;
        }
        "nosubfolders" => {
            let v = val.trim().to_ascii_lowercase();
            q.no_subfolders = v == "1" || v == "yes" || v == "true";
        }
        "file" => {
            q.only_files = true;
            let v = val.trim();
            if !v.is_empty() {
                if v.contains('*') || v.contains('?') {
                    q.glob_pattern = Some(v.to_string());
                } else {
                    let tnorm = norm_case(q, v.to_string());
                    q.name_terms.push(tnorm.clone());
                    if q.substring.is_none() {
                        q.substring = Some(tnorm);
                    }
                }
            }
        }
        "folder" => {
            q.only_dirs = true;
            let v = val.trim();
            if !v.is_empty() {
                if v.contains('*') || v.contains('?') {
                    q.glob_pattern = Some(v.to_string());
                } else {
                    let tnorm = norm_case(q, v.to_string());
                    q.name_terms.push(tnorm.clone());
                    if q.substring.is_none() {
                        q.substring = Some(tnorm);
                    }
                }
            }
        }
        "case" => {
            let v = val.trim().to_ascii_lowercase();
            q.case_sensitive = v == "1" || v == "yes" || v == "sensitive";
        }
        "nocase" => q.case_sensitive = false,
        "wholeword" => {
            let v = val.trim().to_ascii_lowercase();
            q.whole_word = v != "0" && v != "false" && v != "no";
        }
        "wfn" => {
            let v = val.trim().to_ascii_lowercase();
            q.whole_filename = v != "0" && v != "false" && v != "no";
        }
        "startwith" => q.starts_with = Some(norm_case(q, val.to_string())),
        "endwith" => q.ends_with = Some(norm_case(q, val.to_string())),
        "len" => parse_len(q, val)?,
        "attrib" => q.attrib_must |= parse_attrib_letters(val),
        "sort" => parse_sort(q, val)?,
        "nopath" => {
            let v = val.trim().to_ascii_lowercase();
            q.nopath = v == "1" || v == "yes" || v == "true";
        }
        "wildcards" => {
            let v = val.trim().to_ascii_lowercase();
            q.wildcards = v != "0" && v != "false" && v != "no";
        }
        "nowfn" => {
            let v = val.trim().to_ascii_lowercase();
            q.nowfn = v == "1" || v == "yes" || v == "true";
        }
        "da" | "dateaccessed" => {
            parse_access_date_range(q, val)?;
        }
        "depth" => {
            let v = val.trim();
            if let Some((a, b)) = v.split_once("..") {
                q.depth_min = a.trim().parse().ok();
                q.depth_max = b.trim().parse().ok();
            } else if let Ok(n) = v.parse::<u32>() {
                q.depth_min = Some(n);
                q.depth_max = Some(n);
            }
        }
        "child" => {
            if let Ok(n) = val.trim().parse::<u32>() {
                q.child_exact = Some(n);
            }
        }
        "empty" => {
            let v = val.trim().to_ascii_lowercase();
            q.empty_dir = Some(v == "1" || v == "yes" || v == "true");
        }
        "dupe" => q.dupe_kind = Some(val.trim().to_ascii_lowercase()),
        "sizedupe" => q.dupe_kind = Some("size".into()),
        "content" => q.content_substring = Some(val.to_string()),
        "utf8content" => q.utf8content_substring = Some(val.to_string()),
        _ => {
            return Err(Error::Query(format!("未知修饰符: {k}")));
        }
    }
    Ok(())
}

fn type_macro_audio() -> Vec<String> {
    vec![
        "mp3".into(),
        "wav".into(),
        "flac".into(),
        "aac".into(),
        "ogg".into(),
        "wma".into(),
        "m4a".into(),
    ]
}

fn type_macro_video() -> Vec<String> {
    vec![
        "mp4".into(),
        "mkv".into(),
        "avi".into(),
        "mov".into(),
        "wmv".into(),
        "flv".into(),
        "webm".into(),
        "mpeg".into(),
        "mpg".into(),
    ]
}

fn type_macro_pic() -> Vec<String> {
    vec![
        "jpg".into(),
        "jpeg".into(),
        "png".into(),
        "gif".into(),
        "bmp".into(),
        "webp".into(),
        "tif".into(),
        "tiff".into(),
        "ico".into(),
        "heic".into(),
    ]
}

fn type_macro_doc() -> Vec<String> {
    vec![
        "doc".into(),
        "docx".into(),
        "pdf".into(),
        "txt".into(),
        "md".into(),
        "rtf".into(),
        "xls".into(),
        "xlsx".into(),
        "ppt".into(),
        "pptx".into(),
        "csv".into(),
    ]
}

fn type_macro_zip() -> Vec<String> {
    vec![
        "zip".into(),
        "rar".into(),
        "7z".into(),
        "tar".into(),
        "gz".into(),
        "bz2".into(),
        "xz".into(),
        "cab".into(),
    ]
}

fn parse_len(q: &mut ParsedQuery, val: &str) -> std::result::Result<(), Error> {
    let v = val.trim();
    if let Some(rest) = v.strip_prefix('>') {
        let n: u32 = rest
            .trim()
            .parse()
            .map_err(|_| Error::Query(format!("无效 len: {val}")))?;
        q.len_min = Some(n);
    } else if let Some(rest) = v.strip_prefix('<') {
        let n: u32 = rest
            .trim()
            .parse()
            .map_err(|_| Error::Query(format!("无效 len: {val}")))?;
        q.len_max = Some(n);
    } else if let Some((a, b)) = v.split_once("..") {
        let lo: u32 = a
            .trim()
            .parse()
            .map_err(|_| Error::Query(format!("无效 len 范围: {val}")))?;
        let hi: u32 = b
            .trim()
            .parse()
            .map_err(|_| Error::Query(format!("无效 len 范围: {val}")))?;
        q.len_min = Some(lo);
        q.len_max = Some(hi);
    } else {
        let n: u32 = v
            .parse()
            .map_err(|_| Error::Query(format!("无效 len: {val}")))?;
        q.len_min = Some(n);
        q.len_max = Some(n);
    }
    Ok(())
}

fn parse_sort(q: &mut ParsedQuery, val: &str) -> std::result::Result<(), Error> {
    let parts: Vec<&str> = val.split(':').collect();
    let field = parts
        .first()
        .map(|s| s.trim().to_ascii_lowercase())
        .unwrap_or_default();
    match field.as_str() {
        "size" => q.sort_by = SortField::Size,
        "modified" | "mtime" => q.sort_by = SortField::Modified,
        "created" | "ctime" => q.sort_by = SortField::Created,
        "path" => q.sort_by = SortField::Path,
        "name" | "" => q.sort_by = SortField::Name,
        _ => {}
    }
    if let Some(second) = parts.get(1) {
        let d = second.trim().to_ascii_lowercase();
        q.sort_desc = d == "desc" || d == "d" || d == "1" || d == "true";
    }
    Ok(())
}

/// R H S A 映射到自定义位（与 `FileEntry` 低 8 位对齐）
fn parse_attrib_letters(s: &str) -> u32 {
    let mut bits = 0u32;
    for c in s.chars() {
        match c.to_ascii_lowercase() {
            'r' => bits |= 1 << 3,
            'h' => bits |= 1 << 1,
            's' => bits |= 1 << 2,
            'a' => bits |= 1 << 4,
            'd' => bits |= 1 << 0, // 目录
            _ => {}
        }
    }
    bits
}

fn parse_size_full(q: &mut ParsedQuery, val: &str) -> std::result::Result<(), Error> {
    let v = val.trim().to_ascii_lowercase();
    match v.as_str() {
        "tiny" => {
            q.size_max = Some(16 * 1024);
        }
        "small" => {
            q.size_min = Some(16 * 1024);
            q.size_max = Some(1024 * 1024);
        }
        "medium" => {
            q.size_min = Some(1024 * 1024);
            q.size_max = Some(32 * 1024 * 1024);
        }
        "large" => {
            q.size_min = Some(32 * 1024 * 1024);
            q.size_max = Some(128 * 1024 * 1024);
        }
        "huge" => {
            q.size_min = Some(128 * 1024 * 1024);
            q.size_max = Some(1_024u64 * 1024 * 1024);
        }
        "gigantic" => {
            q.size_min = Some(1_024u64 * 1024 * 1024);
        }
        "empty" => {
            q.size_empty = true;
            q.size_min = Some(0);
            q.size_max = Some(0);
        }
        _ => {
            if let Some(rest) = v.strip_prefix('<') {
                q.size_max = Some(parse_size_bytes(rest)?);
            } else if let Some(rest) = v.strip_prefix('>') {
                q.size_min = Some(parse_size_bytes(rest)?);
            } else if let Some((a, b)) = v.split_once("..") {
                q.size_min = Some(parse_size_bytes(a)?);
                q.size_max = Some(parse_size_bytes(b)?);
            } else {
                q.size_min = Some(parse_size_bytes(&v)?);
            }
        }
    }
    Ok(())
}

fn parse_size_bytes(s: &str) -> std::result::Result<u64, Error> {
    let s = s.trim();
    let s_lower = s.to_ascii_lowercase();
    let (num, mul) = if s_lower.ends_with("kb") {
        (&s[..s.len() - 2], 1024u64)
    } else if s_lower.ends_with("mb") {
        (&s[..s.len() - 2], 1024u64 * 1024)
    } else if s_lower.ends_with("gb") {
        (&s[..s.len() - 2], 1024u64 * 1024 * 1024)
    } else if s_lower.ends_with('b') && s.len() > 1 {
        (&s[..s.len() - 1], 1u64)
    } else {
        (s, 1u64)
    };
    let n: u64 = num
        .trim()
        .parse()
        .map_err(|_| Error::Query(format!("无效大小: {s}")))?;
    Ok(n.saturating_mul(mul))
}

fn parse_dm_full(q: &mut ParsedQuery, val: &str) -> std::result::Result<(), Error> {
    parse_date_range(val, true, q)
}

fn parse_dc_full(q: &mut ParsedQuery, val: &str) -> std::result::Result<(), Error> {
    parse_date_range(val, false, q)
}

fn parse_date_range(val: &str, mtime: bool, q: &mut ParsedQuery) -> std::result::Result<(), Error> {
    let v = val.trim().to_ascii_lowercase();
    let now = SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| Error::Query(e.to_string()))?
        .as_secs();
    let day_start = |unix: u64| unix - (unix % 86400);
    let today_start = day_start(now);
    let mut apply = |min_opt: Option<u64>, max_opt: Option<u64>| {
        if mtime {
            if let Some(x) = min_opt {
                q.mtime_min = Some(x);
            }
            if let Some(x) = max_opt {
                q.mtime_max = Some(x);
            }
        } else {
            if let Some(x) = min_opt {
                q.ctime_min = Some(x);
            }
            if let Some(x) = max_opt {
                q.ctime_max = Some(x);
            }
        }
    };
    match v.as_str() {
        "today" => apply(Some(unix_secs_to_filetime(today_start)), None),
        "yesterday" => {
            let ys = today_start.saturating_sub(86400);
            apply(
                Some(unix_secs_to_filetime(ys)),
                Some(unix_secs_to_filetime(today_start).saturating_sub(1)),
            );
        }
        "thisweek" => {
            let (w0_unix, _) = calendar_week_bounds_local();
            let eod = today_start.saturating_add(86400).saturating_sub(1);
            apply(
                Some(unix_secs_to_filetime(w0_unix)),
                Some(unix_secs_to_filetime(eod)),
            );
        }
        "lastweek" => {
            let (w0_unix, w1_unix) = calendar_week_bounds_local();
            let week_sec = 7 * 86400;
            apply(
                Some(unix_secs_to_filetime(
                    w0_unix.saturating_sub(week_sec),
                )),
                Some(unix_secs_to_filetime(w1_unix.saturating_sub(week_sec))),
            );
        }
        "thismonth" => {
            let (m0_unix, _) = calendar_month_bounds_local();
            apply(Some(unix_secs_to_filetime(m0_unix)), None);
        }
        "lastmonth" => {
            let (lm0, lm1) = prev_calendar_month_bounds_local();
            apply(
                Some(unix_secs_to_filetime(lm0)),
                Some(unix_secs_to_filetime(lm1)),
            );
        }
        "thisyear" => {
            let y = Local::now().year();
            let jan1 = first_day_of_year_secs(y);
            apply(Some(unix_secs_to_filetime(jan1)), None);
        }
        "lastyear" => {
            let y = Local::now().year() - 1;
            let s0 = first_day_of_year_secs(y);
            let s1 = first_day_of_year_secs(y + 1).saturating_sub(1);
            apply(
                Some(unix_secs_to_filetime(s0)),
                Some(unix_secs_to_filetime(s1)),
            );
        }
        _ => {
            if let Some((a, b)) = v.split_once("..") {
                let (a_lo, _) = parse_date_token_flexible(a.trim())?;
                let (_, b_hi) = parse_date_token_flexible(b.trim())?;
                apply(Some(a_lo), Some(b_hi));
            } else if let Some(rest) = v.strip_prefix('>') {
                let rest = rest.trim();
                if rest.is_empty() {
                    return Err(Error::Query(format!("不支持的日期条件: {val}")));
                }
                let (lo, _) = parse_date_token_flexible(rest)?;
                apply(Some(lo), None);
            } else if let Some(rest) = v.strip_prefix('<') {
                let rest = rest.trim();
                if rest.is_empty() {
                    return Err(Error::Query(format!("不支持的日期条件: {val}")));
                }
                let (_, hi) = parse_date_token_flexible(rest)?;
                apply(None, Some(hi));
            } else {
                match parse_date_token_flexible(&v) {
                    Ok((lo, hi)) => apply(Some(lo), Some(hi)),
                    Err(_) => return Err(Error::Query(format!("不支持的日期条件: {val}"))),
                }
            }
        }
    }
    Ok(())
}

/// `da:` / `dateaccessed:` — 访问时间由索引中的 atime 字段过滤；若未建 atime，搜索层会报错。
fn parse_access_date_range(q: &mut ParsedQuery, val: &str) -> std::result::Result<(), Error> {
    let v = val.trim().to_ascii_lowercase();
    let now = SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| Error::Query(e.to_string()))?
        .as_secs();
    let day_start = |unix: u64| unix - (unix % 86400);
    let today_start = day_start(now);
    let mut apply = |min_opt: Option<u64>, max_opt: Option<u64>| {
        if let Some(x) = min_opt {
            q.atime_min = Some(x);
        }
        if let Some(x) = max_opt {
            q.atime_max = Some(x);
        }
    };
    match v.as_str() {
        "today" => apply(Some(unix_secs_to_filetime(today_start)), None),
        "yesterday" => {
            let ys = today_start.saturating_sub(86400);
            apply(
                Some(unix_secs_to_filetime(ys)),
                Some(unix_secs_to_filetime(today_start).saturating_sub(1)),
            );
        }
        "thisweek" => {
            let (w0, _) = calendar_week_bounds_local();
            apply(
                Some(unix_secs_to_filetime(w0)),
                Some(unix_secs_to_filetime(today_start.saturating_add(86400).saturating_sub(1))),
            );
        }
        "thismonth" => {
            let (m0, _) = calendar_month_bounds_local();
            apply(Some(unix_secs_to_filetime(m0)), None);
        }
        _ => {
            if let Some((a, b)) = v.split_once("..") {
                let (a_lo, _) = parse_date_token_flexible(a.trim())?;
                let (_, b_hi) = parse_date_token_flexible(b.trim())?;
                apply(Some(a_lo), Some(b_hi));
            } else if let Some(rest) = v.strip_prefix('>') {
                let rest = rest.trim();
                if rest.is_empty() {
                    return Err(Error::Query(format!("不支持的访问日期条件: {val}")));
                }
                let (lo, _) = parse_date_token_flexible(rest)?;
                apply(Some(lo), None);
            } else if let Some(rest) = v.strip_prefix('<') {
                let rest = rest.trim();
                if rest.is_empty() {
                    return Err(Error::Query(format!("不支持的访问日期条件: {val}")));
                }
                let (_, hi) = parse_date_token_flexible(rest)?;
                apply(None, Some(hi));
            } else {
                match parse_date_token_flexible(&v) {
                    Ok((lo, hi)) => apply(Some(lo), Some(hi)),
                    Err(_) => return Err(Error::Query(format!("不支持的访问日期条件: {val}"))),
                }
            }
        }
    }
    Ok(())
}

fn calendar_week_bounds_local() -> (u64, u64) {
    let now = Local::now().naive_local().date();
    let from_mon = now.weekday().num_days_from_monday() as i64;
    let week_start = now - chrono::Duration::days(from_mon);
    let week_end = week_start + chrono::Duration::days(6);
    let s0 = week_start
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_local_timezone(Local)
        .unwrap()
        .timestamp() as u64;
    let s1 = week_end
        .and_hms_opt(23, 59, 59)
        .unwrap()
        .and_local_timezone(Local)
        .unwrap()
        .timestamp() as u64;
    (s0, s1)
}

fn calendar_month_bounds_local() -> (u64, u64) {
    let now = Local::now().naive_local().date();
    let first = NaiveDate::from_ymd_opt(now.year(), now.month(), 1).unwrap();
    let end = if now.month() == 12 {
        NaiveDate::from_ymd_opt(now.year() + 1, 1, 1).unwrap() - chrono::Duration::days(1)
    } else {
        NaiveDate::from_ymd_opt(now.year(), now.month() + 1, 1).unwrap()
            - chrono::Duration::days(1)
    };
    let s0 = first
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_local_timezone(Local)
        .unwrap()
        .timestamp() as u64;
    let s1 = end
        .and_hms_opt(23, 59, 59)
        .unwrap()
        .and_local_timezone(Local)
        .unwrap()
        .timestamp() as u64;
    (s0, s1)
}

fn prev_calendar_month_bounds_local() -> (u64, u64) {
    let now = Local::now().naive_local().date();
    let this_first = NaiveDate::from_ymd_opt(now.year(), now.month(), 1).unwrap();
    let last_end = this_first - chrono::Duration::days(1);
    let last_first = NaiveDate::from_ymd_opt(last_end.year(), last_end.month(), 1).unwrap();
    let s0 = last_first
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_local_timezone(Local)
        .unwrap()
        .timestamp() as u64;
    let s1 = last_end
        .and_hms_opt(23, 59, 59)
        .unwrap()
        .and_local_timezone(Local)
        .unwrap()
        .timestamp() as u64;
    (s0, s1)
}

fn first_day_of_year_secs(year: i32) -> u64 {
    let d = NaiveDate::from_ymd_opt(year, 1, 1).unwrap();
    d.and_hms_opt(0, 0, 0)
        .unwrap()
        .and_local_timezone(Local)
        .unwrap()
        .timestamp() as u64
}

fn parse_date_token(s: &str) -> std::result::Result<u64, Error> {
    let s = s.trim();
    let parts: Vec<&str> = s.split('-').collect();
    if parts.len() == 3 {
        let y: i32 = parts[0].parse().map_err(|_| Error::Query("日期无效".into()))?;
        let m: u32 = parts[1].parse().map_err(|_| Error::Query("日期无效".into()))?;
        let d: u32 = parts[2].parse().map_err(|_| Error::Query("日期无效".into()))?;
        // days_from_civil 已是「相对 1970-01-01 的公历日数」，乘 86400 即 Unix 秒（UTC 当日 0 点）。
        // 切勿再减 11644473600：那是 unix_secs_to_filetime 内部用于 1970↔1601 的位移，重复减会导致
        // saturating_sub 恒为 0 → dm:/dc: 阈值变成「未过滤」。
        let days = days_from_civil(y, m, d);
        let unix = days.saturating_mul(86400);
        Ok(unix_secs_to_filetime(unix))
    } else {
        Err(Error::Query(format!("日期格式应为 YYYY-MM-DD: {s}")))
    }
}

/// `YYYY-MM-DD` 单日，或 `YYYY-MM` 整月（起止 FILETIME）
fn parse_date_token_flexible(s: &str) -> std::result::Result<(u64, u64), Error> {
    let s = s.trim();
    let parts: Vec<&str> = s.split('-').collect();
    if parts.len() == 3 {
        let lo = parse_date_token(s)?;
        let hi = unix_secs_to_filetime(
            filetime_to_unix_secs(lo)
                .saturating_add(86400)
                .saturating_sub(1),
        );
        return Ok((lo, hi));
    }
    if parts.len() == 2 {
        let y: i32 = parts[0]
            .parse()
            .map_err(|_| Error::Query("日期年月无效".into()))?;
        let m: u32 = parts[1]
            .parse()
            .map_err(|_| Error::Query("日期年月无效".into()))?;
        let first = NaiveDate::from_ymd_opt(y, m, 1).ok_or_else(|| Error::Query("日期无效".into()))?;
        let last = if m == 12 {
            NaiveDate::from_ymd_opt(y + 1, 1, 1).unwrap() - chrono::Duration::days(1)
        } else {
            NaiveDate::from_ymd_opt(y, m + 1, 1).unwrap() - chrono::Duration::days(1)
        };
        let lo = first
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_local_timezone(Local)
            .unwrap()
            .timestamp() as u64;
        let hi = last
            .and_hms_opt(23, 59, 59)
            .unwrap()
            .and_local_timezone(Local)
            .unwrap()
            .timestamp() as u64;
        return Ok((unix_secs_to_filetime(lo), unix_secs_to_filetime(hi)));
    }
    Err(Error::Query(format!("日期格式应为 YYYY-MM-DD 或 YYYY-MM: {s}")))
}

/// Howard Hinnant 公历算法，返回 1970-01-01 起的日数近似（与 Unix 日对齐）
fn days_from_civil(y: i32, m: u32, d: u32) -> u64 {
    let y = y as i64;
    let m = m as i64;
    let d = d as i64;
    let m_adj = if m <= 2 { m + 12 } else { m };
    let y_adj = if m <= 2 { y - 1 } else { y };
    let era = (if y_adj >= 0 { y_adj + 399 } else { y_adj - 400 }) / 400;
    let yoe = y_adj - era * 400;
    let doy = (153 * (m_adj - 3) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    (era * 146_097 + doe - 719_468) as u64
}

fn unix_secs_to_filetime(unix_secs: u64) -> u64 {
    const EPOCH_DIFF: u64 = 11_644_473_600;
    (unix_secs.saturating_add(EPOCH_DIFF)).saturating_mul(10_000_000)
}

fn filetime_to_unix_secs(ft: u64) -> u64 {
    const EPOCH_DIFF: u64 = 11_644_473_600;
    (ft / 10_000_000).saturating_sub(EPOCH_DIFF)
}

fn split_path_token(s: &str) -> (&str, &str) {
    let bytes = s.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b' ' {
            return (&s[..i], &s[i + 1..]);
        }
        i += 1;
    }
    (s, "")
}

fn next_token(s: &str) -> (&str, &str) {
    let s = s.trim_start();
    if s.is_empty() {
        return ("", "");
    }
    let mut i = 0usize;
    let bytes = s.as_bytes();
    while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    (&s[..i], &s[i..])
}

fn try_parse_func(s: &str) -> Result<Option<(&'static str, &str, &str)>, Error> {
    let s = s.trim_start();
    let colon = match s.find(':') {
        Some(c) => c,
        None => return Ok(None),
    };
    let key = &s[..colon];
    let rest = &s[colon + 1..];
    let (val, remain) = take_func_val_and_remain(rest);
    let remain = remain.trim_start();
    let key_lower = key.to_ascii_lowercase();
    let mapped: Option<&'static str> = match key_lower.as_str() {
        "ext" => Some("ext"),
        "audio" => Some("audio"),
        "video" => Some("video"),
        "pic" => Some("pic"),
        "image" => Some("pic"),
        "doc" => Some("doc"),
        "exe" => Some("exe"),
        "zip" => Some("zip"),
        "archive" => Some("archive"),
        "size" => Some("size"),
        "dm" | "datemodified" => Some("dm"),
        "dc" | "datecreated" => Some("dc"),
        "count" => Some("count"),
        "regex" => Some("regex"),
        "path" => Some("path"),
        "parent" => Some("parent"),
        "infolder" => Some("infolder"),
        "parentcontains" => Some("parentcontains"),
        "nosubfolders" => Some("nosubfolders"),
        "file" => Some("file"),
        "folder" => Some("folder"),
        "case" => Some("case"),
        "nocase" => Some("nocase"),
        "wholeword" => Some("wholeword"),
        "wfn" => Some("wfn"),
        "startwith" => Some("startwith"),
        "endwith" => Some("endwith"),
        "len" => Some("len"),
        "attrib" => Some("attrib"),
        "sort" => Some("sort"),
        "nopath" => Some("nopath"),
        "wildcards" => Some("wildcards"),
        "nowfn" => Some("nowfn"),
        "da" | "dateaccessed" => Some("da"),
        "depth" => Some("depth"),
        "child" => Some("child"),
        "empty" => Some("empty"),
        "dupe" => Some("dupe"),
        "sizedupe" => Some("sizedupe"),
        "content" => Some("content"),
        "utf8content" => Some("utf8content"),
        _ => None,
    };
    if let Some(m) = mapped {
        return Ok(Some((m, val, remain)));
    }
    Err(Error::Query(format!("未知修饰符: {key}")))
}

/// `key:` 右侧取值：支持双引号包一段（内含空格），否则在空白处截断
fn take_func_val_and_remain(rest: &str) -> (&str, &str) {
    let rest = rest.trim_start();
    if rest.starts_with('"') {
        let bytes = rest.as_bytes();
        let mut i = 1usize;
        while i < bytes.len() {
            if bytes[i] == b'"' {
                let val = &rest[1..i];
                let after = rest[i + 1..].trim_start();
                return (val, after);
            }
            i += 1;
        }
        return (rest.trim(), "");
    }
    let end = rest
        .char_indices()
        .find(|(_, c)| c.is_ascii_whitespace())
        .map(|(i, _)| i)
        .unwrap_or(rest.len());
    (&rest[..end], rest[end..].trim_start())
}

fn next_token_smart(s: &str) -> (&str, &str) {
    let s = s.trim_start();
    if s.is_empty() {
        return ("", "");
    }
    if s.starts_with('"') {
        let bytes = s.as_bytes();
        let mut i = 1usize;
        while i < bytes.len() {
            if bytes[i] == b'"' {
                let tok = &s[1..i];
                return (tok, s[i + 1..].trim_start());
            }
            i += 1;
        }
        return (s, "");
    }
    if s.starts_with('\'') {
        let bytes = s.as_bytes();
        let mut i = 1usize;
        while i < bytes.len() {
            if bytes[i] == b'\'' {
                let tok = &s[1..i];
                return (tok, s[i + 1..].trim_start());
            }
            i += 1;
        }
        return (s, "");
    }
    next_token(s)
}

/// 顶层 `|` 分割（尊重引号与 `<>` 分组）
fn split_top_level_bar(input: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut in_dquote = false;
    let mut in_squote = false;
    let mut angle = 0i32;
    for ch in input.chars() {
        if ch == '"' && angle == 0 && !in_squote {
            in_dquote = !in_dquote;
            cur.push(ch);
            continue;
        }
        if ch == '\'' && angle == 0 && !in_dquote {
            in_squote = !in_squote;
            cur.push(ch);
            continue;
        }
        if ch == '<' && !in_dquote && !in_squote {
            angle += 1;
            cur.push(ch);
            continue;
        }
        if ch == '>' && !in_dquote && !in_squote && angle > 0 {
            angle -= 1;
            cur.push(ch);
            continue;
        }
        if ch == '|' && angle == 0 && !in_dquote && !in_squote {
            out.push(cur.trim().to_string());
            cur.clear();
            continue;
        }
        cur.push(ch);
    }
    if !cur.trim().is_empty() || (!out.is_empty() && input.ends_with('|')) {
        out.push(cur.trim().to_string());
    }
    if out.is_empty() {
        vec![input.to_string()]
    } else {
        out
    }
}

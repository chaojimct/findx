//! 内容倒排（tantivy）。schema v1，字段冻结；升级 = 版本号 bump + 全量重建。
//!
//! 并发模型：`IndexWriter` 本身线程安全（内部 craving），抽取与入库都在 rayon
//! 池里并行；`commit` 仍由调用方串行触发（分批 commit 定界内存）。
//! reader 每次 search 前 `reload()`，保证读到已 commit 数据。
//!
//! 多卷注意：FRN 只在卷内唯一，upsert/delete 键 = `doc_key(volume_id, frn)`
//!（两卷同 FRN 互不覆盖）。

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::Path;

use parking_lot::RwLock;
use tantivy::collector::TopDocs;
use tantivy::query::QueryParser;
use tantivy::schema::{
    Field, IndexRecordOption, Schema, TextFieldIndexing, TextOptions, Value, FAST, INDEXED,
    STORED,
};
use tantivy::tokenizer::{
    BoxTokenStream, LowerCaser, RemoveLongFilter, SimpleTokenizer, TextAnalyzer, Tokenizer,
};
use tantivy::{doc, Index, IndexReader, IndexWriter, ReloadPolicy, Term};
use tantivy_jieba::JiebaTokenizer;

use crate::{Error, Result};

/// 内容 schema 版本。v2：body 去 STORED（体积杀手），snippet 改按需回读；
/// 改字段必须 bump 并全量重建。
pub const SCHEMA_VERSION: u32 = 2;
pub const TOKENIZER_JIEBA: &str = "jieba_findx";

/// 全文匹配模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MatchMode {
    /// 整串连续出现（PhraseQuery slop=0）——「全匹配」。
    Exact,
    /// 分词后全部词都要有（默认 AND）。
    All,
    /// 分词后任一命中（OR）。
    #[default]
    Any,
}
/// 列表摘要单文件上限（字节）：超过直接缺席、选中后单窗补取。
/// 实测 18MB xlsx 解析一次约 25 秒、7.6MB js 约 5 秒——列表装配等不起；
/// 4MB 以下 office/文本通常在数百毫秒内完成。
pub const SNIPPET_LIST_MAX_BYTES: u64 = 4 << 20;
/// 列表摘要全局预算（毫秒）：`snippets_for_keys_windowed` 逐 key 检查，
/// 超时即停（剩下的行懒取），保证搜索 format 阶段不拖过 GUI 超时。
pub const SNIPPET_LIST_BUDGET_MS: u64 = 1000;

/// 索引目录里的版本标记文件（tantivy 自家 meta.json 之外，我方 schema 版本）。
const SCHEMA_VERSION_FILE: &str = "schema_version.json";

pub(crate) fn read_schema_version(dir: &Path) -> Option<u32> {
    let txt = std::fs::read_to_string(dir.join(SCHEMA_VERSION_FILE)).ok()?;
    txt.trim().parse().ok()
}

fn write_schema_version(dir: &Path) -> Result<()> {
    std::fs::write(
        dir.join(SCHEMA_VERSION_FILE),
        SCHEMA_VERSION.to_string(),
    )
    .map_err(|e| Error::Io(dir.display().to_string(), e))
}

/// 混合分词：纯 ASCII 文本（代码/日志/英文文档，占全盘大多数）走快道
///（切分 + 去超长 token + 小写），不碰 jieba；含非 ASCII 才走结巴。
/// 快道单篇提速一个数量级以上，是全量构建超越单线程方案的关键之一。
/// 小写与 AnyTXT 的 FoldCase 对齐；老索引本来就是大小写敏感，
/// 新索引统一小写后英文搜索只会更准（内容索引尚无线上兼容负担）。
///
/// `JiebaTokenizer::new()` 背后是全局静态实例，Clone 零成本；
/// tantivy 给每个索引线程 clone 一份，不存在跨线程争用。
#[derive(Clone)]
pub struct HybridTokenizer {
    jieba: JiebaTokenizer,
    ascii: TextAnalyzer,
}

impl HybridTokenizer {
    pub fn new() -> Self {
        Self {
            jieba: JiebaTokenizer::new(),
            ascii: TextAnalyzer::builder(SimpleTokenizer::default())
                .filter(RemoveLongFilter::limit(40))
                .filter(LowerCaser)
                .build(),
        }
    }
}

impl Default for HybridTokenizer {
    fn default() -> Self {
        Self::new()
    }
}

impl Tokenizer for HybridTokenizer {
    type TokenStream<'a> = BoxTokenStream<'a>;

    fn token_stream<'a>(&'a mut self, text: &'a str) -> BoxTokenStream<'a> {
        if text.is_ascii() {
            BoxTokenStream::new(self.ascii.token_stream(text))
        } else {
            BoxTokenStream::new(self.jieba.token_stream(text))
        }
    }
}

pub struct ContentFields {
    pub key: Field,
    pub frn: Field,
    pub volume: Field,
    pub path: Field,
    pub mtime: Field,
    pub size: Field,
    pub body: Field,
}

/// upsert/delete 键：同一 FRN 在不同卷下必须不同。
pub fn doc_key(volume_id: &str, frn: u64) -> u64 {
    let mut h = DefaultHasher::new();
    volume_id.hash(&mut h);
    frn.hash(&mut h);
    h.finish()
}

pub(crate) fn build_schema() -> (Schema, ContentFields) {
    let mut sb = Schema::builder();
    let key = sb.add_u64_field("key", INDEXED | STORED);
    let frn = sb.add_u64_field("frn", STORED | FAST);
    let volume = sb.add_text_field("volume", STORED);
    let path = sb.add_text_field("path", STORED);
    let mtime = sb.add_u64_field("mtime", FAST | STORED);
    let size = sb.add_u64_field("size", FAST | STORED);
    let body_opts = TextOptions::default().set_indexing_options(
        TextFieldIndexing::default()
            .set_tokenizer(TOKENIZER_JIEBA)
            .set_index_option(IndexRecordOption::WithFreqsAndPositions),
    );
    // v2：body 不再 STORED（全文原文是体积杀手：v1 体积比 86%）。
    // snippet 改按需回读原文生成（snippet.rs），体积只剩倒排。
    let body = sb.add_text_field("body", body_opts);
    let schema = sb.build();
    (
        schema,
        ContentFields {
            key,
            frn,
            volume,
            path,
            mtime,
            size,
            body,
        },
    )
}

pub struct DocMeta {
    pub frn: u64,
    pub volume_id: String,
    pub path: String,
    pub mtime: u64,
    pub size: u64,
}

pub struct ContentHit {
    pub volume_id: String,
    pub frn: u64,
    pub score: f32,
    pub snippet: String,
}

pub struct ContentIndex {
    index: Index,
    fields: ContentFields,
    reader: IndexReader,
    /// 读写锁：抽取/写入并发走读锁（writer 内部线程安全），`commit` 走写锁。
    /// commit 由调用方串行触发，写锁永不争用。
    writer: RwLock<IndexWriter>,
    /// 按需 snippet 池（回读原文 + 缓存）。
    snippets: crate::snippet::SnippetPool,
}

/// 内容索引目录定位：`index.bin` → `<index>.content/`（与 `.bin.v6/` 并列）。
pub fn content_dir_for(index_bin: &Path) -> std::path::PathBuf {
    let mut dir = index_bin.as_os_str().to_owned();
    dir.push(".content");
    std::path::PathBuf::from(dir)
}

/// 目录里有 tantivy 库但 schema 版本对不上（旧版二进制/旧代码建的库）：
/// 调用方应提示重建，而不是当成“未建”。避免新旧二进制混跑时静默走慢路径。
pub fn schema_mismatch(index_bin: &Path) -> bool {
    let dir = content_dir_for(index_bin);
    dir.join("meta.json").is_file() && read_schema_version(&dir) != Some(SCHEMA_VERSION)
}

/// 索引目录已建好才打开（`meta.json` 为 tantivy 建成标志 + 我方 schema 版本一致），
/// 否则 None（调用方走旧慢路径；版本不一致也视为未建，触发重建）。
pub fn open_if_exists(index_bin: &Path) -> Option<ContentIndex> {
    let dir = content_dir_for(index_bin);
    if !dir.join("meta.json").is_file() {
        return None;
    }
    if read_schema_version(&dir) != Some(SCHEMA_VERSION) {
        return None;
    }
    open_or_create(&dir).ok()
}

pub fn open_or_create(dir: &Path) -> Result<ContentIndex> {
    let (schema, fields) = build_schema();
    std::fs::create_dir_all(dir).map_err(|e| Error::Io(dir.display().to_string(), e))?;
    let existed = dir.join("meta.json").is_file();
    if existed && read_schema_version(dir) != Some(SCHEMA_VERSION) {
        return Err(Error::Index(format!(
            "内容索引 schema 版本不匹配（期望 v{SCHEMA_VERSION}），需重建"
        )));
    }
    let index = Index::open_or_create(
        tantivy::directory::MmapDirectory::open(dir)
            .map_err(|e| Error::Index(format!("打开内容索引目录失败: {e}")))?,
        schema,
    )?;
    if !existed {
        write_schema_version(dir)?;
    }
    index
        .tokenizers()
        .register(TOKENIZER_JIEBA, HybridTokenizer::new());
    let reader = index
        .reader_builder()
        .reload_policy(ReloadPolicy::Manual)
        .try_into()?;
    // 多线程 writer：抽取（rayon）与分词/写入（tantivy 内部线程池）双并行。
    // 线程/堆按机器自适应（低配机保守，见 BuildConfig）。
    let cfg = crate::build::BuildConfig::auto();
    let writer = RwLock::new(index.writer_with_num_threads(cfg.index_threads, cfg.heap_bytes)?);
    Ok(ContentIndex {
        index: index.clone(),
        fields,
        reader,
        writer,
        snippets: crate::snippet::SnippetPool::new(),
    })
}

impl ContentIndex {
    /// upsert 语义：先删同 key 旧 doc 再加（tantivy doc 不可变）。
    /// `&self` 即可并发调用（writer 内部线程安全）；同一 key 的并发 upsert
    /// 由调用方保证不重叠（规划按批次串行，批内 key 唯一）。
    pub fn index_doc(&self, meta: &DocMeta, body: &str) -> Result<()> {
        let w = self.writer.read();
        w.delete_term(Term::from_field_u64(
            self.fields.key,
            doc_key(&meta.volume_id, meta.frn),
        ));
        w.add_document(doc!(
            self.fields.key => doc_key(&meta.volume_id, meta.frn),
            self.fields.frn => meta.frn,
            self.fields.volume => meta.volume_id.clone(),
            self.fields.path => meta.path.clone(),
            self.fields.mtime => meta.mtime,
            self.fields.size => meta.size,
            self.fields.body => body.to_owned(),
        ))?;
        Ok(())
    }

    pub fn delete_by_frn(&self, volume_id: &str, frn: u64) -> Result<()> {
        self.writer.read().delete_term(Term::from_field_u64(
            self.fields.key,
            doc_key(volume_id, frn),
        ));
        Ok(())
    }

    pub fn commit(&self) -> Result<()> {
        self.writer.write().commit()?;
        self.reader.reload()?;
        Ok(())
    }

    /// 终局合并：把所有段并成一个（查询更快、体积最小）。
    /// 全量构建结束时调一次（增量构建不调，靠后台自然合并）。
    /// 耗时与体积成正比（GB 级约数分钟），失败也不影响使用（返回 Err 即可忽略）。
    pub fn optimize(&self) -> Result<()> {
        let ids = self.index.searchable_segment_ids().map_err(Error::from)?;
        if ids.len() > 1 {
            let fut = { self.writer.write().merge(&ids) };
            let _ = fut.wait().map_err(Error::from)?;
        }
        self.reader.reload()?;
        Ok(())
    }

    pub fn num_docs(&self) -> u64 {
        self.reader.searcher().num_docs()
    }

    /// 快查询：只要 (volume_id, frn, score)，不生成 snippet（展示页按需取）。
    pub fn search_ids(&self, query_text: &str, limit: usize) -> Result<Vec<(String, u64, f32)>> {
        self.search_ids_mode(query_text, limit, MatchMode::Any)
    }

    /// 全文检索（显式匹配模式）。
    /// - `Exact`：整串连续出现（分词后 PhraseQuery slop=0）——「全匹配」。
    /// - `All`：分词后全部词都要有（BooleanQuery Must）——默认 AND。
    /// - `Any`：分词后任一命中（Should）——OR 模式 / 旧行为。
    pub fn search_ids_mode(
        &self,
        query_text: &str,
        limit: usize,
        mode: MatchMode,
    ) -> Result<Vec<(String, u64, f32)>> {
        use tantivy::query::{
            BooleanQuery, Occur, PhraseQuery, Query, TermQuery,
        };
        use tantivy::schema::IndexRecordOption;
        use tantivy::Term;
        self.reader.reload()?;
        let searcher = self.reader.searcher();
        let field = self.fields.body;
        let mut tok = HybridTokenizer::new();
        // 取分词结果 + **索引里的真实 position**：jieba 对「学习积分」给出
        // ("学习",0),("积分",2)——位置跳号，普通 PhraseQuery（要求逐 1 相邻）
        // 永远匹配不上。Exact 模式必须用 new_with_offset 按原位置构造。
        let (terms, term_positions) = {
            use tantivy::tokenizer::{TokenStream, Tokenizer};
            let mut st = tok.token_stream(query_text);
            let mut terms = Vec::new();
            let mut pos = Vec::new();
            while st.advance() {
                let tk = st.token();
                if tk.text.is_empty() || !tk.text.chars().any(|c| c.is_alphanumeric()) {
                    continue;
                }
                terms.push(tk.text.clone());
                pos.push(tk.position as usize);
            }
            (terms, pos)
        };
        if terms.is_empty() {
            return Ok(Vec::new());
        }
        let query: Box<dyn Query> = match mode {
            MatchMode::Exact => {
                // 用分词时的真实 position（jieba 可能跳号，见上）。
                let ts: Vec<(usize, Term)> = terms
                    .iter()
                    .zip(term_positions.iter())
                    .map(|(t, p)| (*p, Term::from_field_text(field, t)))
                    .collect();
                if ts.len() == 1 {
                    Box::new(TermQuery::new(
                        ts[0].1.clone(),
                        IndexRecordOption::WithFreqs,
                    ))
                } else {
                    Box::new(PhraseQuery::new_with_offset(ts))
                }
            }
            MatchMode::All => {
                let sub: Vec<(Occur, Box<dyn Query>)> = terms
                    .iter()
                    .map(|t| {
                        (
                            Occur::Must,
                            Box::new(TermQuery::new(
                                Term::from_field_text(field, t),
                                IndexRecordOption::WithFreqs,
                            )) as Box<dyn Query>,
                        )
                    })
                    .collect();
                Box::new(BooleanQuery::new(sub))
            }
            MatchMode::Any => {
                let sub: Vec<(Occur, Box<dyn Query>)> = terms
                    .iter()
                    .map(|t| {
                        (
                            Occur::Should,
                            Box::new(TermQuery::new(
                                Term::from_field_text(field, t),
                                IndexRecordOption::WithFreqs,
                            )) as Box<dyn Query>,
                        )
                    })
                    .collect();
                Box::new(BooleanQuery::new(sub))
            }
        };
        let top = searcher.search(&query, &TopDocs::with_limit(limit).order_by_score())?;
        let mut out = Vec::with_capacity(top.len());
        for (score, addr) in top {
            let doc: tantivy::TantivyDocument = searcher.doc(addr)?;
            let frn = doc
                .get_first(self.fields.frn)
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let volume_id = doc
                .get_first(self.fields.volume)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_owned();
            out.push((volume_id, frn, score));
        }
        Ok(out)
    }

    /// 拼音辅索引查询（只接受纯 ASCII 查询；含 CJK 直接返回空，调用方走主索引）。
    ///
    /// 单 token（如 `qw`）：精确 OR 前缀（`^qw.*`，块首匹配）。
    /// `for_full`（全拼侧）连写全拼（如 `quanwen`）：切音节后严格连续
    /// （Phrase），散音节文档不再召回；半个尾音降级 AND + 前缀。
    /// 多 token（如 `quan wen`）：OR 相加，命中越多排名越前（空格写法兼容
    /// “拼音+英文”混合，保持原语义）。
    /// 返回 (volume_id, frn, score)，与 search_ids 同形，方便服务层合并。
    /// `for_full` 区分辅库种类：全拼侧连写查询（`machuntian`）是严格连续语义
    /// （Phrase/AND，打一半不断流）；首字母侧保持“精确 OR 前缀”，音节词不掺入
    /// （音节不可能出现在首字母块里，掺入只会召回噪声）。
    pub fn search_pinyin(
        &self,
        query_text: &str,
        limit: usize,
        for_full: bool,
    ) -> Result<Vec<(String, u64, f32)>> {
        use tantivy::query::{BooleanQuery, Occur, PhraseQuery, RegexQuery, TermQuery};
        use tantivy::Term;
        if !query_text.is_ascii() {
            return Ok(Vec::new());
        }
        let mut tok = HybridTokenizer::new();
        let terms = crate::snippet::query_terms(query_text, &mut tok);
        if terms.is_empty() {
            return Ok(Vec::new());
        }
        self.reader.reload()?;
        let searcher = self.reader.searcher();
        let field = self.fields.body;
        // 全拼侧连写（如 `machuntian`）：切音节后严格连续（Phrase），散音节
        // 文档不再召回——“多输一个音节即收窄”，与输入直觉一致；尾部半个音节
        // （如 `quanwe` 的 `we`）按 AND + 前缀，打一半不断流。
        // 模糊音：每音节展开平翘/前后鼻变体，笛卡尔封顶 8 个 phrase 或（超限回落精确）。
        // （空格写法 `ma chun tian` 保持 OR，兼容“拼音+英文”混合查询。）
        if for_full && terms.len() == 1 {
            let t = &terms[0];
            if let Some((heads, tail)) = crate::pinyin_aux::split_pinyin_query(t) {
                if tail.is_empty() && heads.len() >= 2 {
                    let mut lists: Vec<Vec<&str>> = Vec::with_capacity(heads.len());
                    for h in &heads {
                        let mut v = vec![h.as_str()];
                        v.extend(crate::pinyin_aux::fuzzy_variants(h));
                        lists.push(v);
                    }
                    // 笛卡尔积上限，避免长串组合爆炸。
                    let mut combos: Vec<Vec<&str>> = vec![Vec::new()];
                    for opts in &lists {
                        if combos.len() * opts.len() > 8 {
                            combos.clear();
                            break;
                        }
                        let mut next = Vec::with_capacity(combos.len() * opts.len());
                        for prefix in &combos {
                            for o in opts {
                                let mut c = prefix.clone();
                                c.push(o);
                                next.push(c);
                            }
                        }
                        combos = next;
                    }
                    if !combos.is_empty() {
                        let subs: Vec<(Occur, Box<dyn tantivy::query::Query>)> = combos
                            .into_iter()
                            .map(|combo| {
                                let ts: Vec<Term> = combo
                                    .into_iter()
                                    .map(|h| Term::from_field_text(field, h))
                                    .collect();
                                (
                                    Occur::Should,
                                    Box::new(PhraseQuery::new(ts))
                                        as Box<dyn tantivy::query::Query>,
                                )
                            })
                            .collect();
                        return self.top_aux(&searcher, &BooleanQuery::new(subs), limit);
                    }
                    // 超限回落精确 phrase。
                    let phrase_terms: Vec<Term> = heads
                        .iter()
                        .map(|h| Term::from_field_text(field, h))
                        .collect();
                    let query = PhraseQuery::new(phrase_terms);
                    return self.top_aux(&searcher, &query, limit);
                }
                if !tail.is_empty() {
                    let mut subs: Vec<(Occur, Box<dyn tantivy::query::Query>)> =
                        Vec::with_capacity(heads.len() + 1);
                    for h in &heads {
                        subs.push((
                            Occur::Must,
                            Box::new(TermQuery::new(
                                Term::from_field_text(field, h),
                                IndexRecordOption::Basic,
                            ))
                                as Box<dyn tantivy::query::Query>,
                        ));
                    }
                    if let Ok(re) = RegexQuery::from_pattern(&format!("{tail}.*"), field) {
                        subs.push((Occur::Must, Box::new(re) as Box<dyn tantivy::query::Query>));
                    }
                    return self.top_aux(&searcher, &BooleanQuery::new(subs), limit);
                }
            }
        }
        let query: Box<dyn tantivy::query::Query> = if terms.len() == 1 {
            let t = &terms[0];
            // token 已是小写字母数字（simple+lower），无正则元字符，可直接拼 ^t.*
            // （首字母串走这里；全拼侧切分失败的也回落到这里，如单音节 `ma`）。
            let re = RegexQuery::from_pattern(&format!("{t}.*"), field)
                .map_err(|e| Error::Index(format!("拼音正则查询构造失败: {e}")))?;
            Box::new(BooleanQuery::new(vec![
                (
                    Occur::Should,
                    Box::new(TermQuery::new(
                        Term::from_field_text(field, t),
                        IndexRecordOption::Basic,
                    ))
                        as Box<dyn tantivy::query::Query>,
                ),
                (Occur::Should, Box::new(re) as Box<dyn tantivy::query::Query>),
            ]))
        } else {
            let subs: Vec<(Occur, Box<dyn tantivy::query::Query>)> = terms
                .iter()
                .map(|t| {
                    (
                        Occur::Should,
                        Box::new(TermQuery::new(
                            Term::from_field_text(field, t),
                            IndexRecordOption::Basic,
                        ))
                            as Box<dyn tantivy::query::Query>,
                    )
                })
                .collect();
            Box::new(BooleanQuery::new(subs))
        };
        let top = searcher.search(&*query, &TopDocs::with_limit(limit).order_by_score())?;
        let mut out = Vec::with_capacity(top.len());
        for (score, addr) in top {
            let doc: tantivy::TantivyDocument = searcher.doc(addr)?;
            let frn = doc
                .get_first(self.fields.frn)
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let volume_id = doc
                .get_first(self.fields.volume)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_owned();
            out.push((volume_id, frn, score));
        }
        Ok(out)
    }

    /// 辅库 TopDocs 收集（phrase/boolean 早退路径共用）。
    fn top_aux(
        &self,
        searcher: &tantivy::Searcher,
        query: &dyn tantivy::query::Query,
        limit: usize,
    ) -> Result<Vec<(String, u64, f32)>> {
        let top = searcher.search(query, &TopDocs::with_limit(limit).order_by_score())?;
        let mut out = Vec::with_capacity(top.len());
        for (score, addr) in top {
            let doc: tantivy::TantivyDocument = searcher.doc(addr)?;
            let frn = doc
                .get_first(self.fields.frn)
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let volume_id = doc
                .get_first(self.fields.volume)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_owned();
            out.push((volume_id, frn, score));
        }
        Ok(out)
    }
    /// 按 upsert/delete 键批量取 snippet（service 搜索装配用）。
    ///
    /// v2 按需回读：doc 存 path（STORED）→ 回读原文切窗口（snippet.rs）。
    /// 返回 `key → snippet HTML`；找不到/回读失败/无命中的 key 缺席（调用方回退空串）。
    /// `cap` 截断输入规模，防大页 full-scan。
    pub fn snippets_for_keys(
        &self,
        query_text: &str,
        keys: &[u64],
        cap: usize,
    ) -> Result<std::collections::HashMap<u64, String>> {
        Ok(self
            .snippets_for_keys_windowed(query_text, keys, cap, 0)?
            .into_iter()
            .map(|(k, (html, _, _))| (k, html))
            .collect())
    }

    /// 按 key 批量取第 `window_idx` 个匹配窗口的 snippet（上一个/下一个用），
    /// 返回 `key → (html, 全文匹配点总数)`；找不到/回读失败的 key 缺席。
    ///
    /// 列表装配有界成本（搜索超时的主因曾是这里）：
    /// - 超大文件（`> SNIPPET_LIST_MAX_BYTES`，如十几 MB 的 xlsx 解析一次 25 秒）
    ///   直接缺席——行不受影响，选中后按需走单窗补取；
    /// - 全局预算 `SNIPPET_LIST_BUDGET_MS`，超时即停，剩下的行同样懒取。
    pub fn snippets_for_keys_windowed(
        &self,
        query_text: &str,
        keys: &[u64],
        cap: usize,
        window_idx: usize,
    ) -> Result<std::collections::HashMap<u64, (String, usize, usize)>> {
        use std::collections::HashMap;
        let mut out = HashMap::new();
        if keys.is_empty() || cap == 0 {
            return Ok(out);
        }
        let started = std::time::Instant::now();
        self.reader.reload()?;
        let searcher = self.reader.searcher();
        let mut tok = HybridTokenizer::new();
        let terms = crate::snippet::query_terms(query_text, &mut tok);
        if terms.is_empty() {
            return Ok(out);
        }
        for key in keys.iter().take(cap) {
            if started.elapsed().as_millis() as u64 >= SNIPPET_LIST_BUDGET_MS {
                break;
            }
            let tq = tantivy::query::TermQuery::new(
                Term::from_field_u64(self.fields.key, *key),
                IndexRecordOption::Basic,
            );
            let top = searcher.search(&tq, &TopDocs::with_limit(1).order_by_score())?;
            let Some((_, addr)) = top.first() else {
                continue;
            };
            let doc: tantivy::TantivyDocument = searcher.doc(*addr)?;
            let path = doc
                .get_first(self.fields.path)
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if path.is_empty() {
                continue;
            }
            let mtime = doc
                .get_first(self.fields.mtime)
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            // 超大文件跳过列表摘要（office/大文本解析一次可达数十秒：18MB xlsx
            // 约 25 秒）：行保留（无 snippet），选中后单窗按需补取。
            // 用实时 stat 而不用索引里存的 size（后者在回填期可能为零/过期）；
            // stat 不触发云占位下载，10 个 key 的开销可忽略。
            let too_big = std::fs::metadata(path)
                .map(|m| m.len() > SNIPPET_LIST_MAX_BYTES)
                .unwrap_or(false);
            if too_big {
                continue;
            }
            if let Some((html, total, window_used)) =
                self.snippets
                    .for_doc_windowed(path, mtime, query_text, &terms, window_idx)
            {
                out.insert(*key, (html, total, window_used));
            }
        }
        Ok(out)
    }

    /// 按 key 取全部匹配窗口（AnyTXT 式编号列表用）：一次回读，逐窗渲染。
    /// 返回 `(Vec<(实际窗口号, html)>, 全文匹配点总数)`；`cap` 截断返回个数。
    pub fn all_windows_for_key(
        &self,
        query_text: &str,
        key: u64,
        cap: usize,
    ) -> Result<Option<(Vec<(usize, String)>, usize)>> {
        if cap == 0 {
            return Ok(None);
        }
        self.reader.reload()?;
        let searcher = self.reader.searcher();
        let mut tok = HybridTokenizer::new();
        let terms = crate::snippet::query_terms(query_text, &mut tok);
        if terms.is_empty() {
            return Ok(None);
        }
        let tq = tantivy::query::TermQuery::new(
            Term::from_field_u64(self.fields.key, key),
            IndexRecordOption::Basic,
        );
        let top = searcher.search(&tq, &TopDocs::with_limit(1).order_by_score())?;
        let Some((_, addr)) = top.first() else {
            return Ok(None);
        };
        let doc: tantivy::TantivyDocument = searcher.doc(*addr)?;
        let path = doc
            .get_first(self.fields.path)
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if path.is_empty() {
            return Ok(None);
        }
        // 超大文件跳过全窗列表（与 snippets_for_keys_windowed 同口径）：
        // 单窗走按需补取通道，列表不为一个巨文件卡住整个面板。
        if std::fs::metadata(path)
            .map(|m| m.len() > SNIPPET_LIST_MAX_BYTES)
            .unwrap_or(false)
        {
            return Ok(None);
        }
        let mtime = doc
            .get_first(self.fields.mtime)
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        Ok(self.snippets.all_windows(path, mtime, query_text, &terms, cap))
    }

    /// 全文文本预览用：一次抽取（缓存）+ 命中 spans；输出截断到 `wire_cap`
    ///（char 边界），`truncated` 明示是否被体积/上限截断；超大文件直接 None
    ///（调用方回退摘要窗）。返回 `(文本, 命中字节区间, 全文匹配点总数, 是否截断)`。
    pub fn full_text_for_key(
        &self,
        query_text: &str,
        key: u64,
        wire_cap: usize,
    ) -> Result<Option<(String, Vec<(usize, usize)>, usize, bool)>> {
        let mut tok = HybridTokenizer::new();
        let terms = crate::snippet::query_terms(query_text, &mut tok);
        if terms.is_empty() {
            return Ok(None);
        }
        self.reader.reload()?;
        let searcher = self.reader.searcher();
        let tq = tantivy::query::TermQuery::new(
            Term::from_field_u64(self.fields.key, key),
            IndexRecordOption::Basic,
        );
        let top = searcher.search(&tq, &TopDocs::with_limit(1).order_by_score())?;
        let Some((_, addr)) = top.first() else {
            return Ok(None);
        };
        let doc: tantivy::TantivyDocument = searcher.doc(*addr)?;
        let path = doc
            .get_first(self.fields.path)
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if path.is_empty() {
            return Ok(None);
        }
        let mtime = doc
            .get_first(self.fields.mtime)
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let text = match self.snippets.full_text(path, mtime) {
            Some(t) => t,
            None => return Ok(None),
        };
        let mut spans = crate::snippet::match_spans(&text, &terms);
        let total = spans.len();
        // 截断到传输上限（char 边界），命中区间同步裁剪，总数照实。
        let mut truncated = false;
        let mut out = text.to_string();
        if out.len() > wire_cap {
            let mut n = wire_cap.min(out.len());
            while !out.is_char_boundary(n) {
                n -= 1;
            }
            out.truncate(n);
            out.push_str("\n[内容过长，仅显示开头部分]");
            truncated = true;
        }
        spans.retain(|(a, _)| *a < out.len());
        Ok(Some((out, spans, total, truncated)))
    }

    /// 返回按 BM25 排序的 (volume_id, frn, score, snippet)。
    pub fn search(&self, query_text: &str, limit: usize) -> Result<Vec<ContentHit>> {
        self.reader.reload()?;
        self.reader.reload()?;
        let searcher = self.reader.searcher();
        let qp = QueryParser::for_index(searcher.index(), vec![self.fields.body]);
        let query = qp.parse_query(query_text).map_err(Error::from)?;
        let top = searcher.search(&query, &TopDocs::with_limit(limit).order_by_score())?;
        let mut tok = HybridTokenizer::new();
        let terms = crate::snippet::query_terms(query_text, &mut tok);
        // snippet 按需回读原文，贵：只给前 5 个生成，其余空串（展示页按需取）。
        let snippet_cap = top.len().min(5);
        let mut out = Vec::with_capacity(top.len());
        for (i, (score, addr)) in top.into_iter().enumerate() {
            let doc: tantivy::TantivyDocument = searcher.doc(addr)?;
            let frn = doc
                .get_first(self.fields.frn)
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let volume_id = doc
                .get_first(self.fields.volume)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_owned();
            let snippet = if i < snippet_cap && !terms.is_empty() {
                let path = doc
                    .get_first(self.fields.path)
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let mtime = doc
                    .get_first(self.fields.mtime)
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                if path.is_empty() {
                    String::new()
                } else {
                    self.snippets
                        .for_doc(path, mtime, query_text, &terms)
                        .unwrap_or_default()
                }
            } else {
                String::new()
            };
            out.push(ContentHit {
                volume_id,
                frn,
                score,
                snippet,
            });
        }
        Ok(out)
    }
}

impl findx2_core::ContentSearch for ContentIndex {
    fn search_content(
        &self,
        query: &str,
        limit: usize,
        mode: findx2_core::ContentMatchMode,
    ) -> std::result::Result<Vec<(String, u64)>, findx2_core::Error> {
        let m = match mode {
            findx2_core::ContentMatchMode::Exact => MatchMode::Exact,
            findx2_core::ContentMatchMode::All => MatchMode::All,
            findx2_core::ContentMatchMode::Any => MatchMode::Any,
        };
        self.search_ids_mode(query, limit, m)
            .map(|hits| {
                hits.into_iter()
                    .map(|(volume_id, frn, _)| (volume_id, frn))
                    .collect()
            })
            .map_err(|e| findx2_core::Error::Persist(format!("内容索引查询失败: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tantivy::tokenizer::{TokenStream, Tokenizer};

    fn tokens_of(text: &str) -> Vec<String> {
        let mut t = HybridTokenizer::new();
        let mut stream = t.token_stream(text);
        let mut out = Vec::new();
        while stream.advance() {
            out.push(stream.token().text.clone());
        }
        out
    }

    #[test]
    fn ascii_fast_path_tokenizes() {
        // ASCII 快道：切分 + 小写 + 去超长（和 AnyTXT FoldCase 对齐）。
        let toks = tokens_of("Hello World foo 123");
        assert!(toks.iter().any(|t| t == "hello"));
        assert!(toks.iter().any(|t| t == "foo"));
        assert!(!toks.is_empty());
        // 40 字符以上 token 被丢弃（minified/哈希行不进索引）。
        let long = "a".repeat(41);
        assert!(tokens_of(&long).is_empty());
    }

    #[test]
    fn cjk_still_segments() {
        // 含 CJK 走结巴： smoke 级断言不断言具体切分（词典版本相关）。
        let toks = tokens_of("全文搜索 hello world");
        assert!(toks.len() >= 2);
        assert!(toks.iter().any(|t| t.contains("全文") || t.contains("搜索")));
    }
}

//! 内容倒排（tantivy）。schema v1，字段冻结；升级 = 版本号 bump + 全量重建。
//!
//! 并发模型：`IndexWriter`  behind `Mutex`，调用方（M1 增量线程）串行 commit 即可；
//! reader 每次 search 前 `reload()`，保证读到已 commit 数据。

use std::path::Path;

use parking_lot::Mutex;
use tantivy::collector::TopDocs;
use tantivy::query::QueryParser;
use tantivy::schema::{
    Field, IndexRecordOption, Schema, TextFieldIndexing, TextOptions, Value, FAST, INDEXED,
    STORED,
};
use tantivy::snippet::SnippetGenerator;
use tantivy::{doc, Index, IndexReader, IndexWriter, ReloadPolicy, Term};
use tantivy_jieba::JiebaTokenizer;

use crate::{Error, Result};

/// 内容 schema 版本。v1 字段冻结，改字段必须 bump 并全量重建。
pub const SCHEMA_VERSION: u32 = 1;
pub const TOKENIZER_JIEBA: &str = "jieba_findx";

pub struct ContentFields {
    pub frn: Field,
    pub volume: Field,
    pub path: Field,
    pub mtime: Field,
    pub size: Field,
    pub body: Field,
}

fn build_schema() -> (Schema, ContentFields) {
    let mut sb = Schema::builder();
    let frn = sb.add_u64_field("frn", INDEXED | STORED | FAST);
    let volume = sb.add_text_field("volume", STORED);
    let path = sb.add_text_field("path", STORED);
    let mtime = sb.add_u64_field("mtime", FAST | STORED);
    let size = sb.add_u64_field("size", FAST | STORED);
    let body_opts = TextOptions::default().set_indexing_options(
        TextFieldIndexing::default()
            .set_tokenizer(TOKENIZER_JIEBA)
            .set_index_option(IndexRecordOption::WithFreqsAndPositions),
    );
    // body STORED：M1 snippet 直接取，不回读原文件。
    let body = sb.add_text_field("body", body_opts | STORED);
    let schema = sb.build();
    (
        schema,
        ContentFields {
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
    pub frn: u64,
    pub score: f32,
    pub snippet: String,
}

pub struct ContentIndex {
    fields: ContentFields,
    reader: IndexReader,
    writer: Mutex<IndexWriter>,
}

pub fn open_or_create(dir: &Path) -> Result<ContentIndex> {
    let (schema, fields) = build_schema();
    std::fs::create_dir_all(dir)
        .map_err(|e| Error::Io(dir.display().to_string(), e))?;
    let index = Index::open_or_create(
        tantivy::directory::MmapDirectory::open(dir)
            .map_err(|e| Error::Index(format!("打开内容索引目录失败: {e}")))?,
        schema,
    )?;
    index
        .tokenizers()
        .register(TOKENIZER_JIEBA, JiebaTokenizer::new());
    let reader = index
        .reader_builder()
        .reload_policy(ReloadPolicy::Manual)
        .try_into()?;
    // M0 堆 256MB；M1 按机器内存自适应。
    let writer = Mutex::new(index.writer(256_000_000)?);
    Ok(ContentIndex {
        fields,
        reader,
        writer,
    })
}

impl ContentIndex {
    /// upsert 语义：先删同 frn 旧 doc 再加（tantivy doc 不可变）。
    pub fn index_doc(&self, meta: &DocMeta, body: &str) -> Result<()> {
        let w = self.writer.lock();
        w.delete_term(Term::from_field_u64(self.fields.frn, meta.frn));
        w.add_document(doc!(
            self.fields.frn => meta.frn,
            self.fields.volume => meta.volume_id.clone(),
            self.fields.path => meta.path.clone(),
            self.fields.mtime => meta.mtime,
            self.fields.size => meta.size,
            self.fields.body => body.to_owned(),
        ))?;
        Ok(())
    }

    pub fn delete_by_frn(&self, frn: u64) -> Result<()> {
        self.writer
            .lock()
            .delete_term(Term::from_field_u64(self.fields.frn, frn));
        Ok(())
    }

    pub fn commit(&self) -> Result<()> {
        self.writer.lock().commit()?;
        self.reader.reload()?;
        Ok(())
    }

    pub fn num_docs(&self) -> u64 {
        self.reader.searcher().num_docs()
    }

    /// 返回按 BM25 排序的 (frn, score, snippet)。
    pub fn search(&self, query_text: &str, limit: usize) -> Result<Vec<ContentHit>> {
        self.reader.reload()?;
        let searcher = self.reader.searcher();
        let qp = QueryParser::for_index(searcher.index(), vec![self.fields.body]);
        let query = qp
            .parse_query(query_text)
            .map_err(Error::from)?;
        let top = searcher.search(&query, &TopDocs::with_limit(limit).order_by_score())?;
        let snippet_gen = SnippetGenerator::create(&searcher, &*query, self.fields.body)?;
        let mut out = Vec::with_capacity(top.len());
        for (score, addr) in top {
            let doc: tantivy::TantivyDocument = searcher.doc(addr)?;
            let frn = doc
                .get_first(self.fields.frn)
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let snippet = snippet_gen.snippet_from_doc(&doc).to_html();
            out.push(ContentHit {
                frn,
                score,
                snippet,
            });
        }
        Ok(out)
    }
}

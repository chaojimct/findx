# 内容索引 v4：精确匹配（对齐 AnyTXT）

2026-11-27。解决「搜 `马春天 我与青松` 却命中只含 `马` / 只含 `青松` 的文档」。

## 1. AnyTXT 是什么实现（静态逆向证据）

| 项 | 证据（ATService.exe / ATGUI.exe） |
|---|---|
| 倒排内核 | SQLite **FTS3/4**：`%_segdir` / `%_segments` / `%_content` / `vocab(term,doc,cnt)`、`SAVEPOINT fts3` |
| 中文分词 | 内嵌 **cppjieba 5.1.0**：`g:\vms\anytxt\trunk\include\cppjieba5.1.0\MixSegment.hpp`、`jieba.dict.utf8`、`stop_words.utf8`、RTTI `MPSegment/MixSegment/HMMSegment/QuerySegment` |
| 分词器 | 自研 `att`（注册名之一，基于 `unicode61`），建表参数 `prefix=1` + `tokenize=att ats_offsets=1 ats_prefix_unicode=1 ats_prefix_alnum=0` |
| 位置/偏移 | 自建 `%_offset(docid,col,block,firstpos,npos,data)` 存 **token 偏移**，供 `ats_offsets` / `ats_snippet` 高亮 |
| 搜索模式 | `1: Exact Match, 2: Advanced Match, 4: Regexp Match`，默认 `MatchType=1` |
| Exact 语义 | 语言包：*Do a whole words match searching. For CJK, please input phrases.* |
| 单字/半截词 | `prefix=1`（1 字前缀索引）→ 词前缀查询 |

**结论**：倒排 + **jieba 词级 token + 词序号 position**，不是字符级/bigram；
多词 AND，词内多 token 要求位置相邻（短语）。`prefix=1` 只用于单字/前缀召回。

## 2. 我们错在哪

`tantivy-jieba` 默认 `position = 字符偏移`（实测 `学习@0 / 积分@2`），
tantivy `PhraseQuery` 要求 position 连续 → 短语查询永远失效 →
退化成 token 的 `BooleanQuery(Must)`：

```
马春天 我与青松 → 马 AND 春天 AND 我 AND 与 AND 青松
```

任何包含这些字的文档都命中。这就是误召回的根因，不是倒排的问题。

## 3. v4 实现（`crates/findx2-content/src/index.rs`）

`HybridTokenizer`（索引与查询共用，保证两侧一致）：

- CJK 连续段（汉字/假名/谚文）→ jieba 切词，`position` 逐 1 递增（**词序号**）；
- 拉丁/数字连续段 → 整词一个 token（小写，≤40 字节）；
- 空白/标点 → 跳过，**不占 position**（短语可跨标点）；
- 单字 CJK 查询 → `PhrasePrefixQuery`（等价 AnyTXT `prefix=1`）。

查询构造（`content_word_query`）：

| 查询词 | tantivy 查询 |
|---|---|
| 1 个 token | `TermQuery`（单字 CJK → 前缀查询） |
| 多个 token | `PhraseQuery`（slop=0，原文必须连续） |
| 多个 token 且纯 CJK | `PhraseQuery` OR `整词前缀`（兜底 jieba 在查询/文档上下文切分不一致） |

模式（`MatchMode`）：

- `All`（默认）：每个空白词都要命中（AND），词内用短语；
- `Any`（OR 开关）：任一空白词命中；
- `Exact`（全匹配）：整串所有 token 连成一个短语，原文严格连续。

## 4. 重建

分词器/位置语义变更 → `SCHEMA_VERSION = 4`，必须全量重建内容索引：

```
findx2 content-index --index <index.bin> --rebuild
```

（GUI 全局页「重建内容索引」同效；重建期间 `content:` 走慢速逐文件，结果可能不全。）

## 5. 验收（单测 `index::tests`）

- `马春天` 只命中「我与青松——马春天采访文稿」，**不命中**「马加入春天」；
- `马春天 我与青松` AND 只命中 1 篇；`Exact`（顺序相反）为 0；
- 单字 `青` 命中「青松测评学校」，`岛` 为 0；
- 中文夹英文 `2024年度报告` 中 `hello/world/2024/年度` 都是整词 token。

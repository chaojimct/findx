# findx 全文内容搜索（M0+M1）：设计与使用

> 状态：`feat/content-search` 分支可用。文件名索引零改动（只加了 `SearchOptions.content`
> 可选装配 + `content:` 无索引时回退修复）；内容倒排独立 crate。

## 架构

```
文件变更（USN/inotify/FSEvents，复用现有通道）
  → service content_index::notify_applied（热路径只提 job，不碰 store/文件）
  → 后台线程 resolve 路径 → 抽取 → tantivy upsert/delete → 周期 commit
查询：content: → SearchOptions.content（Arc<dyn ContentSearch>）
  → tantivy 查 (volume_id, frn) → frn_to_entry.get_scoped → 与其它谓词求交
```

- `crates/findx2-content`：`extract`（txt/md/代码 80+ 扩展名 + GB18030 回退）、
  `office`（docx/xlsx/pptx 纯 Rust ooxml + pdf-extract）、`index`（tantivy 0.27 +
  jieba，schema v1，key=`hash(volume,frn)` 多卷安全）、`build`（全量构建，
  CLI 与 service/GUI 共用）、测试（单测 + engine 缝合 + 全链路）。
- service：`content_index.rs` 全局 Hub（`init`/`notify_applied`/`content_opt`），
  `run.rs` + `run_unix.rs` 启动挂载，`NamespaceGate::apply_bound` 与 Windows
  `flush_pending` 双钩子，`search_ipc` + Everything `do_search` 装配快路径。
- CLI：`content-index [--rebuild]` 构建，`search`/`remote` 自动用索引，
  `status` 显示内容索引状态。

## 使用

```powershell
# 1. 已有 index.bin（service 常驻或 CLI 建库均可）
# 2. 建内容索引（一次性；体积约为原文 30~130%，小库比例偏高是大词典摊薄）
findx2 content-index --index index.bin
# 3. 查（service/CLI 自动走 tantivy；没建则回退逐文件慢路径）
findx2 search --index index.bin "content:全文搜索"
findx2 search --index index.bin "content:Value ext:xlsx"
```

## 实测（M0 bench，189→5912 文件语料）

- 建库：~3MB/s 单线程（抽取主导，大文件更快）；840 文件 5.7MB → 索引 7.66MB。
- 查询：ids 2.5~4.5ms（与语料规模弱相关）；snippet 另付 7~30ms/前 5 个
  （按 doc 全文 jieba 分词，展示页按需取，`search_ids` 与 `search` 已分离）。
- 体积比小语料 133%（jieba 词典固定开销），随语料增大会降到 30~50% 区间。

## 已知边界（M2）

- 扫描版 PDF/图片、老 doc/xls 二进制、压缩包内文件、OCR：跳过并计数，不崩溃。
- 损坏 PDF（如手写 xref 偏移错误）：`pdf-extract` 严格解析失败 → 跳过。
  真实世界 PDF 验证通过（lopdf 自带 example.pdf）。
- 增量重抽无 mtime 指纹去重（Rename 零重抽已天然满足；Data 事件必重抽），
  channel 满整批丢（保搜索优先，重建自愈）。
- snippet 限前 5；`CONTENT_FAST_CAP=50_000` 截断（total 语义待 tantivy count 补）。
- GUI：结果分栏/snippet 展示未做（IPC DTO 不变，GUI 零改动即享加速）。

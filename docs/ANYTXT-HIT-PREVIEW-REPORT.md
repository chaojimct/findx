# AnyTXT 命中预览逆向报告（静态分析，2026-11-25）

对象：`C:\Program Files\Anytxt Searcher\`，`ATGUI.exe` 26MB（Qt5 C++，无 WebEngine），
方法：strings 全量提取（`anytxt-atgui-strings.txt`，1,091,092 行）+ DLL 交叉引用 +
只读结构探针。**未启动用户 AnyTXT、无动态行为、无索引写入。**

## 1. 内容检索内核：SQLite FTS5 + 自研扩展

- `fts5: parser stack overflow` / `recursively defined fts5 content table` 等整套
  FTS5 错误串 → 内容倒排就是 SQLite FTS5（经 Wt Dbo 的 SQLite3 后端挂载）。
- 自研扩展（`ats_` 前缀，SQLite 自定义函数/分词器/VFS 注册失败报错串可作证）：
  - `ats_prefix_alnum` / `ats_prefix_unicode`：ASCII 与 Unicode 双前缀分词器
    （CJK 前缀匹配靠后者；对标我方 jieba + 首字母/全拼辅库）。
  - `ats_snippet` / `ats_offsets`：自研摘要与偏移函数（FTS5 原生 `snippet()`/
    `offsets()` 之外的第二套；`wrong number of arguments to ats_snippet()`）。
  - `ats_vfs` / `ats_vfsv2`：自研 VFS（索引文件布局/锁，版本已迭代到 v2）。
- 结论：AnyTXT 内容召回 = FTS5 倒排 + 前缀分词；我方 tantivy 主库 + 拼音辅库
  在架构上对等，无需改道 SQLite。

## 2. 命中预览：HTML 模板 + 网格（证据原文）

预览不是原生控件逐字画的，是一套 `at-preview-*` HTML 模板（嵌入二进制），
渲染宿主是 Wt（`wt.dll` 3.7.0 + `wthttp.dll`，见 §3）：

- 结构：
  `<div class="at-preview-result-grid">`（结果格）→
  `<div class="at-preview-grid-row" data-at-index="%1">`（命中行）→
  `<div class="at-preview-grid-index" data-at-index="%1">%2</div>`（**编号列**）+
  `<div class="at-preview-grid-snippet" data-at-index="%1">%2</div>`（摘要列）。
- 行内高亮：
  `<span class="at-preview-match" data-at-index="%1" style="font-weight:bold;color:%2;">`
  （粗体 + 参数化颜色，`data-at-index` 与行号联动）。
- 分组：`.at-preview-grid-group-header` + `active/expanded` 状态类（按文件分组，
  可折叠；关键字行 `.at-preview-grid-keyword`）。
- 多形态：`.at-preview-tabs .nav-tabs`（同一命中的多种预览形态切换，
  对标我方"命中上下文/系统版式/文字摘录"三 tab）。
- 上一个/下一个：`focusNextPrevGrid(const CAtsScreenGrid*,bool)` +
  `focusNextPrevGridRequest` 信号（Qt slot 链，非 JS；二进制内无预览 JS，
  交互走 C++）。
- 当前命中：`.at-preview-current`（网格与文档双份，联动滚动定位）。
- 触发链：`snippetRequested` → `msgPreviewReady` → `CPreviewPopup`。

## 3. 进程/DLL 分工（交叉引用实测）

- `ATGUI.exe` → `PdfCore.dll`、`wt.dll`、`rpc.dll` + Qt5 全家（无 WebEngine，
  佐证预览走 Wt/富文本而非 Chromium）。
- `ATService.exe` + `ATIHelper.exe` → `rpc.dll`（索引服务侧，IPC 同道）。
- `PdfCore.dll`（12MB）+ `PdfHelper.dll`：PDF 文本与版式。
- `ATDHelper.exe`（ole32/mlang/libxml2）：老 DOC/OLE 文本抽取；
  `ATDHelper2.exe`（7z + PdfHelper + QtXml）：压缩包/PDF/OOXML 新管线；
  `ATEHelper.exe`（QtWidgets/GDI）：表格类抽取与渲染（疑似 Excel）；
  `ATFHelper.exe`（200KB，`query.dll` + ole32）：Windows Search iFilter 垫片；
  `ATIHelper.exe`（7z/PROPSYS/WININET）：元数据/属性抽取。
- `tmc.dll`（319KB）：**tcmalloc 内存分配器**（`PageHeap@tcmalloc` 符号，
  被所有二进制链接），不是功能组件——此前"未知组件"判断作废。
- `wt.dll` 3.7.0（7.6MB）+ `wthttp.dll`：Wt Web 工具包（预览 HTML 渲染/服务）。
- 中文串极少（`文件摘要`等 3 条），主 UI 为英文 + Language 目录语言包。

## 4. FindX 落地方案（差距只剩"编号列表面板"）

| AnyTXT 能力 | FindX 现状 | 缺口 |
|---|---|---|
| 编号命中列表（index 列，可点） | 只有逐窗翻页（上一个/下一个 + k/n） | **需新增：全窗口列表面板** |
| 行内粗体+颜色高亮 | 已有（字面 `<b>` + 拼音对齐标热） | 无 |
| 上一个/下一个 + 位置计数 | 已有（pager k/n） | 无 |
| 按文件分组折叠 | 单文件预览模型，不适用 | 无（形态不同，语义已覆盖） |
| 多形态 tab | 已有（三 tab） | 无 |

实施计划（`ContentSnippets` 侧，预计 1 个后端小改 + 1 个 GUI 面板）：
1. service 新增"取某文档全部摘要窗"（复用 `snippets_for_keys_windowed` 循环，
   上限 50 窗，返回 `Vec<(window, html)>`；IPC 复用现有 `ContentSnippets`，
   entry 单个 + 全量标记，或新增 `ContentSnippetsAll`）。
2. GUI 预览卡下方加"全部命中"列表（序号 badge + 一行摘要，点击 → 现有
   `fetch_snippet_window(entry, k)` 跳窗；当前窗高亮态与 pager 双向同步）。
3. 验收：管道 `ContentSnippets` 全量 == 逐窗遍历拼接；点击列表第 k 项，
   预览窗内容与 pager 跳到 k 完全一致；50 窗以上截断提示。

**状态（2026-11-25）**：1/2 已实现（`ContentSnippetsAll` + `all_windows` + 双处列
表 UI，均有单测与管道验证：全窗 == 逐窗拼接一致）；待用户目检列表交互。

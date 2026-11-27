# 抽取能力升级：依赖、IFilter、构建流水线（2026-11-27）

## 1. 依赖升级

| 库 | 旧 | 新 | 收益 |
|---|---|---|---|
| pdfium-render | 0.8.37 | **0.9.6** | 字体/页对象 double-free 修复、位图切片溢出修复、页树不预加载（大 PDF 更快） |
| pdf-extract | 0.8.2 | **0.12.1** | lopdf 0.42 + CMap/CFF 解析，fallback 的 CJK/加密 PDF 更准 |
| zip | 2.6.1 | **9.8.2** | EOCD/多卷/ZIP64 校验修复，解析不可信 docx/xlsx 更稳 |
| quick-xml | 0.37.5 / 0.32 | **0.42.0** | content/gui 统一；性能与实体处理改进 |
| flate2 | 1.1.10 | 1.1.11 | 补丁 |

兼容性：本机 `pdfium.dll` = 160.0.8179.0（build 8179），高于 pdfium-render 0.9.6
的 `pdfium_latest`（8125），无需换 DLL。

quick-xml 0.42 适配点：`QName::as_ref()` 返回 `&str`；`BytesText::unescape()`
移除（用 `into_inner()` + `quick_xml::escape::unescape`）；`Attribute` 用
`normalized_value(XmlVersion::Implicit1_0)`。

## 2. 老格式：Windows IFilter（.doc/.ppt/.msg/.rtf）

- 走系统已注册 IFilter（Office/WPS/Adobe 安装后注册），**不启动 Word/PowerPoint**。
  与 AnyTXT 的 `ATFHelper.exe` 同构。
- 实现：`findx2-content/src/ifilter.rs`（COM）+ `ifilter_pool.rs`（独立进程池）。
- worker：复用 `pdf-worker --ifilter`（协议同 PDF 文本：路径行 → u64 长度 + UTF-8）。
  2 个常驻 worker，单文件 30s 超时直接 kill 重启，坏文件不拖死构建。
- 安装包/`stage-inno.ps1`/`pack-isolated.ps1` 补发 `pdf-worker.exe`。
- `.msg` 先嗅探：头 4KB 无 NUL 按文本抽（Tcl/系统消息目录），否则走 IFilter（Outlook）。

## 3. 构建流水线（抽取 / 入库解耦）

```
plan ──► 抽取线程池（FINDX_EXTRACT_THREADS，默认 2/4/8 按核数）
           │  DocMeta+text
           ▼
     有界队列（FINDX 队列 64/128/256）
           │
           ▼
     入库线程（tantivy writer 内部多线程）──► commit every 50k
```

- 抽取与写入**重叠**：旧实现每批「抽取完 → 入库完」两次栅栏，IO 与 CPU 互相等。
- 队列有界：抽取快于入库时生产者阻塞（背压），不会把整盘文本堆进内存。
- 抽取线程池独立于 tantivy writer 线程，避免抢核。
- 取消：进度回调返回 false → 置原子标志 + 丢掉接收端，生产者 send 失败退出。

## 4. 验证

（重建后补：耗时、入库数、.doc/.msg 抽取样本、搜索回归）

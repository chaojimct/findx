# findx2-gui-native

FindX 原生 GPUI 客户端（搜索主窗 + 设置壳 + 主题/折叠/诊断 + 真实预览）。

- 不替换现有 Tauri WebView GUI（`gui/`、`gui/src-tauri` 仍保留）。
- **默认隔离运行**：管道名是 `findx2-native-dev`，单独 `--pipe findx2` / `\\.\pipe\findx2` 会被拒绝。
- **`--live`**：作为只读 IPC 客户端连接现有生产管道 `findx2`。不启动、不重启 `findx2-service`，不写 `ProgramData` 索引，也不做 compact/rebuild/install。
- 设置和偏好始终写入 `%LOCALAPPDATA%\FindX-native-dev\`（Unix：`$XDG_DATA_HOME/FindX-native-dev`），不读写正式 GUI 的设置文件。

窗口标题区分模式：`FindX Native (live)` 与 `FindX Native (isolated)`。

## 启动

在仓库根目录：

```text
# 自带模拟后端（不启动、不接触生产 findx2-service）
cargo run -p findx2-gui-native -- --mock

# 只读连接本机已在运行的生产 findx2-service
cargo run -p findx2-gui-native -- --live
```

若要对接一条*隔离的*真实 service（自己准备独立 index 与管道名，切勿使用默认 `findx2`，也不要打开生产 index）：

```text
# 终端 1：隔离后端（示例）
findx2-service --index <独立 index.bin> --pipe findx2-native-dev --no-everything-ipc

# 终端 2
cargo run -p findx2-gui-native -- --pipe findx2-native-dev
```

没有 `--live` 时，生产管道名会被拒绝。本客户端不会 `sc start`、不会 `taskkill findx2-service`、不会改正式安装的索引。

## 快捷键

- 结果列表：ArrowDown / ArrowUp（无选中时 Down 到 0、Up 从末行，与现 GUI 一致）、Home、End
- Enter：打开选中项（搜索框内 Enter 不打开）
- Ctrl+F：聚焦搜索框
- Ctrl+Shift+D：诊断浮层（偏好本地记住）


## Native result table and preview boundaries

The results now use Kit 0.9.1 `DataTable` / `TableState`, not a hand-painted flex list.
All four columns remain available, have minimum widths, share header/body geometry,
and can be resized with boundary handles. Widths are saved only under
`%LOCALAPPDATA%\FindX-native-dev\prefs.json`. Both scrollbars remain visible when
content overflows. Near the bottom the client requests the next 200 hits using
Search `offset`; a query-generation and offset guard rejects stale pages.
The root is flex-constrained to the complete client area; there is no viewport-height
subtraction for native chrome or DPI. The filter rail scrolls independently.

Document previews are read-only and run off the GPUI thread, with selection-generation
stale-drop. No macro engine, shell-open fallback, Office automation, external links,
or document scripts are used.

- PDF (Windows): Windows.Data.Pdf rasterizes page 1, fit inside 1200 x 1600 pixels.
  It is explicitly a **first-page** preview, not a multi-page PDF viewer. Password-
  protected/corrupt files or an unavailable OS engine produce a calm error.
- DOCX/PPTX: bounded OOXML text only, not exact typography, images or embedded objects.
  DOCX reads the document body; PPTX reads up to the first 100 slide parts.
- XLSX: bounded worksheet text with cell addresses, shared strings and cached values.
  It does not calculate formulas, resolve links, render charts, or format date serials.
- RTF: conservative text subset (paragraphs, tabs, Unicode and Windows-1252 escapes).
  Pictures, embedded objects, metadata and unsupported destinations are skipped.
- Legacy binary DOC/XLS/PPT, encrypted OOXML and arbitrary binaries remain unsupported;
  metadata is still available. Existing image, source/text and directory previews remain.

Documents are capped at 32 MiB, XML/RTF parts at 4 MiB, cumulative slide/sheet XML at
16 MiB, and output at 256 KiB. ZIP entries are read in memory, never extracted to disk.

### Evaluation of the old preview mechanism

Reviewed `gui/src-tauri/src/win_preview.rs` and `gui/src/findx/PreviewPane.tsx`.
The old WebView uses IPreviewHandler with a separately owned popup HWND because the
DComp WebView obscures child HWNDs, registered COM handlers (including 32-bit WPS),
STA affinity and unload/recycle logic; it falls back to a browser file-viewer engine.
Blindly importing that global state or loading arbitrary in-process preview-handler DLLs
into GPUI would risk UI-thread stalls, stale/stray windows and crashes. A robust handler
host needs a separate STA/message pump or helper process, cancellation, HWND lifecycle
and per-monitor DPI integration. That work is **not claimed implemented** here.
The implemented PDF engine and data-only Office/RTF parsers do not depend on an installed
Office preview handler and give real errors rather than pretending handlers are available.

The blue `拖拽至此上传` bubble was independently identified as the visible `百度网盘`
Chrome_WidgetWin_1 window owned by BaiduNetdiskUnite.exe (PID 39088 during verification),
not a GPUI Kit element. Its standalone window capture reproduces the exact bubble.
No Baidu process was stopped and no unrelated setting was changed.

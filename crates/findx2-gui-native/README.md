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

Document previews run off the GPUI thread with selection-generation stale-drop.
PDF/OOXML/RTF decoding uses a disposable helper process (10-second timeout); selection
changes cancel and terminate only that request's helper. Corrupt/oversized input fails
calmly. Existing image/text/directory previews remain available.

- PDF (Windows): Windows.Data.Pdf renders the selected page. First/previous/next/last
  controls expose the page count; fit-width and 25%–300% zoom support horizontal and
  vertical panning. Rendering is capped at a 3072-pixel edge. Encrypted/corrupt files
  return an error; this is a raster viewer, not text selection/search/annotations.
- Office: attempts the locally registered IPreviewHandler in a separate COM STA helper.
  A separately owned popup HWND follows the GPUI preview surface, owner movement,
  minimization, resizing and visibility. COM initialization/SetWindow/DoPreview/SetRect/
  Unload stay on that STA. Startup is limited to eight seconds; crashed or unavailable
  handlers do not crash GPUI. The app does not install Office or alter associations.
  Real-layout capability depends on the installed handler and is not claimed for every
  Office feature. On a machine with WPS preview handlers registered, public DOCX, XLSX,
  PPTX and RTF fixtures rendered actual typography, tables, grid/sheet tabs and slides.
  The handler may show its own chrome. Legacy DOC/XLS/PPT remain system-handler-only and
  were not visually accepted in this batch. Without a working handler the UI falls back
  to bounded text (OOXML/RTF) or an explicit unsupported message (legacy binary).
- DOCX/PPTX fallback: bounded OOXML **text excerpt**, not exact typography, images or
  embedded objects. DOCX reads the body; PPTX reads up to 100 slide parts.
- XLSX fallback: worksheet text, cell addresses, shared strings and cached values;
  no formula calculation, links, charts, or date serial formatting.
- RTF fallback: conservative text subset. Pictures and embedded objects are skipped.
- DOC/XLS/PPT: system-handler-only; without a working installed handler the UI explicitly
  says unsupported. There is no invented text/layout fallback for legacy binary formats.
- 系统版式 / 文字摘录 explicitly select native rendering or bounded text fallback. A failed
  native attempt explains the downgrade rather than labeling extracted text full layout.

The app performs no Office automation, macro/script invocation, shell-open preview
fallback, external conversion or upload. Installed native handlers are third-party code
with their own behavior; process isolation is crash/hang containment, not a security
sandbox for hostile documents. Use trusted local documents.

Documents are capped at 32 MiB, XML/RTF parts at 4 MiB, cumulative slide/sheet XML at
16 MiB, and extracted text at 256 KiB. ZIP entries stay in memory, never extracted.
`--preview-file PATH` previews one local file read-only without inserting a fake search
result or requiring indexing; selecting a real search result returns to normal preview.

### Evaluation of the old preview mechanism

Reviewed `gui/src-tauri/src/win_preview.rs` and `gui/src/findx/PreviewPane.tsx`.
The older owned-popup HWND approach is reused, but its global in-process COM state is
not imported: this implementation isolates handler DLLs/COM calls in a disposable STA
helper and keeps GPUI callbacks limited to surface geometry. Native Office helper windows follow owner resize and hide with the preview pane;
switching to text excerpt or another document tears down the previous helper. Cross-monitor
DPI remains dependent on the installed handler.

The blue `拖拽至此上传` bubble was independently identified as the visible `百度网盘`
Chrome_WidgetWin_1 window owned by BaiduNetdiskUnite.exe (PID 39088 during verification),
not a GPUI Kit element. Its standalone window capture reproduces the exact bubble.
No Baidu process was stopped and no unrelated setting was changed.

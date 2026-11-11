# findx2-gui-native

FindX 原生 GPUI 客户端（第一批：搜索主窗 + 设置壳 + 主题/折叠/诊断占位）。

- 不替换现有 Tauri WebView GUI（`gui/`、`gui/src-tauri` 仍保留）。
- **隔离运行**：默认管道名是 `findx2-native-dev`，拒绝连接生产管道 `findx2` / `\\.\pipe\findx2`。
- 设置和偏好写入 `%LOCALAPPDATA%\FindX-native-dev\`（Unix：`$XDG_DATA_HOME/FindX-native-dev`），不读写正式 GUI 的设置文件。

## 启动（隔离）

在仓库根目录：

```text
# 自带模拟后端（不启动、不接触生产 findx2-service）
cargo run -p findx2-gui-native -- --mock
```

若要对接一个**隔离的**真实 service（自己准备独立 index 与管道名，切勿使用默认 `findx2`）：

```text
# 终端 1：隔离后端（示例）
findx2-service --index <独立 index.bin> --pipe findx2-native-dev --no-everything-ipc

# 终端 2
cargo run -p findx2-gui-native -- --pipe findx2-native-dev
```

生产管道名会被拒绝。本客户端不会 `sc start`、不会 `taskkill findx2-service`、不会改正式安装的索引。

## 快捷键

- 结果列表：ArrowDown / ArrowUp（无选中时 Down 从 0、Up 从末行，与现 GUI 一致）、Home、End
- Enter：打开选中项（搜索框内 Enter 不打开）
- Ctrl+F：聚焦搜索框
- Ctrl+Shift+D：诊断浮层（偏好本地记住）
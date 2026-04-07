# FindX — 高性能文件搜索引擎

自研文件搜索引擎，集 Everything 的极速 NTFS 索引与 Listary 的拼音模糊匹配于一体，并完整兼容 Everything SDK v2 IPC，可无缝替代 Everything 供 uTools 等第三方应用调用。

## 架构

```
┌──────────────┐     Named Pipe      ┌──────────────┐
│  FindX.Cli   │ ◄──── JSON-RPC ────► │ FindX.Service│
│   (fx.exe)   │                      │  (FindX.exe) │
└──────────────┘                      └──────┬───────┘
                                             │
┌──────────────┐                      ┌──────┴───────┐
│ FindX.Client │                      │  FindX.Core  │
│  (供第三方)   │                      │ 查询/拼音/AST │
└──────────────┘                      └──────┬───────┘
                                             │ P/Invoke
                              ┌──────────────┴──────────────┐
                              │      findx_engine (Rust)     │
                              │ 紧凑索引·字符串池·哈希FRN映射 │
                              └──────────────┬──────────────┘
                                             │
                              ┌──────────────┴──────────────┐
                              │   FindXNative (C++ / NTFS)  │
                              │   USN Journal · MFT 扫描     │
                              └─────────────────────────────┘
```

| 模块 | 说明 |
|------|------|
| **findx_engine** | Rust `cdylib`，紧凑索引（字符串池 + 扁平记录 + 哈希 FRN 映射 + 排序下标），内存远低于纯托管方案 |
| **FindXNative** | C++ DLL，通过 USN Journal / MFT 批量读取实现极速 NTFS 全量扫描 |
| **FindX.Core** | C# 核心库：拼音匹配、查询编排（词法分析→AST→求值）、持久化、P/Invoke 调用 Rust 引擎 |
| **FindX.Service** | 后台服务进程 `FindX.exe`，Named Pipe IPC + Everything SDK v2 IPC + 系统托盘 |
| **FindX.Client** | 客户端库，供 CLI 及第三方应用消费 |
| **FindX.Cli** | 命令行工具 `fx` |

## 核心特性

- NTFS USN Journal 全量扫描 + 增量实时监控
- 非 NTFS 卷自动回退 ReadDirectoryChangesW
- 拼音混合匹配（全拼、首字母、混合模式，DP 最优路径）
- 文件名 / 拼音首字母排序索引 + 前缀二分（百万级条目低内存）
- 索引二进制持久化，重启后增量更新
- Named Pipe JSON-RPC IPC 协议
- **Everything SDK v2 IPC 完整兼容**（WM_COPYDATA + WM_USER），可直接替代 Everything
- **Everything 搜索语法兼容**（过滤器、布尔运算、通配符、正则等）
- 开机自启 + 系统托盘

## 安装

### 安装包（推荐）

从 [Releases](https://github.com/user/findx/releases) 下载 `FindX-x.x.x-setup.exe`，运行安装向导。

安装选项：
- **FindX 搜索服务** — 后台常驻进程（默认安装）
- **命令行工具 fx** — 可选，安装后自动加入 PATH
- **开机自启动** — 默认启用
- 卸载时自动停止进程、清理注册表和 PATH

> 运行环境：Windows 10+ x64，需安装 [.NET 8 Desktop Runtime](https://dotnet.microsoft.com/download/dotnet/8.0)。

### 便携使用

解压 Release 中的 portable 压缩包，直接运行 `FindX.exe` 即可。

## 使用

### 服务

```powershell
# 启动（系统托盘模式）
FindX.exe

# 无托盘模式（Ctrl+C 停止）
FindX.exe --no-tray
```

### CLI

```powershell
fx search "文件名"
fx search "zhongw" --max 20    # 拼音搜索
fx s "ext:pdf report"          # 过滤器
fx status                      # 查看索引状态
fx reindex                     # 重建索引
```

## 搜索语法

兼容 Everything 搜索语法，支持以下特性：

### 基础搜索

| 语法 | 说明 | 示例 |
|------|------|------|
| `关键词` | 文件名匹配（含拼音模糊） | `readme` |
| `"精确短语"` | 引号内精确匹配 | `"hello world"` |
| `*` / `?` | 通配符（任意字符 / 单个字符） | `*.pdf`、`report_?.doc` |
| `regex:` | 正则表达式匹配 | `regex:^test_\d+` |

### 布尔运算

| 语法 | 说明 | 示例 |
|------|------|------|
| 空格 | AND（所有词同时匹配） | `report 2024` |
| `\|` | OR（匹配任一） | `readme \| changelog` |
| `!` | NOT（排除） | `!temp *.log` |
| `< >` | 分组 | `<readme \| changelog> 2024` |

### 过滤器

| 过滤器 | 说明 | 示例 |
|--------|------|------|
| `file:` | 仅匹配文件 | `file:` |
| `folder:` | 仅匹配文件夹 | `folder:` |
| `ext:` | 扩展名过滤 | `ext:pdf`、`ext:jpg;png` |
| `size:` | 文件大小过滤 | `size:>1mb`、`size:100kb..5mb` |
| `dm:` | 修改日期过滤 | `dm:today`、`dm:>2024-01-01` |
| `path:` | 路径包含 | `path:documents` |
| `nopath:` | 路径不包含 | `nopath:node_modules` |
| `depth:` | 路径深度 | `depth:<=3` |
| `len:` | 文件名长度 | `len:>50` |
| `root:` | 限定根目录 | `root:C:\Users` |
| `attrib:` | 文件属性 | `attrib:H`（隐藏文件） |
| `startwith:` | 文件名前缀 | `startwith:test` |
| `endwith:` | 文件名后缀 | `endwith:_backup` |

### 修饰符

| 修饰符 | 说明 | 示例 |
|--------|------|------|
| `case:` | 区分大小写 | `case: README` |
| `nocase:` | 不区分大小写 | `nocase: readme` |
| `ww:` / `wholeword:` | 全词匹配 | `ww: test` |
| `count:` | 限制结果数量 | `count:10 *.pdf` |

### 组合示例

```
ext:pdf size:>1mb dm:>2024-01-01 report         # 2024年后修改的大于1MB的PDF
folder: path:src depth:<=3                       # src路径下3层以内的文件夹
!ext:exe !ext:dll path:downloads                 # downloads中排除exe和dll
<readme | changelog> ext:md                      # readme或changelog的md文件
case: "TODO" ext:cs path:src                     # 精确大小写匹配TODO
```

## Everything 兼容

FindX 完整实现了 Everything SDK v2 的 IPC 协议，安装后可直接被 uTools、Wox 等调用 Everything 的应用识别和使用：

- **WM_COPYDATA**：Query1 Unicode/ANSI (dwData=2/1)、Query2 Unicode/ANSI (dwData=18/17)
- **WM_USER**：版本查询 (0-5)、数据库状态 (401)、管理状态 (403-411)
- 窗口类名 `EVERYTHING` + `EVERYTHING_TASKBAR_NOTIFICATION` 完整模拟

## IPC 协议

管道名：`\\.\pipe\FindX`，JSON-RPC 风格，每行一个请求/响应：

```json
{"id":1,"method":"search","params":{"query":"zhongw","maxResults":50,"pathFilter":"C:\\Users"}}
{"id":1,"result":{"items":[...],"totalCount":3,"elapsedMs":2}}
```

支持方法：`search`、`status`、`reindex`。

## 从源码构建

### 前置要求

- [.NET 8 SDK](https://dotnet.microsoft.com/download/dotnet/8.0)
- [Rust 工具链](https://rustup.rs/)（cargo）
- MSVC C++ 工具链（可选，用于编译 FindXNative）
- [Inno Setup 6](https://jrsoftware.org/isinfo.php)（可选，用于打包安装程序）

### 快速构建

```powershell
# 完整构建（Rust + .NET + 安装包）
.\build.ps1 -Version 1.0.0

# 跳过安装包
.\build.ps1 -Version 1.0.0 -SkipInstaller

# 跳过 Rust（已编译过）
.\build.ps1 -Version 1.0.0 -SkipRust

# 仅 dotnet build（cargo 自动触发）
dotnet build -c Release
```

### 构建产物

```
publish/
  service/     → FindX.exe + 依赖
  cli/         → fx.exe + 依赖
dist/
  FindX-x.x.x-setup.exe   → 安装包
```

### 运行测试

```powershell
dotnet test src/FindX.Tests
```

## CI/CD

项目使用 GitHub Actions 自动构建和发布。推送 `v*` tag 即触发：

```powershell
git tag v1.0.0
git push origin v1.0.0
```

流水线自动完成：Rust 编译 → .NET 测试 → 发布 → Inno Setup 打包 → 创建 GitHub Release。

也可在 Actions 页面手动触发 `workflow_dispatch`。

## 许可证

MIT

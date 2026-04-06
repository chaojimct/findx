# FindX — 高性能文件搜索引擎

自研文件搜索引擎，集 Everything 的极速 NTFS 索引与 Listary 的拼音模糊匹配于一体。

## 架构

- **FindX.Native** — C++ DLL，通过 USN Journal / MFT 批量读取实现极速 NTFS 全量扫描
- **findx_engine** — Rust `cdylib` 紧凑索引（字符串池 + 扁平记录 + 哈希 FRN 映射 + 排序下标），显著低于纯托管逐文件对象方案的内存
- **FindX.Core** — C# 核心库：拼音匹配与查询编排、持久化、通过 P/Invoke 调用 `findx_engine`
- **FindX.Service** — 后台服务进程，Named Pipe IPC 对外提供搜索能力，系统托盘管理
- **FindX.Client** — 客户端库，供 ClipboardX 及第三方应用消费
- **FindX.Cli** — 命令行工具 `fx`

## 核心特性

- NTFS USN Journal 全量扫描 + 增量实时监控
- 非 NTFS 卷自动回退 ReadDirectoryChangesW
- 拼音混合匹配（全拼、首字母、混合模式，DP 最优路径）
- 文件名 / 拼音首字母排序索引 + 前缀二分（低内存，适合百万级条目）
- 支持正则、模糊匹配、路径过滤、扩展名过滤
- 智能评分排序（匹配类型权重 + 路径深度 + 名称长度）
- 索引二进制持久化，启动时增量更新
- Named Pipe JSON-RPC IPC 协议
- 开机自启 + 系统托盘

## 编译

需安装 **Rust（cargo）** 与 **MSVC 工具链**；`dotnet build` 会先执行 `native/findx-engine` 的 `cargo build` 并复制 `findx_engine.dll`。  
合成压测（不扫盘）：`dotnet run -c Release --project src/FindX.Bench -- 200000`

```bash
# C# 项目（含自动 cargo）
dotnet build
```

**Native DLL（与 ClipboardX `native\ShellNavigate` 同思路：MSBuild + vcxproj）**

- 工程：`native\FindXNative\FindXNative.vcxproj`（源码仍引用 `src\FindX.Native\*.cpp`）
- 双击 `native\FindXNative\build.cmd`，或：

```powershell
cd c:\Users\Mact\dev\tools\findx\native\FindXNative
.\build.ps1
# 仅当 vcxproj 工具集不匹配时：.\build.ps1 -UseCmake
# 缺环境（winget）：.\build.ps1 -InstallBuildTools
```

产出：`native\FindXNative\bin\x64\Release\FindXNative.dll`，并复制到  
`src\FindX.Service\bin\Release\net8.0-windows` 与 `...\Debug\net8.0-windows`。  
`dotnet build` FindX.Service 会从上述 Release 路径自动 `CopyToOutputDirectory`（若文件存在）。

可选：仍可用 `src\FindX.Native\CMakeLists.txt` 手动 CMake。

## 使用

```bash
# 启动服务
FindX.exe

# CLI 搜索
fx search "文件名"
fx search "zhongw" --max 20
fx status
fx reindex
```

## IPC 协议

管道名: `\\.\pipe\FindX`，JSON-RPC 风格，每行一个请求/响应：

```json
{"id":1, "method":"search", "params":{"query":"zhongw","maxResults":50,"pathFilter":"C:\\Users"}}
{"id":1, "result":{"items":[...],"totalCount":3,"elapsedMs":2}}
```

支持 `search`、`status`、`reindex` 三个方法。

# FindX｜全盘秒开 · 拼音即达 · Everything 同款接口，listary的拼音能力

> **当前为 Beta 版本**  
> 功能与性能仍在持续迭代，可能存在缺陷、兼容性问题或边界场景未覆盖。**遇到问题请及时反馈**：[Issues](https://github.com/chaojimct/findx/issues)。你的使用场景与报错信息对改进非常有帮助。

---

## 仓库主页与下载

| 入口 | 地址 | 说明 |
|------|------|------|
| **仓库主页（源码 / README / Issue）** | [github.com/chaojimct/findx](https://github.com/chaojimct/findx) | 浏览代码、文档、提交 Bug、参与讨论 |
| **下载页面（正式构建）** | [github.com/chaojimct/findx/releases](https://github.com/chaojimct/findx/releases) | 每个 **Git 标签**（如 `v1.1.1`）对应一条 Release，内含安装包与便携包 |

**Release 里一般有什么**

- **`FindX-x.x.x-setup.exe`** — Inno Setup 图形安装向导（推荐），可选安装 `fx`、开机自启等。  
- **便携压缩包（portable zip）** — 解压即用 `FindX.exe`，适合不写注册表、不装 PATH 的场景。

构建由 [GitHub Actions](https://github.com/chaojimct/findx/actions) 在推送 `v*` 标签后自动完成；若页面暂无你想要的版本，可在仓库 **Releases** 右侧查看 **Tags** 或等待新 tag 发布。

---

## FindX 是什么

FindX 是一款面向 Windows 的**本地文件名搜索引擎**：基于 NTFS USN / MFT 建立索引，毫秒级响应；同时内置**中文拼音**（全拼、首字母、子串等）与 **Everything 兼容的搜索语法与 IPC**，适合既想要 Everything 级速度、又需要中文模糊搜索、还要对接 uTools 等「Everything 插件」的用户。

---

## 与 Everything 对比

| 维度 | Everything | FindX |
|------|------------|--------|
| **定位** | 经典 NTFS 文件名索引工具，生态成熟 | 自研引擎 + 兼容层，侧重「速度 + 中文 + 可替代 Everything IPC」 |
| **中文 / 拼音** | 以英文文件名与路径为主；中文文件名可搜，**无内置拼音首字母 / 全拼混合模糊**（需依赖文件名本身或第三方习惯） | **内置拼音**：全拼、首字母、混合与子串补充；Rust 侧紧凑索引扫描，大库下仍可控 |
| **Everything 生态** | 官方实现，事实标准 | **Everything SDK v2 IPC 协议兼容**（WM_COPYDATA / WM_USER），可被调用 Everything 的第三方当作 Everything 使用 |
| **搜索语法** | 功能丰富、文档齐全 | **子集兼容**常用过滤器、布尔、通配、正则等（详见 README）；未覆盖项见路线图 |
| **开源与定制** | 闭源免费 | **MIT 开源**，可 fork、审计、二次开发 |
| **成熟度** | 多年生产验证 | **Beta**，行为与边界 case 仍在打磨 |

**一句话**：若你主要用英文路径、且只信官方 Everything，Everything 仍是稳妥之选；若你需要**强中文拼音体验**且希望**尽量不丢 Everything 插件 / IPC 习惯**，FindX 是差异化选项。

---

## 与 Listary 对比

| 维度 | Listary | FindX |
|------|---------|--------|
| **定位** | 全局启动器 + 资源管理器增强 + 搜索，偏「工作流一体」 | 偏 **专用文件名索引 + 搜索窗口 / CLI**，不做完整启动器替代 |
| **文件索引** | 与产品形态深度绑定，体验因版本而异 | **自研 Rust 索引引擎** + USN 增量，目标接近 Everything 类「全盘文件名」体验 |
| **拼音与中文** | 中文用户友好，集成在整体交互里 | **拼音为一级能力**：与排序索引、子串扫描、评分管线打通 |
| **Everything / 第三方协议** | 不主打 Everything IPC 替代 | **明确兼容 Everything SDK v2**，便于与现有工具链共存或迁移 |
| **许可** | 商业软件（有免费版能力边界） | **MIT 开源** |

**一句话**：Listary 强在「随处唤起、与资源管理器一体」的综合效率；FindX 强在 **把「Everything 级索引 + 中文拼音 + Everything 兼容 IPC」绑在一个开源栈里**，适合明确以「文件名搜索 + 插件兼容」为核心的用户。

---

## FindX 的核心优势（汇总）

1. **速度快**：NTFS USN Journal / MFT 批量扫描 + 紧凑内存索引，百万级条目仍可保持较低占用与可接受的查询延迟（具体因机器与索引阶段而异）。  
2. **中文友好**：拼音全拼 / 首字母 / 混合与子串策略结合，适配「只记得读音或片段」的检索习惯。  
3. **兼容 Everything 工作流**：同一套 IPC 与类名约定，降低从 Everything 迁到 FindX 的切换成本。  
4. **语法与过滤器**：Everything 风格查询（AND / OR / NOT、ext / path / size / dm 等）持续对齐，见 README 与路线图。  
5. **开源透明**：代码可审、可改、可集成到自己的工具链。

---

## 反馈与参与

- **Bug / 需求**：[GitHub Issues](https://github.com/chaojimct/findx/issues)  
- **讨论与建议**：欢迎附带系统版本、索引规模、复现步骤与（如有）截图，便于快速定位。

再次说明：**Beta 阶段请对数据与关键操作保留必要备份**；生产环境使用前建议自行评估风险。

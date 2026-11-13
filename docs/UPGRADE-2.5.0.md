# 2.4.10 → 2.5.0：本地候选升级路线（未发布、未安装）

版本依据：GitHub v2.4.10 与本机安装记录/FindX.exe 均为 2.4.10；按 minor +1 准备 2.5.0。
产品版本统一来自 workspace.package.version；各 Rust 包继承，npm/Tauri/Inno 由 scripts/check_release.py 校验。磁盘格式版本与产品版本不是一回事。

## 能复用什么，必须迁移什么
- 保留同一安装 AppId、安装目录、FindX.exe 入口及 FindX2Search 服务名/生产管道 findx2；不要另注册服务。当前 SCM 实际指向开发目录，不能假定已指向 Program Files。
- Windows 默认打包 native，复制为 FindX.exe。只有同目录 FindX.installed 标记且名称为 FindX.exe 时，默认只读连接正式管道；开发原名仍默认隔离。文档预览 worker 必须在单实例分流前进入，仍由同一 exe 运行。
- 快捷方式/现有 HKCU Run 的 FindX 路径可保持；核对参数、工作目录、用户会话权限。任务栏 pin 还依赖 AppUserModelID，不能承诺原 pin 自动等价。
- native 尚无与旧 Tauri 共用的单实例/激活交接；正式切换前需协商退出旧 GUI、保留其状态，不能靠全局 taskkill。当前两个实验 GUI 保持原样。
- 正式设置是 %APPDATA%/tools.findx.gui/findx2-gui-settings.json，字段与 native FindxGuiSettings 基本对齐。按 schema 导入到独立正式 native profile，不覆盖旧 JSON；保留未知字段和旧配置备份。不能把 FindX-native-dev 的实验索引路径/管道覆盖成用户正式设置。
- native 的查询历史/收藏/最近目录/主题在 prefs.json；旧 WebView 的主题、搜索历史及布局部分在 localStorage。需正式导出/导入桥或提示用户选择保留项，不直接解析/改写 WebView 私有数据库。新旧数据同时存在时先展示冲突，不静默覆盖。
- 多标签、选中项、滚动、目录/过滤恢复尚未持久化；不要承诺可恢复。native 更新卡仍为演示状态，不能据其判断真实更新完成。

## 索引与覆盖范围
- 2.4.10 源码已能读取 v5 与 v6 单文件，当前同样支持；分段目录 index.bin.v6 存在时优先于单文件。不能只拷贝 index.bin、仅看最新 mtime，或把一个不匹配的旧 manifest 拼进新段。
- 备份/恢复单位：index.bin、整个 index.bin.v6（全部段和 manifest）、.tri/.tri.pending、.exclude.json、配置及实际 SCM/Run 状态；有其他边车时一并列入清单。冻结写入后计算每个文件 SHA-256、长度、相对名、产品/格式版本、卷标识和 USN 游标。
- 本机 ProgramData 的索引/manifest 内容读取受本地执行权限限制，本批只核查目录元数据与源码，不据此认定正式库覆盖哪些卷。当前实验仅 C 根，绝不能用它直接替代用户旧库/全盘覆盖。
- 逐卷比较 volume identity、根前缀、序列号、journal ID 与 durable USN。只有身份一致、journal 未换且 FirstUsn ≤ durable cursor ≤ NextUsn 才能走增量恢复；否则重建受影响卷并明确告知。不能承诺所有旧库免重建。
- 当前 metadata_ready=false 会阻止周期 checkpoint，正常停机折叠 overlay 的测试不等于断电安全。v6 原位 patch 在旧 manifest 下仍可能可见；Windows 写入还有删除目标再 rename、忽略部分 sync 错误的问题。发布前必须修复并做故障注入证明，不能把“管道在线/搜索有结果”当持久化成功。

## 最小事务切换（待实现/验收，不是本批已执行）
1. 只读 preflight：确认安装版、进程 PID/精确路径、SCM 实际命令、所有卷覆盖、磁盘可用空间、配置来源和权限；下载候选到独立版本目录，验证可信来源、签名/哈希与版本一致。
2. 明示 UAC 和停机影响；用户取消即退出，正式安装、服务、索引和自启动完全不变。未通过兼容/持久化 gate 时不得进入切换。
3. 请求唯一 writer 正常 checkpoint 并停机，等 SCM 真正 STOPPED 和句柄释放；失败就中止，不全局强杀、不启动第二个 watch。记录原来是否 Running，不擅自启动原本 Stopped 的服务。
4. 创建全量一致性备份及哈希 manifest，fsync 后复核；备份采用新目录/create-new，不覆盖历史备份。在副本上验证迁移、两代读取能力和卷覆盖，不触及生产副本。
5. staging → backed_up → index_validated → service_switched → health_passed → committed。每步写事务记录，重试按记录幂等；断电/占用/复制失败保留旧入口与备份，不仅凭 installer exit=0 标记成功。
6. 健康验收必须包括：正确 exe/版本/服务身份、协议能力协商、所有卷加载完且 watch 健康、代表查询、持久化 commit 成功且无错误、停机/重启读回同一 durable generation/游标。当前 status 尚缺 durable generation/last persist error 与完整逐卷验收接口，需补齐。
7. 成功后才切快捷方式/Run/入口并标记 committed；恢复原运行策略。旧安装器目前强杀同名进程、忽略部分服务安装结果且无回滚，不能直接复用作平滑升级器。
8. 失败先停新 writer，再恢复**配套旧二进制+旧索引/边车+旧配置/SCM/Run**；保留失败新库和日志用于排查。不能仅降 exe 版本去写已经迁移的活动库。回退后按旧游标重放，journal 已失效则需要重建。

## Gate 分级和本批证据
P0：部分元数据周期持久化、断电/保存失败传播、安装事务及健康回退、全卷/USN 覆盖、服务路径/协议/单实例。
P1：旧偏好导入及启动状态恢复、深层 parent fallback 性能验收。
P2：全局文件/目录分项接口、native 真实自动更新通道；现阶段只能准确展示总索引项，不冒充分项。

release/2.5.0.json 保持 publish_ready=false。发布工作流在构建/上传前要求所有 required gate 通过；不打 tag、不发布 release。
新增隔离测试使用公开两卷小 fixture，验证 v5 → v6 读回和完整旧备份恢复；另一个测试明确展示旧 manifest 不能回滚原位 prefix patch，**这是已复现的发布阻塞，不是断电安全通过**。
本地候选只供准备/测试，不能因为版本已变为 2.5.0 就覆盖现有安装。最小下一步：先修复并验收持久化及错误状态，再做受控升级事务；无需先重写整个更新器。


Additional drill: installed 2.4.10 CLI and candidate both read the same public v5/v6 copies without changing any hash. The second-volume root has pre-existing redundant backslashes in BOTH versions. Windows path normalization preserves its D:\public.txt identity; exact canonical-path validation remains a P1 gate, not proof of new data loss. An ignored reproduction is retained. Production index contents were not read.

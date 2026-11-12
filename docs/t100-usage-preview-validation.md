# t99/t100 本地扫描预览与来源提示

## 范围与根因

仅修改 native GUI 的本地扫描预览/呈现；没有修改引擎、IPC、服务、索引和历史缓存，没有部署正式服务。
基线 ad794de；上游对照 DiskTree 9b18b272ca3c5a145f6ff58eaf89dc4d311eeaea。
上游 scan_preview 的数据本身也是有限根桶，但 app/treemap_view 把它送入真正的 treemap；本项目旧实现把每桶直接画成等高横排竖条，并没有 progress transition。因而不能把这个缺陷归咎于上游，也不能从静态截图推断动画帧机制。

## 实现

- 保留实际扫描目录 identity/有限祖先层级，按文件类别汇总已测内容。预览的 256 KiB 保守预算只约束视觉摘要，不约束扫描路径数量/时间；超出摘要合并为明确的“其他已测项（预览合并）”，实测字节不会丢失。
- 二维递归、标题带/细间隙/类别颜色；identity-hash 稳定二分拓扑，不因大小排序或新桶到达而重新洗牌。权重变化仍必然改变面积，不能承诺新数据不改变布局。
- 同 identity 从当前实际插值状态续接 360 ms smoothstep；新块从父块进入/淡入，离开淡出。子块按当前父块内容区裁剪，避免旧块穿入邻居。保证位置 C0 连续，不声称速度 C1 连续。
- 每 240 ms 至多重建一次进度布局（几何/钻取变化即时）；动画帧只采样，不每帧重排。点击/悬停使用同一已画 frame。保留选中 identity、缩放、预览钻取/返回；完成或取消向实测树交接。
- 尊重减少动画设置；未知总量不生成百分比/虚构面积。扫描中 Inspector 显示已测字节/计数/总量未知及预览合并状态，不暴露不适用的清理动作。
- 本地扫描按钮明确“本地实测中”/“本地实测”；旧服务不支持明确显示“当前引擎不支持目录占用接口；需更新引擎，或选择本地实测（不会自动更新服务）”，不再前缀“索引可能滞后”。切换来源不会升级服务。
- 活跃扫描绑定实际扫描根，不能让后台进度被另一个地址错误标注。

## 自动与进程验证

最终 locked lib tests：178 passed / 0 failed / 1 ignored；locked 独立 dev build 成功；git diff --check 通过。
三个预览回归覆盖摘要预算与权重守恒、层级二维布局、重定向连续性、240 ms 合并、reduce-motion、scope/hit/完成交接。

可重复隔离脚本：`python scripts/test_usage_preview_fixture.py`。
它在 target/native-usage-t100 下创建自己的公开 fixture / LOCALAPPDATA，缓存沿用现有非 live fixture 隔离分支 `%TEMP%/findx-fixture-cache-<owned PID>`（不是用户历史缓存）。共 3072 文件、393 目录，启动独立 EXE `--mock`，实际运行平台目录扫描器；没有服务 IPC、USN/MFT watcher、全卷枚举或用户输入注入。debug-only command 文件只在 fixture lock 且非 live 生效，用来做 drill/zoom/up/reset/cancel；fixture 人为延迟仅为了稳定复现渐进更新。
三个独立进程均以 WM_CLOSE 正常退出，测试完整扫描、深色取消、减少动画取消。未强杀或激活窗口。

最后构建又完成一轮默认 metrics-only 验证，报告 `target/native-usage-t100/fixture-1794504110/dynamic-report.json`：完整扫描 3465 entries（含目录），enum 40894 ms（含每目录 400 ms 人工延迟），aggregate 1 ms，4 workers/peak 4，partial=false；两个取消均 partial=true。1024 light / 900 dark / 900 reduced 共 98/20/19 次布局，最大布局耗时 1258/439/403 微秒。OS peak working set 99,278,848 / 99,368,960 / 99,176,448 字节；private bytes 最大 149,860,352 / 147,460,096 / 147,365,888。完整 run 403 次 CPU paint，mean 2319 us/max 4328 us（不是 GPU 帧耗时，更不是 60 fps 认证）。fixture 不代表真实全卷性能。

## 视觉证据边界：未通过最终视觉验收

较早版本实屏时间序列已确认真实层级/颜色、导航/缩放/取消，并发现 Inspector 未切到扫描内容、子块过渡溢出邻居，随后已修正。这些旧图不作为最终版本验收证据，最早未初始化画面也不交付。
最终版本三个实际进程均成功扫描/绘制/交接，但 Pillow ImageGrab 实屏提供方抛出 `OSError: screen grab failed`，最终图为零帧，不能把空白 contact sheet 或旧 GIF 当新证据。已尝试同一公开提供方更小的 primary-only owned-window capture，也失败；没有换 PrintWindow/GDI 等替代路径，没有锁屏/解锁/切会话/提权/抢焦点或借 box desktop 操作 MZ。只读诊断 WTS Active + InputDesktop Default 不足以确定失败根因，不能声称已经证实断开或锁屏。

脚本默认只跑进程/指标；`--capture` 是以后在提供方恢复后的显式同一路径取证，首个提供方错误后即停止截图，不自动重试别的机制。生成的 report 将 visual_verified 固定 false，必须人工 Read 实际时间序列后才可验收；无帧时不产生伪 contact/动图。
相关公开依据：
- https://pillow.readthedocs.io/en/stable/reference/ImageGrab.html
- https://github.com/python-pillow/Pillow/issues/4899

## 运行与交付

独立 EXE：`target/native-usage-t100/findx2-gui-native.exe`，没有覆盖 t97 用户 live EXE。
最后只读查询原 PID56408 已不在进程列表；退出原因未知，未关闭/重启/自动恢复它。原服务 PID7552/16628 仍存在且 Responding，未操作部署。

当前交付是可构建、真实进程验证过的代码检查点，不是已完成最终视觉验收的宣称。待实屏提供方可用后，应复验最终 1024/900 宽浅/深色、动画连续性、Inspector 与完成/取消交接。

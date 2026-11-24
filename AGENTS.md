# AGENTS.md — 仓库任务卫生约（2026-11-24 起执行）

C 盘曾被 `target/` 下几十个历史任务 staging 目录塞满（192GB，只剩 7.5GB）。
以后每个任务遵守：

1. **staging 目录 single-owner**：任务级 `CARGO_TARGET_DIR`、证据转储、大体积
   临时目录必须自命名可识别（如 `target/t<编号>-<事由>`），只能建在仓库
   `target/` 下或仓库外，禁止散落到盘根、`C:\Users\chaoj\AppData\Local\Temp`
   之外的地方（Temp 由系统管）。
2. **任务结束必清**：收尾时删掉自己建的 staging 目录（`Remove-Item -Recurse
   -Force`），只保留要进报告的 evidence（小体积，<500MB）。
3. **兜底**：收尾跑一次（仓库根）：
   `powershell -ExecutionPolicy Bypass -File scripts/clean-stale-target-dirs.ps1 -Force`
   无 `-Force` 只列候选不删；脚本自动保护 `target/debug|release|tmp` 和
   `git worktree list` 已注册的 worktree。
4. **红线**：不删生产数据（`C:\ProgramData\FindX`、系统服务）、不删他人已注册
   worktree、不删 `gui/`（未跟踪但有用，见 .gitignore 约定）。

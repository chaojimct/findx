/**
 * 在发布 NSIS 安装包前，将工作区已编译的 findx2-cli 与 findx2-service
 * 复制到 src-tauri/bundled/，由 tauri bundle.resources 打入安装包。
 * 非 Windows 平台在 CI/交叉构建中跳过（仅打 GUI）。
 */
import { copyFileSync, existsSync, mkdirSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { execSync } from "node:child_process";

const __dirname = dirname(fileURLToPath(import.meta.url));
const guiRoot = join(__dirname, "..");
const workspaceRoot = join(guiRoot, "..");
const outDir = join(guiRoot, "src-tauri", "bundled");
const target = join(workspaceRoot, "target", "release");

if (process.platform !== "win32") {
  console.log("[bundle-win-exes] 非 Windows，跳过（不打包 CLI/服务）。");
  process.exit(0);
}

const bins = ["findx2.exe", "fx.exe", "findx2-service.exe"];
mkdirSync(outDir, { recursive: true });
console.log(
  "[bundle-win-exes] 正在 cargo build -p findx2-cli -p findx2-service --release ...",
);
execSync("cargo build -p findx2-cli -p findx2-service --release", {
  cwd: workspaceRoot,
  stdio: "inherit",
  env: process.env,
});
for (const f of bins) {
  const src = join(target, f);
  if (!existsSync(src)) {
    throw new Error(
      `[bundle-win-exes] 未找到 ${src}，请确认已在本工作区用 release 成功构建 CLI/服务。`,
    );
  }
  const dest = join(outDir, f);
  copyFileSync(src, dest);
  console.log(`[bundle-win-exes] 已复制 ${f}`);
}

/**
 * 将本机 release 的 CLI / 服务复制到 src-tauri/bundled/，供 Tauri resources 打进安装包。
 * Windows：findx2.exe / fx.exe / findx2-service.exe
 * Unix：findx2 / fx / findx2-service（无后缀）
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
const isWin = process.platform === "win32";
const bins = isWin
  ? ["findx2.exe", "fx.exe", "findx2-service.exe"]
  : ["findx2", "fx", "findx2-service"];

mkdirSync(outDir, { recursive: true });
console.log(
  `[bundle-native-bins] 正在 cargo build -p findx2-cli -p findx2-service --release (${process.platform}) ...`,
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
      `[bundle-native-bins] 未找到 ${src}，请确认已在本工作区用 release 成功构建 CLI/服务。`,
    );
  }
  copyFileSync(src, join(outDir, f));
  console.log(`[bundle-native-bins] 已复制 ${f}`);
}

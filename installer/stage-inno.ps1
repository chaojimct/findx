# 将 tauri build --no-bundle 的 target/release 产物整理到 installer/stage，供 iscc 打包。
# 在仓库根目录或传入 -RepoRoot 后执行。失败时给出明确错误（缺 FindX.exe 等）。
param(
  [string] $RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
)

$ErrorActionPreference = "Stop"
$rel = Join-Path $RepoRoot "target\release"
$stage = Join-Path $RepoRoot "installer\stage"
$mainFindX = Join-Path $rel "FindX.exe"
$mainGui = Join-Path $rel "findx2-gui.exe"
$main = $null
if (Test-Path $mainFindX) { $main = $mainFindX }
elseif (Test-Path $mainGui) { $main = $mainGui }
if (-not $main) {
  throw "未找到 $mainFindX 或 $mainGui。请先在 gui 目录执行: npm run tauri build -- --no-bundle"
}
$res = Join-Path $rel "resources"
if (-not (Test-Path $res)) {
  throw "未找到 $res。Tauri 未产出 resources 目录，无法制 Inno 包。"
}
Remove-Item $stage -Recurse -Force -ErrorAction SilentlyContinue
$null = New-Item -ItemType Directory -Path $stage
# 安装包与 [Icons] 均要求 {app}\FindX.exe 固定名
Copy-Item -Path $main -Destination (Join-Path $stage "FindX.exe") -Force
Copy-Item -Path $res -Destination (Join-Path $stage "resources") -Recurse
Get-ChildItem -Path $rel -Filter *.dll -File -ErrorAction SilentlyContinue | ForEach-Object {
  Copy-Item -Path $_.FullName -Destination $stage
}
foreach ($f in @("findx2.exe", "fx.exe", "findx2-service.exe")) {
  $p = Join-Path $rel $f
  if (-not (Test-Path $p)) {
    throw "未找到 $p。请确认 beforeBuild 已执行 bundle:win-exes 并成功 cargo build -p findx2-cli -p findx2-service --release。"
  }
  Copy-Item -Path $p -Destination $stage
}
$n = (Get-ChildItem -Path $stage -Recurse -File -ErrorAction SilentlyContinue | Measure-Object).Count
if ($n -lt 1) {
  throw "Inno 源目录 $stage 内无任何文件，ISCC 会报 stage\* 无匹配。"
}
Write-Host "已写入 Inno 源目录: $stage （$n 个文件）"

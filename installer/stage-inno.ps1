# 将 tauri build --no-bundle 的 target/release 产物整理到 installer/stage，供 iscc 打包。
# 在仓库根目录或传入 -RepoRoot 后执行。失败时给出明确错误（缺 FindX.exe 等）。
param(
  [string] $RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
)

$ErrorActionPreference = "Stop"
$stage = Join-Path $RepoRoot "installer\stage"
$releaseCandidates = @(
  (Join-Path $RepoRoot "target\release"),
  (Join-Path $RepoRoot "target\x86_64-pc-windows-msvc\release"),
  (Join-Path $RepoRoot "gui\target\release")
)
$rel = $null
foreach ($c in $releaseCandidates) {
  $fx = Join-Path $c "FindX.exe"
  $gui = Join-Path $c "findx2-gui.exe"
  if ((Test-Path $fx) -or (Test-Path $gui)) {
    $rel = $c
    Write-Host "[stage-inno] 使用 release 目录: $rel"
    break
  }
}
if (-not $rel) {
  Write-Host "[stage-inno] 下列目录均未发现 FindX.exe / findx2-gui.exe："
  $releaseCandidates | ForEach-Object { Write-Host "  $_" }
  $t = Join-Path $RepoRoot "target"
  if (Test-Path $t) {
    Write-Host "[stage-inno] $($t) 下子项："
    Get-ChildItem $t -ErrorAction SilentlyContinue | ForEach-Object { Write-Host "  $($_.Name)" }
  }
  throw "未找到 GUI release 产物。请先在 gui 目录执行: npm run tauri build -- --no-bundle"
}
$mainFindX = Join-Path $rel "FindX.exe"
$mainGui = Join-Path $rel "findx2-gui.exe"
if (Test-Path $mainFindX) { $main = $mainFindX }
else { $main = $mainGui }
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
  throw "Inno 源目录 $stage 内无任何文件，ISCC 无法收集安装文件。"
}
Write-Host "已写入 Inno 源目录: $stage （$n 个文件）"

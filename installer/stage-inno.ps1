# 将 tauri build --no-bundle 的 target/release 产物整理到 installer/stage，供 iscc 打包。
# 在仓库根目录或传入 -RepoRoot 后执行。失败时给出明确错误（缺 FindX.exe 等）。
# 可选 -ReleaseDir：指定含 FindX.exe/findx2-gui.exe 的 release 目录（优先于默认 target\release），
# 用于本机 target\release 下 exe 被服务/杀软占用无法覆盖时，配合 CARGO_TARGET_DIR 另目录构建。
param(
  [string] $RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path,
  [string] $ReleaseDir = ""
)

$ErrorActionPreference = "Stop"
$stage = Join-Path $RepoRoot "installer\stage"
$releaseCandidates = @(
  (Join-Path $RepoRoot "target\release"),
  (Join-Path $RepoRoot "target\x86_64-pc-windows-msvc\release"),
  (Join-Path $RepoRoot "gui\target\release")
)
if ($ReleaseDir) {
  if (-not (Test-Path $ReleaseDir)) { throw "-ReleaseDir path not found: $ReleaseDir" }
  $rd = (Resolve-Path $ReleaseDir).Path
  $releaseCandidates = @($rd) + $releaseCandidates
  Write-Host "[stage-inno] prefer -ReleaseDir: $rd"
}
$rel = $null
foreach ($c in $releaseCandidates) {
  $fx = Join-Path $c "FindX.exe"
  $gui = Join-Path $c "findx2-gui.exe"
  if ((Test-Path $fx) -or (Test-Path $gui)) {
    $rel = $c
    Write-Host "[stage-inno] using release dir: $rel"
    break
  }
}
if (-not $rel) {
  Write-Host "[stage-inno] no FindX.exe / findx2-gui.exe in:"
  $releaseCandidates | ForEach-Object { Write-Host "  $_" }
  $t = Join-Path $RepoRoot "target"
  if (Test-Path $t) {
    Write-Host "[stage-inno] children of $t :"
    Get-ChildItem $t -ErrorAction SilentlyContinue | ForEach-Object { Write-Host "  $($_.Name)" }
  }
  throw "GUI release build not found. Run from gui: npm run tauri build -- --no-bundle"
}
$mainFindX = Join-Path $rel "FindX.exe"
$mainGui = Join-Path $rel "findx2-gui.exe"
if (Test-Path $mainFindX) { $main = $mainFindX }
else { $main = $mainGui }
$res = Join-Path $rel "resources"
if (-not (Test-Path $res)) {
  throw "resources folder missing: $res"
}
Remove-Item $stage -Recurse -Force -ErrorAction SilentlyContinue
$null = New-Item -ItemType Directory -Path $stage
# Installer expects {app}\FindX.exe
Copy-Item -Path $main -Destination (Join-Path $stage "FindX.exe") -Force
Copy-Item -Path $res -Destination (Join-Path $stage "resources") -Recurse
Get-ChildItem -Path $rel -Filter *.dll -File -ErrorAction SilentlyContinue | ForEach-Object {
  Copy-Item -Path $_.FullName -Destination $stage
}
foreach ($f in @("findx2.exe", "fx.exe", "findx2-service.exe")) {
  $p = Join-Path $rel $f
  if (-not (Test-Path $p)) {
    throw "missing $p ; ensure bundle:win-exes and cargo build -p findx2-cli -p findx2-service --release"
  }
  Copy-Item -Path $p -Destination $stage
}
$n = (Get-ChildItem -Path $stage -Recurse -File -ErrorAction SilentlyContinue | Measure-Object).Count
if ($n -lt 1) {
  throw "Stage has no files: $stage"
}
Write-Host "[stage-inno] done: $stage file-count=$n"

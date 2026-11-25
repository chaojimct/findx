<#
.SYNOPSIS
  生产 rollout：拼音全文能力上线（service + CLI + pdf-worker；可选 GUI）。
  以管理员身份运行（右键 → 以管理员身份运行）。
.DESCRIPTION
  前置（已完成）：release 构建（含 P0-G/P0-P2/辅库/拼音）、生产 content 主库
  （C:\ProgramData\FindX\index.bin.content，92522 篇）+ 拼音辅库（25689 篇）。
  本脚本：停 FindX2Search → 换二进制 → 起服务 → 管道验证 ready + content 挂载。
  GUI（FindX.exe）运行时锁定：默认不碰；关掉 GUI 后加 -IncludeGui 再跑一次。
.EXAMPLE
  powershell -ExecutionPolicy Bypass -File prod-rollout-pinyin.ps1
  powershell -ExecutionPolicy Bypass -File prod-rollout-pinyin.ps1 -IncludeGui
#>
param([switch]$IncludeGui)

$ErrorActionPreference = 'Stop'
function Fail($m) { Write-Host "FAIL: $m" -ForegroundColor Red; exit 1 }

$isAdmin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
if (-not $isAdmin) { Fail '请以管理员身份运行（右键 → 以管理员身份运行）' }

$prod = 'C:\Program Files\FindX'
$bak = 'C:\ProgramData\FindX\backup-2026-11-25-pinyin'
$rel = 'C:\Users\chaoj\dev\tools\findx\target\release'
$pipe = 'findx2'

function PipeJson($obj, $timeoutMs) {
  $c = New-Object System.IO.Pipes.NamedPipeClientStream('.', $pipe, [System.IO.Pipes.PipeDirection]::InOut)
  $c.Connect($timeoutMs)
  $w = New-Object System.IO.StreamWriter($c); $w.AutoFlush = $true
  $r = New-Object System.IO.StreamReader($c)
  $w.WriteLine(($obj | ConvertTo-Json -Compress -Depth 6))
  $line = $r.ReadLine(); $c.Close()
  return ($line | ConvertFrom-Json)
}

# 0) 版本确认：release 二进制必须含辅库协议（防拿错旧构建）。
$bytes = [IO.File]::ReadAllBytes("$rel\findx2-service.exe")
$txt = [Text.Encoding]::ASCII.GetString($bytes)
if ($txt -notmatch 'pinyin-initials' -or $txt -notmatch 'content_snippets_all') {
  Fail 'release service 不含拼音辅库协议，请重新 cargo build --release'
}
Write-Host 'OK: release service 含拼音辅库协议'

# 1) 停服务。
$s = Get-Service -Name 'FindX2Search' -ErrorAction SilentlyContinue
if (-not $s) { Fail '找不到服务 FindX2Search' }
if ($s.Status -ne 'Stopped') {
  Stop-Service -Name 'FindX2Search' -Force
  $s.WaitForStatus('Stopped', '00:01:00')
}
Write-Host 'OK: 服务已停'

# 2) 备份校验（此前已备一份；这里只校验存在）。
foreach ($f in @('findx2-service.exe', 'FindX.exe', 'findx2.exe', 'fx.exe')) {
  if (-not (Test-Path "$bak\$f")) { Fail "备份缺失: $bak\$f" }
}
Write-Host 'OK: 备份存在'

# 3) 换二进制。
Copy-Item "$rel\findx2-service.exe" "$prod\findx2-service.exe" -Force
Copy-Item "$rel\findx2.exe" "$prod\findx2.exe" -Force
Copy-Item "$rel\findx2.exe" "$prod\fx.exe" -Force
Copy-Item "$rel\pdf-worker.exe" "$prod\pdf-worker.exe" -Force
Write-Host 'OK: service/CLI/pdf-worker 已换'

if ($IncludeGui) {
  $g = Get-Process -Name 'FindX' -ErrorAction SilentlyContinue
  if ($g) { Fail '检测到 FindX.exe 仍在运行：请先关闭正式 GUI 再加 -IncludeGui 重跑' }
  Copy-Item "$rel\findx2-gui-native.exe" "$prod\FindX.exe" -Force
  Write-Host 'OK: GUI 已换（下次启动生效）'
} else {
  $g = Get-Process -Name 'FindX' -ErrorAction SilentlyContinue
  if ($g) { Write-Host 'SKIP: GUI 正在运行，未动 FindX.exe（关闭后再加 -IncludeGui 跑一次）' -ForegroundColor Yellow }
  else {
    Copy-Item "$rel\findx2-gui-native.exe" "$prod\FindX.exe" -Force
    Write-Host 'OK: GUI 未运行，顺手一起换了'
  }
}

# 4) 起服务。
Start-Service -Name 'FindX2Search'
Start-Sleep 20

# 5) 管道验证：ready + 内容库挂载 + 辅库。
$st = PipeJson @{type = 'status'} 30000
echo ("status: state=" + $st.content_state + " docs=" + $st.content_docs + " aux_ini=" + $st.content_aux_ini_docs + " aux_full=" + $st.content_aux_full_docs)
if ($st.content_state -ne 'ready') { Fail ('content 未就绪: ' + $st.content_state + ' / ' + $st.content_detail) }
if ([int]$st.content_aux_ini_docs -le 0 -or [int]$st.content_aux_full_docs -le 0) { Fail '拼音辅库未挂载' }
$q = PipeJson @{type = 'search'; query = 'content:quanwen'; pinyin = $true; pinyin_initials = $true; pinyin_full = $true; limit = 5; offset = 0} 120000
echo ("smoke content:quanwen total=" + $q.total + " ph_rows=" + @($q.hits | Where-Object { $_.pinyin_hit }).Count)
if ([int]$q.total -le 0) { Fail '冒烟查询零命中' }
Write-Host 'ROLLUP OK：生产拼音全文已上线（P0-H 白天重启验证一并完成：计数见 status，增量跟随看后续轮询）' -ForegroundColor Green

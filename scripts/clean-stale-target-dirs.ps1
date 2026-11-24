# clean-stale-target-dirs.ps1 : clean stale task staging dirs under target/.
# Keeps target/debug, target/release, target/tmp and git-registered worktrees.
# Keeps small evidence dirs (below 500MB).
# Usage from repo root:
#   powershell -ExecutionPolicy Bypass -File scripts/clean-stale-target-dirs.ps1        # list only
#   powershell -ExecutionPolicy Bypass -File scripts/clean-stale-target-dirs.ps1 -Force # delete
param([switch]$Force)

$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot
$target = Join-Path $repo 'target'
if (-not (Test-Path $target)) { Write-Host "no target dir: $target"; exit 0 }

$protected = @('debug', 'release', 'tmp')
try {
  $wt = git -C $repo worktree list --porcelain 2>$null
  foreach ($line in $wt) {
    if ($line -match '^worktree (.+)$') {
      $p = $Matches[1].Trim()
      if ($p.StartsWith($target, [StringComparison]::OrdinalIgnoreCase)) {
        $protected += Split-Path -Leaf $p
      }
    }
  }
} catch { Write-Host 'git worktree list failed, only built-in protected names apply' }

$thresholdMB = 500
$cands = @()
foreach ($d in Get-ChildItem $target -Directory -Force) {
  if ($protected -contains $d.Name) { continue }
  $sum = (Get-ChildItem $d.FullName -Recurse -ErrorAction SilentlyContinue |
    Measure-Object Length -Sum).Sum
  $mb = [math]::Round($sum / 1MB, 1)
  if ($mb -ge $thresholdMB) { $cands += [pscustomobject]@{ Name = $d.Name; MB = $mb } }
}

if ($cands.Count -eq 0) { Write-Host 'no stale staging dirs'; exit 0 }
$cands | Sort-Object MB -Descending | Format-Table -AutoSize | Out-String | Write-Host

if (-not $Force) {
  Write-Host 'dry-run only, re-run with -Force to delete'
  exit 0
}
foreach ($c in $cands) {
  Remove-Item -Recurse -Force (Join-Path $target $c.Name)
  Write-Host "deleted $($c.Name) ($($c.MB)MB)"
}
$free = (Get-PSDrive C).Free
Write-Host ("C: free now {0:N1}GB" -f ($free / 1GB))

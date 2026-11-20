# ASCII-only on purpose. Windows PowerShell 5.1 parses this file with or without a BOM.
# It is the regression for ACP936: the Chinese handoff scripts must carry a UTF-8 BOM
# and must parse on the Windows PowerShell that is executing this file.
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$files = @(
  'installer\upgrade-transaction.ps1',
  'scripts\test-installer-handoff.ps1',
  'scripts\test-installer-handoff-ci.ps1'
)
foreach ($rel in $files) {
  $path = Join-Path $root $rel
  $bytes = [IO.File]::ReadAllBytes($path)
  if ($bytes.Length -lt 3 -or $bytes[0] -ne 0xEF -or $bytes[1] -ne 0xBB -or $bytes[2] -ne 0xBF) {
    throw ('UTF-8 BOM required for Windows PowerShell 5.1: ' + $rel)
  }
  $tokens = $null
  $errors = $null
  $null = [Management.Automation.Language.Parser]::ParseFile($path, [ref]$tokens, [ref]$errors)
  if ($errors.Count) { throw ($rel + ' does not parse on this Windows PowerShell: ' + ($errors | Out-String)) }
}
& (Join-Path $PSScriptRoot 'test-installer-handoff.ps1')
Write-Output ('ps51-handoff-encoding-ok ' + $PSVersionTable.PSVersion.ToString())

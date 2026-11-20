# Windows PowerShell 5.1 reads a script without a UTF-8 BOM as the ANSI code page.
# The 2.5.8 candidate's receipt module had no BOM. On zh-CN (CP936) the bytes of
# U+3002 "。" consumed the closing quote, so Import-Module died at
# transaction-receipt.psm1:88:77 UnexpectedToken published.json
# before the transaction directory existed. This file is that regression.
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$installer = Join-Path $root 'installer'
$iss = Get-Content -LiteralPath (Join-Path $installer 'FindX.iss') -Raw -Encoding UTF8

function Assert-True([bool]$Condition, [string]$Message) {
    if (-not $Condition) { throw $Message }
}

foreach ($name in @('stop-mode.ps1', 'transaction-files.psm1', 'transaction-platform.psm1', 'transaction-receipt.psm1', 'upgrade-transaction.ps1')) {
    Assert-True ($iss.Contains('Source: "' + $name + '"')) "FindX.iss does not package $name"
    Assert-True ($iss.Contains("ExtractTemporaryFile('$name')")) "FindX.iss does not extract $name"
}

foreach ($name in @('transaction-files.psm1', 'transaction-platform.psm1', 'transaction-receipt.psm1', 'upgrade-transaction.ps1')) {
    $bytes = [IO.File]::ReadAllBytes((Join-Path $installer $name))
    $nonAscii = $false
    foreach ($b in $bytes) { if ($b -ge 128) { $nonAscii = $true; break } }
    Assert-True $nonAscii "$name unexpectedly lost its non-ASCII strings"
    Assert-True ($bytes.Length -ge 3 -and $bytes[0] -eq 0xEF -and $bytes[1] -eq 0xBB -and $bytes[2] -eq 0xBF) "$name has non-ASCII text and no UTF-8 BOM; Windows PowerShell 5.1 will decode it as ANSI"
}

$receiptPath = Join-Path $installer 'transaction-receipt.psm1'
$receiptBytes = [IO.File]::ReadAllBytes($receiptPath)
$bare = $receiptBytes[3..($receiptBytes.Length - 1)]
$cp936 = [Text.Encoding]::GetEncoding(936)
$decoded = $cp936.GetString($bare)
$sim = Join-Path ([IO.Path]::GetTempPath()) ('findx-receipt-cp936-' + [guid]::NewGuid().ToString('N') + '.psm1')
[IO.File]::WriteAllText($sim, $decoded, (New-Object Text.UTF8Encoding $true))
$simErrors = $null
$null = [System.Management.Automation.Language.Parser]::ParseFile($sim, [ref]$null, [ref]$simErrors)
Remove-Item -LiteralPath $sim -Force
$first = $simErrors | Select-Object -First 1
Assert-True ($null -ne $first) 'CP936 decode of the no-BOM receipt no longer reproduces the 5.1 parse break'
Assert-True ($first.Extent.StartLineNumber -eq 88 -and $first.Extent.StartColumnNumber -eq 77 -and $first.ErrorId -eq 'UnexpectedToken' -and $first.Message.Contains('published.json')) "CP936 simulation is no longer the 22:53 error: $($first.Extent.StartLineNumber):$($first.Extent.StartColumnNumber) $($first.ErrorId) $($first.Message)"

$realErrors = $null
$null = [System.Management.Automation.Language.Parser]::ParseFile($receiptPath, [ref]$null, [ref]$realErrors)
Assert-True ($null -eq $realErrors -or @($realErrors).Count -eq 0) 'BOM receipt still has parse errors'

$importDir = Join-Path ([IO.Path]::GetTempPath()) ('findx-bootstrap-import-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $importDir | Out-Null
try {
    Copy-Item -LiteralPath (Join-Path $installer 'stop-mode.ps1') -Destination $importDir
    Copy-Item -LiteralPath $receiptPath -Destination $importDir
    Import-Module (Join-Path $importDir 'transaction-receipt.psm1') -Force -ErrorAction Stop
    Assert-True ($null -ne (Get-Command Test-FxTxServiceVersionProbe -ErrorAction SilentlyContinue)) 'Receipt module imported without the packaged stop-mode.ps1'
} finally {
    Remove-Module transaction-receipt -Force -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $importDir -Recurse -Force -ErrorAction SilentlyContinue
}

$work = Join-Path ([IO.Path]::GetTempPath()) ('findx-bootstrap-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $work | Out-Null
try {
    foreach ($name in @('stop-mode.ps1', 'transaction-files.psm1', 'transaction-platform.psm1', 'transaction-receipt.psm1', 'upgrade-transaction.ps1')) {
        Copy-Item -LiteralPath (Join-Path $installer $name) -Destination (Join-Path $work $name)
    }
    $driver = Get-Content -LiteralPath (Join-Path $work 'upgrade-transaction.ps1') -Raw
    $driver = $driver.Replace('Add-Type -AssemblyName System.ServiceProcess', '$null = ''service-process-present''')
    $driver = $driver.Replace("Import-Module (Join-Path `$PSScriptRoot 'transaction-files.psm1') -Force -Global", '$null = ''files-present''')
    $driver = $driver.Replace("Import-Module (Join-Path `$PSScriptRoot 'transaction-platform.psm1') -Force -Global", '$null = ''platform-present''')
    Assert-True ($driver.Contains('transaction-receipt.psm1') -and $driver.Contains('service-process-present')) 'Bootstrap test copy no longer calls the receipt import'
    [IO.File]::WriteAllText((Join-Path $work 'upgrade-transaction.ps1'), $driver, (New-Object Text.UTF8Encoding $true))
    [IO.File]::WriteAllText((Join-Path $work 'transaction-receipt.psm1'), "throw 'bootstrap-marker-receipt'`r`n", (New-Object Text.UTF8Encoding $true))
    $tx = Join-Path $work 'tx'
    $app = Join-Path $work 'app'
    $hostExe = (Get-Process -Id $PID).Path
    & $hostExe -NoProfile -NonInteractive -File (Join-Path $work 'upgrade-transaction.ps1') -Phase Prepare -TransactionRoot $tx -InstallRoot $app -DataRoot $app -NativeConfigRoot $app -LegacyConfigRoot $app -ExpectedVersion '2.5.8' -ServiceStopSeconds 180
    $exit = $LASTEXITCODE
    Assert-True ($exit -ne 0) 'Broken receipt import returned success'
    Assert-True (Test-Path -LiteralPath (Join-Path $tx 'failure.txt')) 'Bootstrap failure did not create failure.txt'
    Assert-True (Test-Path -LiteralPath (Join-Path $tx 'failure.detail.txt')) 'Bootstrap failure did not create failure.detail.txt'
    Assert-True ((Get-Content -LiteralPath (Join-Path $tx 'reason.code') -Raw).Trim() -eq 'bootstrap') 'reason.code is not bootstrap'
    $failure = Get-Content -LiteralPath (Join-Path $tx 'failure.txt') -Raw
    Assert-True ($failure.Contains('bootstrap-marker-receipt')) "failure.txt lost the original receipt exception: $failure"
    Assert-True (-not (Test-Path -LiteralPath (Join-Path $tx 'journal.json'))) 'Bootstrap failure wrote transaction state'
} finally {
    Remove-Item -LiteralPath $work -Recurse -Force -ErrorAction SilentlyContinue
}

Write-Output 'installer bootstrap encoding and early failure log passed'

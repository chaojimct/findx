# Decision table for the installer stop gate. No SCM and no service process.
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
. (Join-Path (Join-Path $PSScriptRoot '..') (Join-Path 'installer' 'stop-mode.ps1'))

function Assert-Mode([string]$Name, $Actual, [string]$Expected) {
 if ($Actual -cne $Expected) { throw ($Name + ' => ' + $Actual + ' wanted ' + $Expected) }
}

$probe = Test-FxTxServiceVersionProbe "findx2-service 2.5.5`n" 0
if (-not $probe) { throw '2.5.5 version line must pass the argument probe' }
if (-not (Test-FxTxServiceVersionProbe 'findx2-service 2.5.7' 0)) { throw 'probe must accept the image version, not the installer version' }
if (Test-FxTxServiceVersionProbe 'findx2-service 2.5.5' 1) { throw 'nonzero probe exit is not a service image' }
if (Test-FxTxServiceVersionProbe 'not-a-service 2.5.5' 0) { throw 'foreign image must fail the probe' }
if (Test-FxTxServiceVersionProbe 'findx2-service 2.5.5 extra' 0) { throw 'probe line must be exact' }
if (Test-FxTxServiceVersionProbe '' 0) { throw 'empty probe must fail' }

$protocol2 = [pscustomobject]@{
 type = 'upgrade_capabilities_result'
 protocol_version = 2
 durable_prepare = $true
 runtime_error = $null
 last_checkpoint_error = 'historical checkpoint IO'
 reason = 'authenticated protocol2 prepare/commit supported'
}
foreach ($installer in @('2.5.3', '2.5.5', '2.5.7')) {
 Assert-Mode ('installer ' + $installer) (Get-FxTxPreStopMode $true $protocol2 '' $installer) 'protocol2-attempt'
}
Assert-Mode 'probe fail ignores capability' (Get-FxTxPreStopMode $false $protocol2 '' '2.5.7') 'accepted-partial'

$source = [pscustomobject]@{
 type = 'upgrade_capabilities_result'
 protocol_version = 2
 durable_prepare = $true
 runtime_error = 'stat: 680 unresolved'
 last_checkpoint_error = $null
 reason = 'authenticated protocol2 prepare/commit supported'
}
Assert-Mode 'runtime error' (Get-FxTxPreStopMode $true $source '' '2.5.7') 'accepted-partial'

$legacy = [pscustomobject]@{
 type = 'upgrade_capabilities_result'
 protocol_version = 1
 durable_prepare = $false
 runtime_error = $null
 reason = 'legacy'
}
Assert-Mode 'protocol1' (Get-FxTxPreStopMode $true $legacy '' '2.5.7') 'accepted-partial'
Assert-Mode 'error reply' (Get-FxTxPreStopMode $true ([pscustomobject]@{ type = 'error'; message = 'unsupported management protocol' }) '' '2.5.7') 'accepted-partial'
Assert-Mode 'transport' (Get-FxTxPreStopMode $true $null 'Bounded management response timeout; state may remain sealed, no replacement' '2.5.7') 'accepted-partial'
Assert-Mode 'identity' (Get-FxTxPreStopMode $true $null 'Service image/PID/creation identity changed; no STOP' '2.5.7') 'refuse-identity'
$malformed = [pscustomobject]@{ type = 'upgrade_capabilities_result'; protocol_version = 2; durable_prepare = 'yes'; reason = 'x' }
Assert-Mode 'malformed' (Get-FxTxPreStopMode $true $malformed '' '2.5.7') 'refuse-malformed'

Assert-Mode 'metadata' (Get-FxTxPrepareOutcome 'Real prepare refused: {"message":"metadata not ready; preparation aborted"}' $false) 'accepted-partial'
Assert-Mode 'watch' (Get-FxTxPrepareOutcome 'native watch/backfill unhealthy; no prepare' $false) 'accepted-partial'
Assert-Mode 'service timeout' (Get-FxTxPrepareOutcome 'prepare timeout; RAM retained, no receipt' $false) 'accepted-partial'
Assert-Mode 'client timeout' (Get-FxTxPrepareOutcome 'Bounded management response timeout; state may remain sealed, no replacement' $false) 'refuse-sealed'
Assert-Mode 'wrong index' (Get-FxTxPrepareOutcome 'wrong index or timeout bound' $false) 'refuse-contract'
Assert-Mode 'token' (Get-FxTxPrepareOutcome 'metadata not ready' $true) 'protocol2-commit'
Assert-Mode 'clean prepare' (Get-FxTxPrepareOutcome '' $false) 'protocol2-commit'

$partial = Test-FxTxAcceptedStopExit 1066 1
if (-not $partial.accepted -or $partial.protocol2Receipt -or -not $partial.savedWithSourceErrors) { throw '1066/1 must be an accepted non-receipt save' }
$clean = Test-FxTxAcceptedStopExit 0 0
if (-not $clean.accepted -or $clean.protocol2Receipt -or $clean.savedWithSourceErrors) { throw '0/0 without a receipt file is still not protocol2' }
if ((Test-FxTxAcceptedStopExit 1066 2).accepted) { throw 'panic exit must not be accepted' }
if ((Test-FxTxAcceptedStopExit 1 0).accepted) { throw 'unspecified exit must not be accepted' }
if ((Test-FxTxAcceptedStopExit 0 1).accepted) { throw 'specific 1 without 1066 must not be accepted' }

Write-Output 'upgrade stop-mode decision tests passed'

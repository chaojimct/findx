# Pure production capability parser checks: no service, UAC, index or registry access.
Set-StrictMode -Version Latest
$ErrorActionPreference='Stop'
Import-Module (Join-Path $PSScriptRoot '../installer/transaction-receipt.psm1') -Force
function Good { return '{"type":"upgrade_capabilities_result","protocol_version":2,"durable_prepare":true,"runtime_error":null,"last_checkpoint_error":null,"reason":"support is not a save ACK"}'|ConvertFrom-Json }
$count=0
$c=Good;Assert-FxTxUpgradeCapabilities $c;$count++
$c=Good;$c.last_checkpoint_error='previous AccessDenied, repaired; fresh prepare still required';Assert-FxTxUpgradeCapabilities $c;$count++
foreach($change in @(
 {param($c)$c.protocol_version=1},
 {param($c)$c.protocol_version='2'},
 {param($c)$c.durable_prepare=$false},
 {param($c)$c.durable_prepare='true'},
 {param($c)$c.runtime_error='late producer failure'},
 {param($c)$c.type='stop_committed'},
 {param($c)$c.PSObject.Properties.Remove('runtime_error')},
 {param($c)$c.PSObject.Properties.Remove('last_checkpoint_error')},
 {param($c)$c.reason=$null}
)){
 $c=Good;& $change $c;$rejected=$false;try{Assert-FxTxUpgradeCapabilities $c}catch{$rejected=$true}
 if(-not $rejected){throw 'Invalid capability accepted'};$count++
}
$source=Get-Content (Join-Path $PSScriptRoot '../installer/transaction-receipt.psm1') -Raw
$cap=$source.IndexOf(" $"+'capabilities=& $call');$identity=$source.IndexOf(" $"+'identity=& $call');$prepare=$source.IndexOf(" $"+'prepared=& $call');$stop=$source.IndexOf('$controller.Stop()')
if($cap -lt 0 -or $identity -le $cap -or $prepare -le $identity -or $stop -le $prepare){throw 'Capability must precede identity/prepare/STOP'};$count++
Write-Output ('PASS pure production capability/ordering assertions: '+$count)

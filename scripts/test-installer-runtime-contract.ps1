# Pure state-ordering unit tests; NO SCM, native receipt, index or OS mutation.
Set-StrictMode -Version Latest
$ErrorActionPreference='Stop'
$path=Join-Path $PSScriptRoot '..\installer\upgrade-transaction.ps1'
$tokens=$null;$errors=$null;$ast=[Management.Automation.Language.Parser]::ParseFile($path,[ref]$tokens,[ref]$errors)
if($errors.Count){throw ($errors|Out-String)}
foreach($name in @('Resolve-UpgradeServiceStart','Resolve-RollbackRuntime','Invoke-Rollback','Restore-OriginalRuntime')){
 $node=@($ast.FindAll({param($n) $n -is [Management.Automation.Language.FunctionDefinitionAst]},$true)|Where-Object {$_.Name -eq $name})
 if($node.Count -ne 1){throw 'Missing exact production function'}
 Invoke-Expression $node[0].Extent.Text
}
function Assert($ok,$why){if(-not $ok){throw $why}}
function Get-ExactService {if($script:missing){return $null};return [pscustomobject]@{State=$script:serviceState}}
function Enable-FxTxFullSecurity {}
function Invoke-FxTxPreparedServiceStop {param($ServiceName,$PipeName,$Image,$Index,$Cli,$ExpectedVersion,$Evidence,$TimeoutSeconds,$OnState)
 $script:events.Add('receipt');if($script:receiptFails){throw 'actual receipt refusal simulated for state unit test'};$script:serviceState='Stopped';& $OnState ([pscustomobject]@{stage='stopped'})
}
function Set-State($name){$script:s.state=$name;$script:events.Add($name)}
function Save-State {}
function Get-InstalledGui {return ,@()}
function Restore-FxTxVerifiedBackup {$script:events.Add('restore')}
function Restore-FxTxPlatform {$script:events.Add('platform')}
function Start-Service {$script:events.Add('start');$script:serviceState='Running'}
function Wait-NewServiceReady {$script:events.Add('ready');if($script:healthFails){throw 'unit readiness failure'}}
# Avoid even instantiating a ServiceController: run initial restore in Running
# state after emulated backup restoration. The receipt-before-restore ordering
# and refusal state are the contract under test; actual start/SCM remains CI.
function Restore-FxTxVerifiedBackup {$script:events.Add('restore');$script:serviceState='Running'}
$TransactionRoot=Join-Path ([IO.Path]::GetTempPath()) ('findx-pure-state-'+[guid]::NewGuid().ToString('N'))
# Production function creates only this owned evidence directory, never an index.
$rows=@()
try{
 foreach($case in @('receipt-before-restore','receipt-refusal-retains-safe-state','missing-original-registration')){
  $script:events=[Collections.Generic.List[string]]::new();$script:serviceState='Running';$script:missing=$case -eq 'missing-original-registration';$script:receiptFails=$case -eq 'receipt-refusal-retains-safe-state';$script:healthFails=$false
  $script:s=[pscustomobject]@{state='installed';backupVerified=$true;candidateServiceStarted=$true;createdService=$false;serviceName='NotARealService';pipeName='NotARealPipe';expectedVersion='2.5.3';timeoutSeconds=1;roots=[pscustomobject]@{app='not-a-real-app';data='not-a-real-index'};platform=$null;terminalHandoff=[pscustomobject]@{stage='stopped'};original=[pscustomobject]@{serviceExists=$true;serviceRunning=$true;guiRunning=$false;guiExitRequested=$false}}
  $failed=$false;try{if($script:missing){Restore-OriginalRuntime}else{Invoke-Rollback}}catch{$failed=$true}
  if($case -eq 'receipt-before-restore'){Assert (-not $failed -and $script:events.IndexOf('receipt') -lt $script:events.IndexOf('restore') -and $script:s.state -eq 'rolled_back') 'Receipt must precede any old-set restore'}
  else{Assert $failed 'Unsafe state was incorrectly accepted';Assert (-not $script:events.Contains('restore') -and -not $script:events.Contains('rolled_back')) 'Failure must never claim old-set restore or recovery'}
  $rows+=@{case=$case;passed=$true;kind='pure production state-order unit, not native acceptance';events=@($script:events.ToArray())}
 }
 Assert ((Resolve-UpgradeServiceStart $true $true 'Manual' 'Stopped') -eq 'start-prior-running') 'Previously running service must still restart'
 Assert ((Resolve-UpgradeServiceStart $true $false 'Auto' 'Stopped') -eq 'start-automatic') 'Stopped automatic service must start after a successful upgrade'
 Assert ((Resolve-UpgradeServiceStart $true $false 'Automatic' 'Stopped') -eq 'start-automatic') 'Automatic startup alias must start'
 Assert ((Resolve-UpgradeServiceStart $true $false 'Manual' 'Stopped') -eq 'leave-stopped') 'Previously stopped manual service must stay stopped'
 Assert ((Resolve-UpgradeServiceStart $true $false 'Disabled' 'Stopped') -eq 'leave-stopped') 'Disabled service must stay stopped'
 Assert ((Resolve-UpgradeServiceStart $false $false 'Auto' 'Stopped') -eq 'skip-absent') 'A service created in this transaction is not this decision'
 $runningHandoff=$false;try{Resolve-UpgradeServiceStart $true $true 'Auto' 'Running'}catch{$runningHandoff=$true};Assert $runningHandoff 'A previously running service must still be stopped before the candidate starts'
 Assert ((Resolve-RollbackRuntime $true 'Stopped') -eq 'start') 'Rollback must restart a service that was running before install'
 Assert ((Resolve-RollbackRuntime $false 'Stopped') -eq 'leave-stopped') 'Rollback must not start a service that was stopped before install'
 Assert ((Resolve-RollbackRuntime $false 'Running') -eq 'leave-stopped') 'Rollback must not keep a candidate started only because startup was automatic'
 $script:events=[Collections.Generic.List[string]]::new();$script:serviceState='Running';$script:missing=$false;$script:receiptFails=$false;$script:healthFails=$false
 $script:s=[pscustomobject]@{state='installed';backupVerified=$true;candidateServiceStarted=$true;createdService=$false;serviceName='NotARealService';pipeName='NotARealPipe';expectedVersion='2.5.3';timeoutSeconds=1;roots=[pscustomobject]@{app='not-a-real-app';data='not-a-real-index'};platform=$null;terminalHandoff=[pscustomobject]@{stage='stopped'};original=[pscustomobject]@{serviceExists=$true;serviceRunning=$false;guiRunning=$false;guiExitRequested=$false;serviceStartMode='Auto'}}
 Invoke-Rollback
 Assert ($script:s.state -eq 'rolled_back' -and $script:events.IndexOf('receipt') -ge 0 -and $script:events.IndexOf('receipt') -lt $script:events.IndexOf('restore') -and -not $script:events.Contains('start')) 'Rollback of an automatic start must stop the candidate and restore the prior stopped state'
 $rows+=@{case='stopped-automatic-upgrade-and-rollback';passed=$true;kind='pure production decision unit, not native SCM acceptance';events=@($script:events.ToArray())}
 $rows|ConvertTo-Json -Depth 8
}finally{if(Test-Path $TransactionRoot){Remove-Item -LiteralPath $TransactionRoot -Recurse -Force}}

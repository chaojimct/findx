# Isolated handoff unit. This is NOT production Windows acceptance of shipped 2.5.3.
# It does not touch ProgramData, services, setup, or a running FindX.
# Keep this file UTF-8 with BOM. Windows PowerShell 5.1 on ACP936 cannot parse it otherwise.
Set-StrictMode -Version Latest
$ErrorActionPreference='Stop'
$path=Join-Path $PSScriptRoot '..\installer\upgrade-transaction.ps1'
$tokens=$null;$errors=$null;$ast=[Management.Automation.Language.Parser]::ParseFile($path,[ref]$tokens,[ref]$errors)
if($errors.Count){throw ($errors|Out-String)}
foreach($name in @('Resolve-UpgradeLease','Get-LeaseRefusalMessage','Set-LeaseReason','Get-ShippedQuitContract','Get-NormalQuitPlan','Find-QuitMenuIndex','Get-MenuActivationKeys','Open-ExclusiveFile','Open-PhaseLock')){
 $node=@($ast.FindAll({param($n) $n -is [Management.Automation.Language.FunctionDefinitionAst]},$true)|Where-Object {$_.Name -eq $name})
 if($node.Count -ne 1){throw ('Missing exact production function '+$name)}
 Invoke-Expression $node[0].Extent.Text
}
function Assert($ok,$why){if(-not $ok){throw $why}}
$script:reasonCode=''
$contract=Get-ShippedQuitContract
Assert ($contract.trayCaption -ceq '退出 FindX（不停止索引服务）') 'Shipped tray caption drifted'
Assert ($contract.mainTitle -ceq 'FindX' -and $contract.settingsTitle -ceq 'FindX 设置') 'Window titles drifted'
Assert ($contract.trayMessage -ceq 'FindX.Native.Tray.v1' -and $contract.normalQuitMessage -ceq 'FindX.Native.NormalQuit.v1') 'Quit messages drifted'
$plan253=@(Get-NormalQuitPlan '2.5.3' 'not-the-published-252-hash')
Assert ($plan253.Count -eq 2 -and $plan253[0] -eq 'normal-quit-message' -and $plan253[1] -eq 'shipped-tray-menu') '2.5.3 must try the new message and then the shipped tray caption'
Assert ((@(Get-NormalQuitPlan '2.5.5' 'abc') -join ',') -eq 'normal-quit-message,shipped-tray-menu') 'A newer GUI keeps the same two-step quit'
$only252=@(Get-NormalQuitPlan '2.5.2' $contract.published252)
Assert ($only252.Count -eq 1 -and $only252[0] -eq 'legacy-252-close') 'Only the published 2.5.2 image may use window close'
Assert (@(Get-NormalQuitPlan '2.5.2' 'deadbeef')[0] -eq 'refuse') 'A different 2.5.2 hash must not be closed'
$menu=@('显示 FindX',$contract.trayCaption)
Assert ((Find-QuitMenuIndex $menu) -eq 1) 'The shipped quit caption is the second tray item'
Assert ((Find-QuitMenuIndex @($contract.trayCaption,'显示 FindX')) -eq 0) 'A caption in another slot must be found exactly once'
Assert ((Find-QuitMenuIndex @('退出','Quit','Exit')) -eq -1) 'The short names that UIA used to search are not the shipped command'
Assert ((Find-QuitMenuIndex @($contract.trayCaption,$contract.trayCaption)) -eq -1) 'Two quit captions are ambiguous'
$keys=@(Get-MenuActivationKeys 1)
Assert (($keys -join ',') -eq 'DOWN,DOWN,RETURN') 'The second item needs two downs before Enter'
Assert ((Resolve-UpgradeLease 'installed' $true $true $false) -eq 'refuse-live-owner') 'A live owner is refused'
Assert ((Resolve-UpgradeLease 'installed' $false $true $true) -eq 'refuse-lock-held') 'A busy lock with a dead owner is refused'
Assert ((Resolve-UpgradeLease 'installed' $false $false $false) -eq 'refuse-foreign') 'Another user or directory is refused'
Assert ((Resolve-UpgradeLease 'quiesced' $false $true $false) -eq 'recover-and-continue') 'A dead owner and a free lock continue after rollback'
Assert ((Resolve-UpgradeLease 'rolled_back' $false $true $false) -eq 'replace') 'A finished rollback can be replaced'
Assert ((Resolve-UpgradeLease 'committed' $true $false $true) -eq 'replace') 'A committed transaction can be replaced'
Set-LeaseReason 'refuse-lock-held'
Assert ($script:reasonCode -eq 'upgrade-lock-held') 'Lock refusal needs a stable reason code'
Assert ((Get-LeaseRefusalMessage 'refuse-lock-held') -eq '上一次升级仍在收尾，安装锁被占用。请稍后再试，不要删除锁文件。本次未改动。') 'Lock refusal stays a short Chinese sentence'
$root=Join-Path ([IO.Path]::GetTempPath()) ('findx-handoff-'+[guid]::NewGuid().ToString('N'))
try{
 [IO.Directory]::CreateDirectory($root)|Out-Null
 $held=Open-PhaseLock $root
 Assert ($null -ne $held) 'A free phase.lock can be held'
 $again=Open-PhaseLock $root
 Assert ($null -eq $again) 'A busy phase.lock is refused instead of deleted'
 Assert ([IO.File]::Exists((Join-Path $root 'phase.lock'))) 'Refusal must leave the lock file in place'
 $held.Dispose()
 Assert ([IO.File]::Exists((Join-Path $root 'phase.lock'))) 'Releasing the handle must not delete the lock'
 $reopen=Open-PhaseLock $root
 Assert ($null -ne $reopen) 'The same lock can be held again after the owner releases it'
 $reopen.Dispose()
 Assert ([IO.File]::Exists((Join-Path $root 'phase.lock'))) 'The second release must not delete the lock'
}finally{ if(Test-Path -LiteralPath $root){ Remove-Item -LiteralPath $root -Recurse -Force } }
Write-Output 'isolated-handoff-unit-ok'

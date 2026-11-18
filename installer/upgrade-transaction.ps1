# Called by the official Inno script. No force-stop or process-name termination.
[CmdletBinding()]
param(
 [Parameter(Mandatory=$true)][ValidateSet('Prepare','Verify','Commit','Rollback')][string]$Phase,
 [Parameter(Mandatory=$true)][string]$TransactionRoot,
 [string]$InstallRoot,[string]$DataRoot,[string]$NativeConfigRoot,[string]$LegacyConfigRoot,
 [string]$ServiceName='FindX2Search',[string]$PipeName='findx2',[string]$ExpectedVersion,
 [int]$InstallService=0,[int]$TimeoutSeconds=12,[int]$AddPath=0,[string]$UninstallKey,[string]$GroupRoot,[string]$DesktopRoot,[int]$CreateGroup=0,
 [switch]$TestBuild,[ValidateSet('none','backup','health','stop-timeout','interrupt-after-copy')][string]$Fault='none'
)
Set-StrictMode -Version Latest
$ErrorActionPreference='Stop'
Import-Module (Join-Path $PSScriptRoot 'transaction-files.psm1') -Force -Global
Import-Module (Join-Path $PSScriptRoot 'transaction-platform.psm1') -Force -Global
$script:s=$null; $script:lock=$null
function Save-State {
 $temp=Join-Path $TransactionRoot ('journal.'+[guid]::NewGuid().ToString('N')+'.tmp')
 $bytes=[Text.UTF8Encoding]::new($false).GetBytes(($script:s | ConvertTo-Json -Depth 60))
 $f=[IO.File]::Open($temp,'CreateNew','Write','None'); try{$f.Write($bytes,0,$bytes.Length);$f.Flush($true)}finally{$f.Dispose()}
 $dest=Join-Path $TransactionRoot 'journal.json'
 if([IO.File]::Exists($dest)){[IO.File]::Replace($temp,$dest,(Join-Path $TransactionRoot ('journal.previous.'+[guid]::NewGuid().ToString('N')+'.json')))}else{[IO.File]::Move($temp,$dest)}
}
function Set-State([string]$name){$script:s.state=$name;Save-State}
function Get-InstallerOwner {
 $parent=(Get-CimInstance Win32_Process -Filter "ProcessId=$PID").ParentProcessId
 $p=Get-Process -Id $parent -ErrorAction Stop
 return [pscustomobject]@{pid=[int]$p.Id;startTicks=$p.StartTime.ToUniversalTime().Ticks;sid=[Security.Principal.WindowsIdentity]::GetCurrent().User.Value}
}
function Test-InstallerOwnerAlive($Owner) {
 $p=Get-Process -Id $Owner.pid -ErrorAction SilentlyContinue
 return $p -and $p.StartTime.ToUniversalTime().Ticks -eq [long]$Owner.startTicks
}
function Enter-InstallerLease {
 $leasePath=Join-Path $script:leaseStore 'active.json'
 $guard=[IO.File]::Open((Join-Path $script:leaseStore 'active.lock'),'OpenOrCreate','ReadWrite','None')
 try {
  $me=Get-InstallerOwner
  if([IO.File]::Exists($leasePath)){
   $lease=Get-Content -LiteralPath $leasePath -Raw -Encoding UTF8 | ConvertFrom-Json
   if($lease.schema -ne 1){throw 'Unknown active transaction schema; prior evidence retained'}
   $oldRoot=Get-FxTxPath $lease.root;Assert-FxTxPrivateDirectory $oldRoot
   $journal=Join-Path $oldRoot 'journal.json'
   if(-not [IO.File]::Exists($journal) -or (Get-Item -LiteralPath $journal).Length -gt 67108864){throw 'Active transaction has no complete bounded journal; no replacement'}
   $old=Get-Content -LiteralPath $journal -Raw -Encoding UTF8 | ConvertFrom-Json
   if($old.schema -ne 2){throw 'Unsupported interrupted transaction; no replacement'}
   if($old.state -notin @('committed','rolled_back')){
    if(Test-InstallerOwnerAlive $lease.owner){throw 'Another installer still owns this product transaction'}
    if($lease.owner.sid -cne $me.sid -or $old.roots.app -ine $script:s.roots.app){throw 'Interrupted transaction belongs to a different user/install root; retained, not guessed'}
    $oldLock=[IO.File]::Open((Join-Path $oldRoot 'phase.lock'),'Open','ReadWrite','None')
    try{
     # A dead setup can leave a still-live helper: its phase lock prevents racing it.
     $requested=$TransactionRoot
     $TransactionRoot=$oldRoot;$script:s=$old;$script:s.installerOwner=$me
     Write-FxTxUpdateJson $leasePath ([ordered]@{schema=1;root=$oldRoot;owner=$me})
     Save-State;Invoke-Rollback
     $restart=Join-Path $oldRoot 'restart-original-user-gui.flag'
     $TransactionRoot=$requested;$script:s=$null
     if([IO.File]::Exists($restart)){[IO.File]::Copy($restart,(Join-Path $requested 'restart-original-user-gui.flag'),$false)}
     throw 'Previous interrupted upgrade was recovered and verified. No new upgrade was installed; run Setup again.'
    }finally{$oldLock.Dispose()}
   }
  }
  $script:s.installerOwner=$me;Save-State
  Write-FxTxUpdateJson $leasePath ([ordered]@{schema=1;root=$TransactionRoot;owner=$me})
 }finally{$guard.Dispose()}
}
function Assert-InstallerLease {
 $lease=Get-Content -LiteralPath (Join-Path $script:leaseStore 'active.json') -Raw -Encoding UTF8 | ConvertFrom-Json
 $me=Get-InstallerOwner
 if($lease.schema -ne 1 -or $lease.root -ine $TransactionRoot -or $lease.owner.pid -ne $me.pid -or [long]$lease.owner.startTicks -ne $me.startTicks){throw 'This installer does not own the active transaction'}
}

function Get-ExactService {
 $svc=Get-CimInstance Win32_Service -Filter ("Name='"+$script:s.serviceName+"'")
 if($svc){
  # This release does not migrate an unexpected service command or account.
  $expected=Join-Path $script:s.roots.app 'findx2-service.exe'
  if(-not $svc.PathName.StartsWith('"'+$expected+'" ',[StringComparison]::OrdinalIgnoreCase)){throw 'Unexpected service image/arguments: refusing to change it'}
  if($svc.ServiceType -ne 'Own Process'){throw 'Shared-process service is unsupported'}
 }
 return $svc
}
function Wait-ServiceStopped([int]$oldPid) {
 $until=[DateTime]::UtcNow.AddSeconds($script:s.timeoutSeconds)
 do{
  $v=Get-ExactService
  $process=if($oldPid){Get-Process -Id $oldPid -ErrorAction SilentlyContinue}else{$null}
  if(((-not $v) -or $v.State -eq 'Stopped') -and -not $process){return}
  Start-Sleep -Milliseconds 100
 }while([DateTime]::UtcNow -lt $until)
 throw 'Graceful stop timeout; no force-stop was attempted'
}
function Stop-ExactService {
 $v=Get-ExactService
 if($v -and $v.State -ne 'Stopped'){
  $controller=[ServiceProcess.ServiceController]::new($script:s.serviceName)
  try{$controller.Stop()}finally{$controller.Dispose()}
  Wait-ServiceStopped ([int]$v.ProcessId)
 }
}
function Restore-OriginalRuntime {
 $v=Get-ExactService
 if($script:s.original.serviceRunning -and $v -and $v.State -eq 'Stopped'){
  Start-Service -Name $script:s.serviceName
  $controller=[ServiceProcess.ServiceController]::new($script:s.serviceName)
  try{$controller.WaitForStatus([ServiceProcess.ServiceControllerStatus]::Running,[TimeSpan]::FromSeconds($script:s.timeoutSeconds))}finally{$controller.Dispose()}
 }
 $v=Get-ExactService
 if($script:s.original.serviceRunning -and $v -and $v.State -ne 'Running'){throw 'Original service is still transitioning; runtime recovery not confirmed'}
 # Existing stopped services are deliberately NOT started.
 if($script:s.original.guiRunning -and $script:s.original.guiExitRequested -and (Get-InstalledGui).Count -eq 0){[IO.File]::WriteAllText((Join-Path $TransactionRoot 'restart-original-user-gui.flag'),(Join-Path $script:s.roots.app 'FindX.exe'))}
}
function Get-InstalledGui {
 $path=Join-Path $script:s.roots.app 'FindX.exe'
 return ,@(Get-CimInstance Win32_Process -Filter "Name='FindX.exe'" | Where-Object {$_.ExecutablePath -and $_.ExecutablePath.Equals($path,[StringComparison]::OrdinalIgnoreCase)})
}
function Request-NormalGuiExit {
 $targets=Get-InstalledGui
 if(-not $targets.Count){return}
 Add-Type -AssemblyName UIAutomationClient
 Add-Type -AssemblyName UIAutomationTypes
 foreach($target in $targets){
  $condition=[Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::ProcessIdProperty,[int]$target.ProcessId)
  $windows=[Windows.Automation.AutomationElement]::RootElement.FindAll([Windows.Automation.TreeScope]::Children,$condition)
  $invoked=$false
  foreach($window in $windows){
   foreach($name in @('退出','Quit','Exit')){
    $matches=$window.FindAll([Windows.Automation.TreeScope]::Descendants,[Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::NameProperty,$name))
    if($matches.Count -eq 1 -and $matches[0].Current.IsEnabled){
     $pattern=$null
     if($matches[0].TryGetCurrentPattern([Windows.Automation.InvokePattern]::Pattern,[ref]$pattern)){$script:s.original.guiExitRequested=$true;Save-State;$pattern.Invoke();$invoked=$true;break}
    }
   }
   if($invoked){break}
  }
  if(-not $invoked){throw 'Installed GUI has no supported normal Quit entry available; installation unchanged'}
  $until=[DateTime]::UtcNow.AddSeconds($script:s.timeoutSeconds)
  while((Get-Process -Id $target.ProcessId -ErrorAction SilentlyContinue) -and [DateTime]::UtcNow -lt $until){Start-Sleep -Milliseconds 100}
  if(Get-Process -Id $target.ProcessId -ErrorAction SilentlyContinue){throw 'GUI did not exit normally; no close/hide/force fallback'}
  $script:s.original.guiStopped=$true;Save-State
 }
}
function Verify-PayloadAndIndex {
 $path=Join-Path $script:s.roots.app 'payload-manifest.json'
 if(-not [IO.File]::Exists($path) -or (Get-Item -LiteralPath $path).Length -gt 16777216){throw 'Missing/bounded payload manifest'}
 $manifest=Get-Content -LiteralPath $path -Raw -Encoding UTF8 | ConvertFrom-Json
 if($manifest.version -ne $script:s.expectedVersion){throw 'Payload manifest version mismatch'}
 foreach($f in $manifest.files){
  Assert-FxTxRelative $f.name
  $full=Join-Path $script:s.roots.app $f.name
  if(-not [IO.File]::Exists($full) -or (Get-Item -LiteralPath $full).Length -ne $f.length -or (Get-FxTxFileHash $full) -ne $f.sha256){throw ('Payload hash mismatch: '+$f.name)}
 }
 $indexes=@((Join-Path $script:s.roots.data 'index.bin'),(Join-Path $TransactionRoot 'backup\data\index.bin'))
 foreach($index in $indexes){
 if([IO.File]::Exists($index) -or [IO.Directory]::Exists($index+'.v6')){
  # This verifies already-stopped index readability/integrity, NEVER latest save ACK.
  $cli=Join-Path $script:s.roots.app 'findx2.exe'
  $info=[Diagnostics.ProcessStartInfo]::new($cli,('status -i "'+$index+'"'));$info.UseShellExecute=$false;$info.CreateNoWindow=$true
  $info.RedirectStandardOutput=$true;$info.RedirectStandardError=$true;$info.StandardOutputEncoding=[Text.UTF8Encoding]::new($false);$info.StandardErrorEncoding=[Text.UTF8Encoding]::new($false)
  $child=[Diagnostics.Process]::Start($info)
  $out=$child.StandardOutput.ReadToEndAsync();$err=$child.StandardError.ReadToEndAsync()
  if(-not $child.WaitForExit(30000)){throw 'Read-only index verification timeout; no force-stop'}
  [IO.File]::WriteAllText((Join-Path $TransactionRoot ('index-health-'+[array]::IndexOf($indexes,$index)+'.log')),($out.Result+"`n"+$err.Result))
  if($child.ExitCode -ne 0 -or ($out.Result+"`n"+$err.Result) -match '回退|failed integrity|走全量重建'){throw 'Stored index did not validate without fallback; original state/evidence retained'}
 }}
 $null=Read-FxTxVerifiedBackup (Join-Path $TransactionRoot 'backup')
}
function Invoke-Rollback {
 if($script:s.state -eq 'committed' -or $script:s.state -eq 'rolled_back'){return}
 if($script:s.backupVerified){
  # Each installer phase is a NEW PS process. Audit-aware verification must be
  # re-enabled during rollback/recovery, not just during initial backup.
  Enable-FxTxFullSecurity
  Set-State 'rollback_requested'; Stop-ExactService
  if((Get-InstalledGui).Count){throw 'Candidate GUI still active; retained backup, refusing unsafe restore'}
  if($script:s.createdService){
   $v=Get-ExactService
   if($v){& "$env:SystemRoot\System32\sc.exe" delete $script:s.serviceName | Out-Null;if($LASTEXITCODE){throw 'Cannot remove this transaction service'}}
  }
  Set-State 'restoring'
  $null=Restore-FxTxVerifiedBackup (Join-Path $TransactionRoot 'backup') (Join-Path $TransactionRoot 'failed-new')
  Set-State 'restoration_verified'
 }
 if($script:s.platform){Restore-FxTxPlatform $script:s.platform (Join-Path $TransactionRoot 'failed-shortcuts')}
 Restore-OriginalRuntime; Set-State 'rolled_back'
}
try{
 $TransactionRoot=Get-FxTxPath $TransactionRoot
 $script:leaseStore=if($TestBuild){Join-Path ([IO.Path]::GetDirectoryName($TransactionRoot)) '.findx-upgrade-store'}else{[IO.Path]::GetDirectoryName($TransactionRoot)}
 if($Phase -eq 'Prepare'){
  $roots=[ordered]@{app=(Get-FxTxPath $InstallRoot);data=(Get-FxTxPath $DataRoot);nativeConfig=(Get-FxTxPath $NativeConfigRoot);legacyConfig=(Get-FxTxPath $LegacyConfigRoot)}
  if($ServiceName -notmatch '^[A-Za-z][A-Za-z0-9_-]{0,100}$' -or $PipeName -notmatch '^[A-Za-z][A-Za-z0-9_-]{0,100}$'){throw 'Invalid exact endpoint name'}
  if($TestBuild){
   if($env:GITHUB_ACTIONS -ne 'true' -or $ServiceName -notmatch '^FindXUpgradeTest_[a-f0-9]{32}$' -or $PipeName -notmatch '^findx-upgrade-test-[a-f0-9]{32}$'){throw 'Test package is restricted to isolated CI namespace'}
   $allowed=Get-FxTxPath $env:RUNNER_TEMP
   foreach($path in @($roots.Values)+@($TransactionRoot)){if(-not $path.StartsWith($allowed+'\',[StringComparison]::OrdinalIgnoreCase)){throw 'Test path outside runner temp'}}
  }elseif($ServiceName -ne 'FindX2Search' -or $PipeName -ne 'findx2' -or $Fault -ne 'none'){throw 'Production endpoints/fault policy cannot be overridden'}
  Set-FxTxPrivateDirectory $script:leaseStore
  if(Test-Path -LiteralPath (Join-Path $TransactionRoot 'journal.json')){throw 'Unfinished/existing transaction requires recovery, not overwrite'}
  if(Test-Path -LiteralPath $TransactionRoot){throw 'Transaction root must be new; prior evidence retained'}
  Set-FxTxPrivateDirectory $TransactionRoot
  $script:s=[pscustomobject]@{schema=2;installerOwner=$null;state='created';roots=[pscustomobject]$roots;serviceName=$ServiceName;pipeName=$PipeName;expectedVersion=$ExpectedVersion;installService=($InstallService -eq 1);timeoutSeconds=$TimeoutSeconds;testBuild=[bool]$TestBuild;fault=$Fault;platform=$null;backupVerified=$false;createdService=$false;original=[pscustomobject]@{serviceExists=$false;serviceRunning=$false;guiRunning=$false;guiStopped=$false;guiExitRequested=$false;serviceCommand='';serviceStartMode=''};error=''}
  $script:lock=[IO.File]::Open((Join-Path $TransactionRoot 'phase.lock'),'OpenOrCreate','ReadWrite','None')
  Save-State;Enter-InstallerLease
  $svc=Get-ExactService
  if($svc){$script:s.original.serviceExists=$true;$script:s.original.serviceRunning=($svc.State -eq 'Running');$script:s.original.serviceCommand=$svc.PathName;$script:s.original.serviceStartMode=$svc.StartMode;if($svc.State -notin @('Running','Stopped')){throw 'Service in transition; unchanged'}}
  $script:s.original.guiRunning=((Get-InstalledGui).Count -gt 0); Save-State
  if($script:s.original.serviceRunning){
   # Published 2.5.2 has no durable-save ACK. A stop/exit0 or old valid checksum
   # cannot prove metadata-only dirty state was committed. Never pretend it does.
   # Production refuses BEFORE stopping a legacy writer or closing its GUI.
   # Only the explicit isolated-CI adversarial fixture exercises the bounded stop.
   if($script:s.testBuild -and $script:s.fault -eq 'stop-timeout'){Stop-ExactService}
   throw 'Legacy running service has no verifiable final-save acknowledgment; refusing before replacement. No production stop was requested.'
  }
  if(-not $script:s.testBuild){
   $me=[Security.Principal.WindowsIdentity]::GetCurrent().User.Value;$session=(Get-Process -Id $PID).SessionId
   $owners=@(Get-CimInstance Win32_Process -Filter "Name='explorer.exe' AND SessionId=$session" | ForEach-Object {(Invoke-CimMethod -InputObject $_ -MethodName GetOwnerSid).Sid} | Sort-Object -Unique)
   if($owners.Count -ne 1 -or $owners[0] -ne $me){throw 'Original user profile cannot be verified in this session; no profile or running application changed'}
  }
  foreach($p in $roots.Values){if([IO.Path]::GetPathRoot($p) -ine [IO.Path]::GetPathRoot($TransactionRoot)){throw 'Cross-volume atomic recovery is unsupported; refused before replacement'}}
  Request-NormalGuiExit
  Enable-FxTxFullSecurity
  Set-State 'quiesced'
  $links=@();if($CreateGroup -eq 1){$links+=@(Join-Path $GroupRoot 'FindX.lnk');$links+=@(Join-Path $GroupRoot '卸载 FindX.lnk')};if($DesktopRoot){$links+=@(Join-Path $DesktopRoot 'FindX.lnk')}
  if($script:s.testBuild){foreach($link in $links){if(-not (Get-FxTxPath $link).StartsWith($allowed+'\',[StringComparison]::OrdinalIgnoreCase)){throw 'Test shortcut outside runner temp'}}}
  $script:s.platform=Get-FxTxPlatformSnapshot $UninstallKey $ServiceName $links $roots.app ($AddPath -eq 1);Save-State
  if($script:s.fault -eq 'backup'){throw 'Injected CI backup failure before any replacement'}
  $map=[ordered]@{};foreach($prop in $script:s.roots.PSObject.Properties){$map[$prop.Name]=[string]$prop.Value}
  # Reserve a second old-set copy for rollback, plus bounded candidate staging.
  $estimate=Get-FxTxInventory $map
  $null=New-FxTxVerifiedBackup $map (Join-Path $TransactionRoot 'backup') -CandidateBytes ([decimal]$estimate.bytes + 268435456) -AdministratorsOnly
  $script:s.backupVerified=$true;Set-State 'backup_verified'
 }else{
  $script:lock=[IO.File]::Open((Join-Path $TransactionRoot 'phase.lock'),'Open','ReadWrite','None')
  $script:s=Get-Content -LiteralPath (Join-Path $TransactionRoot 'journal.json') -Raw -Encoding UTF8 | ConvertFrom-Json
  $script:leaseStore=if($script:s.testBuild){Join-Path ([IO.Path]::GetDirectoryName($TransactionRoot)) '.findx-upgrade-store'}else{[IO.Path]::GetDirectoryName($TransactionRoot)}
  Assert-FxTxPrivateDirectory $TransactionRoot;Assert-InstallerLease
  if($script:s.schema -ne 2){throw 'Unsupported transaction schema'}
  if($Phase -eq 'Verify'){
   if($script:s.state -ne 'backup_verified'){throw 'Invalid verification transition'}
   Set-State 'installed'
   $gui=Join-Path $script:s.roots.app 'FindX.exe'
   if(-not [IO.File]::Exists($gui) -or (Get-Item -LiteralPath $gui).VersionInfo.ProductVersion -ne $script:s.expectedVersion){throw 'Installed GUI version mismatch'}
   # Never start a candidate writer before stored data and backup validate.
   Verify-PayloadAndIndex
   if($script:s.installService -and -not $script:s.original.serviceExists){
    # Registration is exact-name and happens ONLY after verified backup/file copy.
    $exe=Join-Path $script:s.roots.app 'findx2-service.exe'
    $cmd='"'+$exe+'" --service --index "'+(Join-Path $script:s.roots.data 'index.bin')+'" --volume C: --pipe '+$script:s.pipeName
    if($script:s.testBuild){$cmd+=' --no-watch --no-backfill --no-everything-ipc'}
    $script:s.createdService=$true;Save-State
    $null=New-Service -Name $script:s.serviceName -BinaryPathName $cmd -StartupType Automatic
    # Fresh explicit service selection may start it; stopped old service never starts.
    Start-Service -Name $script:s.serviceName
    $c=[ServiceProcess.ServiceController]::new($script:s.serviceName);try{$c.WaitForStatus([ServiceProcess.ServiceControllerStatus]::Running,[TimeSpan]::FromSeconds($script:s.timeoutSeconds))}finally{$c.Dispose()}
   }
   if($script:s.fault -eq 'health'){if($script:s.testBuild){[IO.File]::WriteAllText((Join-Path $script:s.roots.data 'index.bin'),'deliberately damaged CI fixture only')};throw 'Injected CI post-copy health failure'}
   Apply-FxTxPlatformPath $script:s.platform
   Set-State 'health_verified'
  }elseif($Phase -eq 'Commit'){
   if($script:s.state -ne 'health_verified'){throw 'Cannot commit without verified health'}
   Set-State 'committed';if($script:s.original.guiRunning -and $script:s.original.guiExitRequested -and (Get-InstalledGui).Count -eq 0){[IO.File]::WriteAllText((Join-Path $TransactionRoot 'restart-original-user-gui.flag'),(Join-Path $script:s.roots.app 'FindX.exe'))}
  }else{Invoke-Rollback}
 }
 exit 0
}catch{
 $message=$_.Exception.ToString()
 if($script:s){
  $script:s.error=$message
  try{Save-State;if($Phase -eq 'Prepare'){Invoke-Rollback}}catch{$message+='; recovery also refused: '+$_.Exception.Message}
 }
 if([IO.Directory]::Exists($TransactionRoot)){[IO.File]::WriteAllText((Join-Path $TransactionRoot 'failure.txt'),$message);if($message -match 'Legacy running service'){[IO.File]::WriteAllText((Join-Path $TransactionRoot 'reason.code'),'legacy-save-unconfirmed')}}
 Write-Error $message -ErrorAction Continue
 exit 1
}finally{if($script:lock){$script:lock.Dispose()}}

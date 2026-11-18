# Called by the official Inno script. No force-stop or process-name termination.
[CmdletBinding()]
param(
 [Parameter(Mandatory=$true)][ValidateSet('Prepare','Verify','Commit','Rollback')][string]$Phase,
 [Parameter(Mandatory=$true)][string]$TransactionRoot,
 [string]$InstallRoot,[string]$DataRoot,[string]$NativeConfigRoot,[string]$LegacyConfigRoot,
 [string]$ServiceName='FindX2Search',[string]$PipeName='findx2',[string]$ExpectedVersion,
 [int]$InstallService=0,[int]$TimeoutSeconds=12,
 [switch]$TestBuild,[ValidateSet('none','backup','health')][string]$Fault='none'
)
Set-StrictMode -Version Latest
$ErrorActionPreference='Stop'
Import-Module (Join-Path $PSScriptRoot 'transaction-files.psm1') -Force
$script:s=$null; $script:lock=$null
function Save-State {
 $temp=Join-Path $TransactionRoot ('journal.'+[guid]::NewGuid().ToString('N')+'.tmp')
 $bytes=[Text.UTF8Encoding]::new($false).GetBytes(($script:s | ConvertTo-Json -Depth 15))
 $f=[IO.File]::Open($temp,'CreateNew','Write','None'); try{$f.Write($bytes,0,$bytes.Length);$f.Flush($true)}finally{$f.Dispose()}
 $dest=Join-Path $TransactionRoot 'journal.json'
 if([IO.File]::Exists($dest)){[IO.File]::Replace($temp,$dest,(Join-Path $TransactionRoot ('journal.previous.'+[guid]::NewGuid().ToString('N')+'.json')))}else{[IO.File]::Move($temp,$dest)}
}
function Set-State([string]$name){$script:s.state=$name;Save-State}
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
 if($script:s.original.serviceRunning -and $v -and $v.State -ne 'Running'){throw 'Original service is still transitioning; runtime recovery not confirmed'}
 # Existing stopped services are deliberately NOT started.
 if($script:s.original.guiRunning){[IO.File]::WriteAllText((Join-Path $TransactionRoot 'restart-original-user-gui.flag'),'1')}
}
function Get-InstalledGui {
 $path=Join-Path $script:s.roots.app 'FindX.exe'
 return @(Get-CimInstance Win32_Process -Filter "Name='FindX.exe'" | Where-Object {$_.ExecutablePath -and $_.ExecutablePath.Equals($path,[StringComparison]::OrdinalIgnoreCase)})
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
     if($matches[0].TryGetCurrentPattern([Windows.Automation.InvokePattern]::Pattern,[ref]$pattern)){$pattern.Invoke();$invoked=$true;break}
    }
   }
   if($invoked){break}
  }
  if(-not $invoked){throw 'Installed GUI has no supported normal Quit entry available; installation unchanged'}
  $until=[DateTime]::UtcNow.AddSeconds($script:s.timeoutSeconds)
  while((Get-Process -Id $target.ProcessId -ErrorAction SilentlyContinue) -and [DateTime]::UtcNow -lt $until){Start-Sleep -Milliseconds 100}
  if(Get-Process -Id $target.ProcessId -ErrorAction SilentlyContinue){throw 'GUI did not exit normally; no close/hide/force fallback'}
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
 $index=Join-Path $script:s.roots.data 'index.bin'
 if([IO.File]::Exists($index) -or [IO.Directory]::Exists($index+'.v6')){
  # This verifies already-stopped index readability/integrity, NEVER latest save ACK.
  $cli=Join-Path $script:s.roots.app 'findx2.exe'
  $info=[Diagnostics.ProcessStartInfo]::new($cli,('status -i "'+$index+'"'));$info.UseShellExecute=$false;$info.CreateNoWindow=$true
  $info.RedirectStandardOutput=$true;$info.RedirectStandardError=$true
  $child=[Diagnostics.Process]::Start($info)
  $out=$child.StandardOutput.ReadToEndAsync();$err=$child.StandardError.ReadToEndAsync()
  if(-not $child.WaitForExit(30000)){throw 'Read-only index verification timeout; no force-stop'}
  [IO.File]::WriteAllText((Join-Path $TransactionRoot 'index-health.txt'),($out.Result+"`n"+$err.Result))
  if($child.ExitCode -ne 0){throw 'Stopped index is not readable by the candidate CLI'}
 }
}
function Invoke-Rollback {
 if($script:s.state -eq 'committed' -or $script:s.state -eq 'rolled_back'){return}
 if($script:s.backupVerified){
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
 Restore-OriginalRuntime; Set-State 'rolled_back'
}
try{
 $TransactionRoot=Get-FxTxPath $TransactionRoot
 if($Phase -eq 'Prepare'){
  $roots=[ordered]@{app=(Get-FxTxPath $InstallRoot);data=(Get-FxTxPath $DataRoot);nativeConfig=(Get-FxTxPath $NativeConfigRoot);legacyConfig=(Get-FxTxPath $LegacyConfigRoot)}
  if($ServiceName -notmatch '^[A-Za-z][A-Za-z0-9_-]{0,100}$' -or $PipeName -notmatch '^[A-Za-z][A-Za-z0-9_-]{0,100}$'){throw 'Invalid exact endpoint name'}
  if($TestBuild){
   if($env:GITHUB_ACTIONS -ne 'true' -or $ServiceName -notmatch '^FindXUpgradeTest_[a-f0-9]{32}$' -or $PipeName -notmatch '^findx-upgrade-test-[a-f0-9]{32}$'){throw 'Test package is restricted to isolated CI namespace'}
   $allowed=Get-FxTxPath $env:RUNNER_TEMP
   foreach($path in @($roots.Values)+@($TransactionRoot)){if(-not $path.StartsWith($allowed+'\',[StringComparison]::OrdinalIgnoreCase)){throw 'Test path outside runner temp'}}
  }elseif($ServiceName -ne 'FindX2Search' -or $PipeName -ne 'findx2' -or $Fault -ne 'none'){throw 'Production endpoints/fault policy cannot be overridden'}
  if(Test-Path -LiteralPath (Join-Path $TransactionRoot 'journal.json')){throw 'Unfinished/existing transaction requires recovery, not overwrite'}
  $null=[IO.Directory]::CreateDirectory($TransactionRoot)
  $script:s=[pscustomobject]@{schema=1;state='created';roots=[pscustomobject]$roots;serviceName=$ServiceName;pipeName=$PipeName;expectedVersion=$ExpectedVersion;installService=($InstallService -eq 1);timeoutSeconds=$TimeoutSeconds;testBuild=[bool]$TestBuild;fault=$Fault;backupVerified=$false;createdService=$false;original=[pscustomobject]@{serviceExists=$false;serviceRunning=$false;guiRunning=$false;serviceCommand='';serviceStartMode=''};error=''}
  $script:lock=[IO.File]::Open((Join-Path $TransactionRoot 'phase.lock'),'OpenOrCreate','ReadWrite','None')
  $svc=Get-ExactService
  if($svc){$script:s.original.serviceExists=$true;$script:s.original.serviceRunning=($svc.State -eq 'Running');$script:s.original.serviceCommand=$svc.PathName;$script:s.original.serviceStartMode=$svc.StartMode;if($svc.State -notin @('Running','Stopped')){throw 'Service in transition; unchanged'}}
  $script:s.original.guiRunning=((Get-InstalledGui).Count -gt 0); Save-State
  Request-NormalGuiExit
  if($script:s.original.serviceRunning){
   # Published 2.5.2 has no durable-save ACK. A stop/exit0 or old valid checksum
   # cannot prove metadata-only dirty state was committed. Never pretend it does.
   Stop-ExactService
   throw 'Legacy running service has no verifiable final-save acknowledgment; refusing replacement. Original runtime will be restored.'
  }
  Set-State 'quiesced'
  if($script:s.fault -eq 'backup'){throw 'Injected CI backup failure before any replacement'}
  $map=[ordered]@{};foreach($prop in $script:s.roots.PSObject.Properties){$map[$prop.Name]=[string]$prop.Value}
  # Reserve a second old-set copy for rollback, plus bounded candidate staging.
  $estimate=Get-FxTxInventory $map
  $null=New-FxTxVerifiedBackup $map (Join-Path $TransactionRoot 'backup') -CandidateBytes ([decimal]$estimate.bytes + 268435456)
  $script:s.backupVerified=$true;Set-State 'backup_verified'
 }else{
  $script:lock=[IO.File]::Open((Join-Path $TransactionRoot 'phase.lock'),'Open','ReadWrite','None')
  $script:s=Get-Content -LiteralPath (Join-Path $TransactionRoot 'journal.json') -Raw -Encoding UTF8 | ConvertFrom-Json
  if($script:s.schema -ne 1){throw 'Unsupported transaction schema'}
  if($Phase -eq 'Verify'){
   if($script:s.state -ne 'backup_verified'){throw 'Invalid verification transition'}
   Set-State 'installed'
   $gui=Join-Path $script:s.roots.app 'FindX.exe'
   if(-not [IO.File]::Exists($gui) -or (Get-Item -LiteralPath $gui).VersionInfo.ProductVersion -ne $script:s.expectedVersion){throw 'Installed GUI version mismatch'}
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
   Verify-PayloadAndIndex
   Set-State 'health_verified'
  }elseif($Phase -eq 'Commit'){
   if($script:s.state -ne 'health_verified'){throw 'Cannot commit without verified health'}
   Set-State 'committed';if($script:s.original.guiRunning){[IO.File]::WriteAllText((Join-Path $TransactionRoot 'restart-original-user-gui.flag'),'1')}
  }else{Invoke-Rollback}
 }
 exit 0
}catch{
 $message=$_.Exception.ToString()
 if($script:s){
  $script:s.error=$message
  try{Save-State;if($Phase -eq 'Prepare'){Invoke-Rollback}}catch{$message+='; recovery also refused: '+$_.Exception.Message}
 }
 if([IO.Directory]::Exists($TransactionRoot)){[IO.File]::WriteAllText((Join-Path $TransactionRoot 'failure.txt'),$message)}
 Write-Error $message -ErrorAction Continue
 exit 1
}finally{if($script:lock){$script:lock.Dispose()}}

param([Parameter(Mandatory=$true)][string]$PublishedPayload,[Parameter(Mandatory=$true)][string]$CandidatePayload,[Parameter(Mandatory=$true)][string]$Iscc,[switch]$EngineCandidate,[string]$PayloadSourceCommit=$env:GITHUB_SHA,[string[]]$OnlyCases=@())
Set-StrictMode -Version Latest
$ErrorActionPreference='Stop'
if($env:GITHUB_ACTIONS -ne 'true' -or $env:RUNNER_OS -ne 'Windows' -or $env:COMPUTERNAME -eq 'MZ' -or $env:USERPROFILE -eq 'C:\Users\chaoj'){throw 'Only disposable Windows CI runner allowed'}
if(-not ([Security.Principal.WindowsPrincipal]::new([Security.Principal.WindowsIdentity]::GetCurrent())).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)){throw 'Runner has no native SCM rights; no UAC/fallback'}
$repo=[IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$base=Join-Path $env:RUNNER_TEMP ('FindXUpgradeTest_'+[guid]::NewGuid().ToString('N'))
$null=[IO.Directory]::CreateDirectory($base)
$rows=[Collections.Generic.List[object]]::new()
if($EngineCandidate){
 $manifest=Get-Content (Join-Path $CandidatePayload 'stage-manifest.json') -Raw|ConvertFrom-Json
 if($manifest.sourceCommit -cne $PayloadSourceCommit -or $manifest.engineUnchanged -ne $false){throw 'Not this exact rebuilt engine candidate'}
 foreach($f in $manifest.files){$x=Join-Path $CandidatePayload $f.name;if((Get-FileHash $x).Hash.ToLowerInvariant() -cne $f.sha256 -or (Get-Item $x).Length -ne $f.length){throw 'Exact new payload manifest mismatch'}}
}

Import-Module (Join-Path $repo 'installer\transaction-files.psm1') -Force -Global
Import-Module (Join-Path $repo 'installer\transaction-platform.psm1') -Force -Global
Import-Module (Join-Path $repo 'installer\transaction-receipt.psm1') -Force -Global
Import-Module (Join-Path $PSScriptRoot 'test-owned-ntfs-volume.psm1') -Force
Import-Module (Join-Path $PSScriptRoot 'native-shortcut-fixture.psm1') -Force
Enable-FxTxFullSecurity
function Journal([string]$root){return (Get-Content -LiteralPath (Join-Path $root 'journal.json') -Raw -Encoding UTF8 | ConvertFrom-Json)}
function AssertGuiHealth([string]$root){
 $render=Get-Content -LiteralPath (Join-Path $root 'gui-health\render-health.json') -Raw -Encoding UTF8 | ConvertFrom-Json
 $exit=Get-Content -LiteralPath (Join-Path $root 'gui-health\normal-exit.json') -Raw -Encoding UTF8 | ConvertFrom-Json
 Assert ($render.version -eq '2.5.8' -and $render.frames -ge 3 -and $render.storage_ok -and $render.isolated -and $render.normal_quit_requested) 'Actual final GUI render/persistence health missing'
 Assert ($exit.version -eq '2.5.8' -and $exit.passed -and $exit.normal_event_loop_return) 'Actual final GUI normal event-loop return missing'
}
function AssertIpcHealth([string]$root){
 $health=Get-Content -LiteralPath (Join-Path $root 'service-health.json') -Raw -Encoding UTF8 | ConvertFrom-Json
 Assert ($health.healthy -and -not $health.loading -and $health.entry_count -gt 0) 'Real loaded IPC readiness was never reached'
}
function AssertPlatformRestored($snapshot){
 $key=[Microsoft.Win32.Registry]::LocalMachine.OpenSubKey('SYSTEM\CurrentControlSet\Control\Session Manager\Environment')
 try{
  Assert ($key.GetValue('Path',$null,[Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames) -ceq $snapshot.path.value) 'PATH raw bytes/value not preserved'
  Assert ($key.GetValueKind('Path').ToString() -eq $snapshot.path.kind) 'PATH registry type changed'
 }finally{$key.Dispose()}
 foreach($link in $snapshot.shortcuts){
  Assert ([IO.File]::Exists($link.path) -eq $link.exists) 'Shortcut existence changed'
  if($link.exists){
   Assert ([Convert]::ToBase64String([IO.File]::ReadAllBytes($link.path)) -ceq $link.bytes) 'Shortcut bytes changed'
   Assert-FxTxSame (Get-FxTxMetadata $link.path) $link.metadata 'Actual ISS shortcut ACL/timestamps/attributes'
  }
 }
 Assert-FxTxSame (Read-FxTxRegistryTree $snapshot.serviceSpec) $snapshot.service 'Actual ISS service registration/security'
 Assert-FxTxSame (Read-FxTxRegistryTree $snapshot.uninstallSpec) $snapshot.uninstall 'Actual ISS uninstall registration/security'
}
function Assert($condition,[string]$reason){if(-not $condition){throw $reason}}
function Hash([string]$p){return (Get-FileHash -LiteralPath $p -Algorithm SHA256).Hash}
function Write-NativeResults {
 [ordered]@{scope=$(if($EngineCandidate){'actual ISS exact rebuilt engine/GUI source+hash manifest; real running engine and published252 refusal; synthetic service for unsafe image refusal and stop-timeout'}else{'historical GUI-only fixture; not new engine acceptance'});cases=@($rows.ToArray());passed=@($rows | Where-Object{$_.passed}).Count;failed=@($rows | Where-Object{-not $_.passed}).Count;payloadSourceCommit=$PayloadSourceCommit;installerSourceCommit=$env:GITHUB_SHA;requestedCases=@($cases);candidateGUIIntegrated=$true;candidateGuiSha256=(Hash (Join-Path $CandidatePayload 'FindX.exe'));productionTouched=$false} | ConvertTo-Json -Depth 10 | Set-Content -Encoding UTF8 (Join-Path $base 'results.json')
}
$hostSource=@'
using System;
using System.IO;
using System.ServiceProcess;
using System.Reflection;
[assembly:AssemblyFileVersion("2.5.2.0")]
[assembly:AssemblyInformationalVersion("2.5.2")]
class FixtureService:ServiceBase{
 string index;
 FixtureService(string name,string path){ServiceName=name;index=path;CanStop=true;}
 protected override void OnStart(string[] args){File.AppendAllText(index+".starts","start\n");}
 protected override void OnStop(){while(File.Exists(index+".hold-stop")){RequestAdditionalTime(1000);System.Threading.Thread.Sleep(100);}File.AppendAllText(index+".stops","stop\n");}
 static string Arg(string[] a,string k){int p=Array.IndexOf(a,k);return p>=0&&p+1<a.Length?a[p+1]:"";}
 static void Main(string[] args){string pipe=Arg(args,"--pipe");if(!pipe.StartsWith("findx-upgrade-test-"))throw new Exception("fixture namespace");string name="FindXUpgradeTest_"+pipe.Substring("findx-upgrade-test-".Length);ServiceBase.Run(new FixtureService(name,Arg(args,"--index")));}
}
'@
$fixtureHost=Join-Path $base 'service-fixture.exe'
Add-Type -TypeDefinition $hostSource -OutputAssembly $fixtureHost -OutputType WindowsApplication -ReferencedAssemblies System.ServiceProcess
$runningCases=@('running253-success','running253-save-refusal','running253-backup-failure','running253-health-rollback','running253-interrupted-recovery','running253-copy-verification-rollback')
$cases=@('fresh-no-service','published252-stopped-service','backup-failure','health-rollback','legacy-running-refusal','stop-timeout','reentry-refusal','interrupted-recovery','new-service-ipc','new-service-health-rollback','fresh-selected-no-index','platform-rollback')
if($EngineCandidate){$cases+= $runningCases}
if($OnlyCases.Count){
 if(@($OnlyCases|Select-Object -Unique).Count -ne $OnlyCases.Count -or @($OnlyCases|Where-Object {$_ -notin $cases}).Count){throw 'Unknown/duplicate requested cases'}
 $cases=@($cases|Where-Object {$_ -in $OnlyCases})
}
foreach($case in $cases){
 $id=[guid]::NewGuid().ToString('N');$service='FindXUpgradeTest_'+$id;$pipe='findx-upgrade-test-'+$id
 $root=Join-Path $base $case;$app=Join-Path $root 'app 中文 space';$data=Join-Path $root 'index state';$native=Join-Path $root 'native config';$legacy=Join-Path $root 'legacy config';$tx=Join-Path $root 'tx';$stage=Join-Path $root 'payload';$build=Join-Path $root 'build'
 foreach($p in @($root,$data,$native,$legacy,$stage,$build)){ $null=[IO.Directory]::CreateDirectory($p) }
 $created=$false;$saveAcl=$null;$saveV6=$null;$isRunningNew=$case -in $runningCases;$originalServicePid=0;$ownedUninstall='';$ownedVolumeImage='';$volume='C:'
 try{
  Write-Output ('NATIVE_CASE_START='+$case)
  Copy-Item -Path (Join-Path $CandidatePayload '*') -Destination $stage -Recurse
  Assert ((Hash (Join-Path $stage 'FindX.exe')) -eq (Hash (Join-Path $CandidatePayload 'FindX.exe'))) 'Candidate copy changed'
  # Create a real small public index with the UNCHANGED published engine, no watch/backfill/host.
  $owned=Join-Path $root 'owned public fixture';$null=[IO.Directory]::CreateDirectory($owned);[IO.File]::WriteAllText((Join-Path $owned 'owned.txt'),'installer safety fixture')
  if($case -in @('new-service-ipc','new-service-health-rollback') -or $isRunningNew){
   # --no-watch deliberately reports healthy=false in the published252engine.
   # Positive readiness therefore uses real USN ONLY on a new guarded tiny VHD.
   $ownedVolumeImage=Join-Path $root 'owned-health.vhd'
   $privateVolume=New-FxOwnedTestVolume $ownedVolumeImage ('FXTX'+$id.Substring(0,8).ToUpperInvariant())
   $volume=$privateVolume.Volume
   [IO.File]::WriteAllText((Join-Path $privateVolume.Root 'owned-health.txt'),'own tiny real NTFS fixture')
  }
  if($case -ne 'fresh-selected-no-index'){
  $stop=Join-Path $root 'foreground-stop.signal';[IO.File]::WriteAllText($stop,'stop')
  # PS5 promotes informational native stderr into terminating ErrorRecords under
  # ErrorActionPreference=Stop. Capture native streams, judge actual exit status.
  $indexExecutable=Join-Path $PublishedPayload 'findx2-service.exe'
  $indexArguments='--fixture-root "'+$owned+'" --index "'+(Join-Path $data 'index.bin')+'" --volume C: --pipe '+$pipe+' --no-watch --no-backfill --no-everything-ipc --shutdown-file "'+$stop+'"'
  if($ownedVolumeImage){$indexExecutable=Join-Path $PublishedPayload 'findx2.exe';$indexArguments='index --volume '+$volume+' --output "'+(Join-Path $data 'index.bin')+'"'}
  $pi=[Diagnostics.ProcessStartInfo]::new($indexExecutable,$indexArguments)
  $pi.UseShellExecute=$false;$pi.CreateNoWindow=$true;$pi.RedirectStandardOutput=$true;$pi.RedirectStandardError=$true
  $child=[Diagnostics.Process]::Start($pi);$stdout=$child.StandardOutput.ReadToEndAsync();$stderr=$child.StandardError.ReadToEndAsync()
  Assert ($child.WaitForExit(30000)) 'Published engine fixture creation timed out; no force-stop'
  [IO.File]::WriteAllText((Join-Path $root 'fixture-build.log'),($stdout.Result+"`n"+$stderr.Result))
  Assert ($child.ExitCode -eq 0) 'Published engine fixture creation failed'
  }
  [IO.File]::WriteAllText((Join-Path $native 'prefs.json'),'{"theme":"dark","unknown":"preserve"}')
  [IO.File]::WriteAllText((Join-Path $legacy 'old-settings.json'),'{"unknown":"preserve legacy"}')
  $oldIndex=if($case -eq 'fresh-selected-no-index'){''}else{Hash (Join-Path $data 'index.bin')};$oldConfig=Hash (Join-Path $native 'prefs.json')
  $oldGui=''
  if($case -notin @('fresh-no-service','new-service-ipc','new-service-health-rollback','fresh-selected-no-index')){
   $null=[IO.Directory]::CreateDirectory($app)
   foreach($name in @('FindX.exe','findx2.exe','fx.exe','findx2-service.exe')){Copy-Item -LiteralPath (Join-Path $(if($isRunningNew){$CandidatePayload}else{$PublishedPayload}) $name) -Destination (Join-Path $app $name)}
   [IO.File]::WriteAllBytes((Join-Path $app 'FindX.installed'),[byte[]](0x31,13,10));$oldGui=Hash (Join-Path $app 'FindX.exe')
  }
  if($case -in @('published252-stopped-service','legacy-running-refusal','stop-timeout') -or $isRunningNew){
   if($case -eq 'stop-timeout' -or $case -eq 'legacy-running-refusal'){Copy-Item -LiteralPath $fixtureHost -Destination (Join-Path $app 'findx2-service.exe') -Force}
   $command='"'+(Join-Path $app 'findx2-service.exe')+'" --service --index "'+(Join-Path $data 'index.bin')+'" --pipe '+$pipe
   if($isRunningNew){$command+=' --volume '+$volume+' --no-everything-ipc --save-interval-secs 1'}
   $null=New-Service -Name $service -BinaryPathName $command -StartupType Manual;$created=$true
   $null=New-ItemProperty -Path ('HKLM:\SYSTEM\CurrentControlSet\Services\'+$service) -Name Environment -PropertyType MultiString -Value @('ProgramData='+(Join-Path $root 'programdata'))
   if($case -in @('legacy-running-refusal','stop-timeout') -or $isRunningNew){if($case -eq 'stop-timeout'){[IO.File]::WriteAllText((Join-Path $data 'index.bin.hold-stop'),'hold')};Start-Service $service;$originalServicePid=[int](Get-CimInstance Win32_Service -Filter ("Name='"+$service+"'")).ProcessId}
  }
  if($isRunningNew){
   $until=[DateTime]::UtcNow.AddSeconds(45);$ready=$false
   do{try{$st=Read-FxTxServiceStatus $pipe $originalServicePid;$ready=(-not $st.loading -and $st.healthy -and $st.entry_count -gt 0 -and (-not $isRunningNew -or $st.metadata_ready))}catch{};if(-not $ready){[Threading.Thread]::Sleep(100)}}while(-not $ready -and [DateTime]::UtcNow -lt $until)
   Assert $ready 'Actual source engine never reached real watched IPC readiness'
   if($case -eq 'running253-save-refusal'){
    $saveV6=Join-Path $data 'index.bin.v6';$until=[DateTime]::UtcNow.AddSeconds(30);while(-not (Test-Path (Join-Path $saveV6 'published.json')) -and [DateTime]::UtcNow -lt $until){[Threading.Thread]::Sleep(100)};Assert (Test-Path (Join-Path $saveV6 'published.json')) 'No actual published writer stage';$saveAcl=Get-Acl $saveV6;$denied=Get-Acl $saveV6
    $rule=[Security.AccessControl.FileSystemAccessRule]::new([Security.Principal.SecurityIdentifier]::new('S-1-5-18'),[Security.AccessControl.FileSystemRights]'Write,Delete,DeleteSubdirectoriesAndFiles',[Security.AccessControl.InheritanceFlags]'ContainerInherit,ObjectInherit',[Security.AccessControl.PropagationFlags]::None,[Security.AccessControl.AccessControlType]::Deny)
    $denied.AddAccessRule($rule);Set-Acl $saveV6 $denied
   }
  }
  if($case -eq 'running253-copy-verification-rollback'){[IO.File]::AppendAllText((Join-Path $stage 'resources\component-manifest.json'),'deliberate-private-copied-payload-corruption')}
  & $Iscc '/DMyAppVersion=2.5.8' '/DTxTestBuild=1' (('/DMyServiceName=')+$service) (('/DTxPipeName=')+$pipe) ('/DPublishDir='+$stage) ('/O'+$build) (Join-Path $repo 'installer\FindX.iss') *> (Join-Path $root 'compile.log')
  Assert ($LASTEXITCODE -eq 0) 'Official ISS test-mode compilation failed'
  $setup=Join-Path $build 'FindX-2.5.8-fixture-setup.exe';$fault=if($case -in @('backup-failure','running253-backup-failure')){'backup'}elseif($case -in @('health-rollback','new-service-health-rollback','running253-health-rollback')){'health'}elseif($case -eq 'platform-rollback'){'post-platform'}elseif($case -eq 'stop-timeout'){'stop-timeout'}elseif($case -in @('interrupted-recovery','running253-interrupted-recovery')){'interrupt-after-copy'}else{'none'}
  $args='/VERYSILENT /SUPPRESSMSGBOXES /NORESTART /NOICONS /COMPONENTS="main,clicfg" /TASKS="" /DIR="'+$app+'" /TXROOT="'+$tx+'" /TXDATA="'+$data+'" /TXNATIVE="'+$native+'" /TXLEGACY="'+$legacy+'" /TXFAULT='+$fault+' /LOG="'+(Join-Path $root 'setup.log')+'"'
  $group=Join-Path $root 'shortcuts group';$desktop=Join-Path $root 'shortcuts desktop'
  if($ownedVolumeImage){$args+=' /TXIMAGE="'+$ownedVolumeImage+'"'}
  $args+=' /TXGROUP="'+$group+'" /TXDESKTOP="'+$desktop+'"'
  if($case -in @('new-service-ipc','new-service-health-rollback','fresh-selected-no-index')){$args=$args.Replace('/COMPONENTS="main,clicfg"','/COMPONENTS="main,servicec,clicfg"')}
  $pathBefore=[Environment]::GetEnvironmentVariable('Path','Machine');$shortcutsBefore=@()
  if($case -eq 'platform-rollback'){
   $ownedUninstall='Registry::HKEY_LOCAL_MACHINE\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\FindXUpgradeTest-'+$service+'_is1'
   Assert (-not (Test-Path -LiteralPath $ownedUninstall)) 'Unique uninstall fixture already exists'
   $null=New-Item -Path $ownedUninstall
   $null=New-ItemProperty -LiteralPath $ownedUninstall -Name 'OwnedFixtureBinary' -Value ([byte[]](1,7,255)) -PropertyType Binary
   $null=New-Item -Path ($ownedUninstall+'\OwnedUnknownChild')
   $null=New-ItemProperty -LiteralPath ($ownedUninstall+'\OwnedUnknownChild') -Name 'OwnedFixtureValue' -Value '%TEMP%\preserve unknown' -PropertyType ExpandString
   foreach($dir in @($group,$desktop)){$null=[IO.Directory]::CreateDirectory($dir)}
   $target=(Resolve-Path -LiteralPath (Join-Path $app 'FindX.exe')).ProviderPath
   Test-FxWshShortcutSupport $root $target (Join-Path $root 'shortcut-com-diagnostic.json')
   foreach($link in @((Join-Path $group 'FindX.lnk'),(Join-Path $group '卸载 FindX.lnk'),(Join-Path $desktop 'FindX.lnk'))){
    New-FxNativeShortcutFixture $root $target $link
    $shortcutsBefore+=@{path=$link;hash=(Hash $link)}
   }
   $args=$args.Replace('/NOICONS ','').Replace('/TASKS=""','/TASKS="addpath,desktopicon"')
  }
  $process=Start-Process -FilePath $setup -ArgumentList $args -PassThru -Wait
  $expectedSuccess=$case -in @('fresh-no-service','published252-stopped-service','reentry-refusal','new-service-ipc','fresh-selected-no-index','running253-success')
  if($expectedSuccess){Assert ($process.ExitCode -eq 0) ('Expected success, got '+$process.ExitCode);Assert ((Get-Content -LiteralPath (Join-Path $tx 'journal.json') -Raw -Encoding UTF8 | ConvertFrom-Json).state -eq 'committed') 'No commit record'}else{Assert ($process.ExitCode -ne 0) 'Fault did not refuse package'}
  Assert ((Hash (Join-Path $native 'prefs.json')) -eq $oldConfig) 'Config changed'
  if($oldIndex -and -not $isRunningNew){$verifyOld=if($case -eq 'new-service-ipc'){Join-Path $tx 'backup\data\index.bin'}else{Join-Path $data 'index.bin'};Assert ((Hash $verifyOld) -eq $oldIndex) 'Original persisted index not preserved exactly'}elseif(-not $oldIndex){Assert (-not (Test-Path -LiteralPath (Join-Path $data 'index.bin'))) 'Installer fabricated an index'}
  if($case -in @('backup-failure','health-rollback','legacy-running-refusal','stop-timeout')){Assert ((Hash (Join-Path $app 'FindX.exe')) -eq $oldGui) 'Old GUI binary changed on refusal/rollback'}
  if($case -in @('health-rollback','new-service-health-rollback')){
   $j=Journal $tx
   Assert ($j.backupVerified -and $j.error -match 'Injected CI post-copy health failure') 'Did not reach intended health fault after verified backup'
   AssertGuiHealth $tx
   Assert (Test-Path -LiteralPath (Join-Path $tx 'index-health-0.log')) 'Current persisted index was never validated'
   Assert (Test-Path -LiteralPath (Join-Path $tx 'index-health-1.log')) 'Backup persisted index was never validated'
  }
  if($case -eq 'backup-failure'){$j=Journal $tx;Assert (-not $j.backupVerified -and $j.error -match 'Injected CI backup failure before any replacement') 'Not the intended pre-replacement backup refusal'}
  if($case -eq 'legacy-running-refusal'){
   $j=Journal $tx;Assert ($j.original.serviceRunning -and -not $j.backupVerified -and $j.error -match 'Legacy running service has no verified protocol2; refused BEFORE STOP') 'Legacy refusal occurred for an unrelated reason'
   Assert ((Get-Content -LiteralPath (Join-Path $tx 'reason.code') -Raw).Trim() -eq 'legacy-save-unconfirmed') 'Missing clear legacy refusal reason'
   Assert ([int](Get-CimInstance Win32_Service -Filter ("Name='"+$service+"'")).ProcessId -eq $originalServicePid) 'Legacy service was restarted/replaced'
  }
  if($case -eq 'stop-timeout'){
   $j=Journal $tx;$svc=Get-CimInstance Win32_Service -Filter ("Name='"+$service+"'")
   Assert ($j.original.serviceRunning -and -not $j.backupVerified -and $j.error -match 'Graceful stop timeout; no force-stop was attempted') 'Did not reach intended graceful-stop timeout'
   Assert ($svc.State -eq 'Stop Pending' -and [int]$svc.ProcessId -eq $originalServicePid) 'Timeout fixture was force-stopped/restarted'
   Assert ($j.state -ne 'rolled_back') 'Unresolved pending stop was falsely reported recovered'
  }
  if($case -eq 'health-rollback'){Assert (-not (Test-Path -LiteralPath ('Registry::HKEY_LOCAL_MACHINE\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\FindXUpgradeTest-'+$service+'_is1'))) 'Failed install left uninstall registration';Assert ((Get-Content -LiteralPath (Join-Path $tx 'journal.json') -Raw -Encoding UTF8 | ConvertFrom-Json).state -eq 'rolled_back') 'Rollback not confirmed';Assert (-not (Test-Path -LiteralPath (Join-Path $app 'resources\component-manifest.json'))) 'Candidate addition left active';Assert (Test-Path -LiteralPath (Join-Path $tx 'failed-new\app\resources\component-manifest.json')) 'Failed new evidence discarded'}
  if($expectedSuccess){Assert ((Hash (Join-Path $app 'FindX.exe')) -eq (Hash (Join-Path $CandidatePayload 'FindX.exe'))) 'Installed GUI not final candidate bytes';AssertGuiHealth $tx}
  if($case -eq 'new-service-ipc'){$created=$true;Assert ((Get-Service $service).Status -eq 'Running') 'Fresh real service not running';AssertIpcHealth $tx}
  if($case -eq 'new-service-health-rollback'){AssertIpcHealth $tx;Assert (-not (Get-Service $service -ErrorAction SilentlyContinue)) 'Created service remained after rollback';Assert ((Get-Content -LiteralPath (Join-Path $tx 'journal.json') -Raw -Encoding UTF8 | ConvertFrom-Json).state -eq 'rolled_back') 'Fresh service failure rollback incomplete'}
  if($case -eq 'fresh-selected-no-index'){$created=$true;Assert ((Get-Service $service).Status -eq 'Stopped') 'Uninitialized service was started';Assert ((Get-CimInstance Win32_Service -Filter ("Name='"+$service+"'")).StartMode -eq 'Manual') 'Uninitialized service would autostart';Assert (Test-Path -LiteralPath (Join-Path $tx 'service-uninitialized.flag')) 'Uninitialized status hidden'}
  if($case -eq 'platform-rollback'){
   Assert ([Environment]::GetEnvironmentVariable('Path','Machine') -ceq $pathBefore) 'Machine PATH not exactly restored'
   foreach($link in $shortcutsBefore){Assert ((Hash $link.path) -eq $link.hash) 'Old real shortcut not restored'}
   Assert ((Get-Content -LiteralPath (Join-Path $tx 'journal.json') -Raw -Encoding UTF8 | ConvertFrom-Json).state -eq 'rolled_back') 'Platform rollback not confirmed'
   Assert ((Get-Content -LiteralPath (Join-Path $tx 'journal.json') -Raw -Encoding UTF8 | ConvertFrom-Json).error -match 'after actual PATH') 'Never exercised post-PATH failure'
   $j=Journal $tx;Assert ($j.platform.addPath -and $j.platform.newPath -cne $j.platform.path.value -and @($j.platform.shortcuts).Count -eq 3) 'Selected PATH and all real shortcuts were not in transaction scope'
   AssertGuiHealth $tx;AssertPlatformRestored $j.platform
   $applied=Get-Content -LiteralPath (Join-Path $tx 'platform-applied.json') -Raw -Encoding UTF8 | ConvertFrom-Json
   Assert ($applied.path -ceq $j.platform.newPath) 'Fault happened before actual selected PATH mutation'
   foreach($link in $j.platform.shortcuts){$seen=@($applied.shortcuts | Where-Object {$_.path -eq $link.path});Assert ($seen.Count -eq 1 -and $seen[0].exists -and $seen[0].bytes -cne $link.bytes) 'Fault happened before actual shortcut replacement'}
   # Reentry through the REAL setup: same journal refuses, a fresh transaction
   # reaches the same selected-task fault again and restores the same old state.
   $again=Start-Process -FilePath $setup -ArgumentList $args -PassThru -Wait
   Assert ($again.ExitCode -ne 0 -and (Journal $tx).state -eq 'rolled_back') 'Rolled-back transaction was silently overwritten'
   $repeat=Join-Path $root 'tx-platform-repeat';$repeatArgs=$args.Replace(('/TXROOT="'+$tx+'"'),('/TXROOT="'+$repeat+'"'))
   $retry=Start-Process -FilePath $setup -ArgumentList $repeatArgs -PassThru -Wait
   $rj=Journal $repeat
   Assert ($retry.ExitCode -ne 0 -and $rj.state -eq 'rolled_back' -and $rj.error -match 'after actual PATH') 'Actual selected-task rollback reentry never reached intended fault'
   AssertGuiHealth $repeat;AssertPlatformRestored $j.platform

  }
  if($case -eq 'published252-stopped-service'){Assert ((Get-Service $service).Status -eq 'Stopped') 'Previously stopped service was started'}
  if($case -eq 'legacy-running-refusal'){Assert ((Get-Service $service).Status -eq 'Running') 'Old running service not restored'}
  if($case -eq 'reentry-refusal'){$again=Start-Process -FilePath $setup -ArgumentList $args -PassThru -Wait;Assert ($again.ExitCode -ne 0) 'Committed transaction reused implicitly'}
  if($case -in @('interrupted-recovery','running253-interrupted-recovery')){
   Assert ($process.ExitCode -eq 73 -and (Journal $tx).state -eq 'backup_verified') 'Actual installer did not interrupt at the intended post-copy boundary'
   Assert ((Hash (Join-Path $app 'FindX.exe')) -eq (Hash (Join-Path $CandidatePayload 'FindX.exe'))) 'Post-copy interrupt happened before final payload copy'

   $recovery=Join-Path $root 'tx-recovery';$resumeArgs=$args.Replace(('/TXROOT="'+$tx+'"'),('/TXROOT="'+$recovery+'"')).Replace('/TXFAULT=interrupt-after-copy','/TXFAULT=none')
   $resumed=Start-Process -FilePath $setup -ArgumentList $resumeArgs -PassThru -Wait
   Assert ($resumed.ExitCode -ne 0) 'Recovery must not claim a new installation'
   Assert ((Get-Content -LiteralPath (Join-Path $tx 'journal.json') -Raw -Encoding UTF8 | ConvertFrom-Json).state -eq 'rolled_back') 'Interrupted actual installer was not recovered'
   Assert ((Hash (Join-Path $app 'FindX.exe')) -eq $oldGui) 'Interrupted binary set not restored'
   Assert (-not (Test-Path -LiteralPath (Join-Path $app 'resources\component-manifest.json'))) 'Interrupted candidate addition remained active'
   Assert ((Hash (Join-Path $data 'index.bin')) -eq $oldIndex) 'Interrupted index not restored'
   $finalArgs=$resumeArgs.Replace(('/TXROOT="'+$recovery+'"'),('/TXROOT="'+(Join-Path $root 'tx-final')+'"'))
   $final=Start-Process -FilePath $setup -ArgumentList $finalArgs -PassThru -Wait
   Assert ($final.ExitCode -eq 0) 'Healthy retry after verified recovery failed';AssertGuiHealth (Join-Path $root 'tx-final')
  }
  if($isRunningNew){
   $j=Journal $tx
   if($case -eq 'running253-save-refusal'){
    Assert (-not $j.backupVerified -and $j.error -match 'Real prepare refused:.*checkpoint writer:.*(denied|Access|os error 5)') 'Save refusal did not reach actual OS writer failure'
    $svc=Get-CimInstance Win32_Service -Filter ("Name='"+$service+"'");Assert ($svc.State -eq 'Running' -and $svc.ProcessId -eq $originalServicePid) 'Failed prepare destroyed/restarted original RAM owner'
    # Permission repair is not a durability result: the real periodic writer
    # deliberately backs off five seconds on IO failure. Do not feed a new event
    # or bypass its retained error. Observe same-instance, fresh disk recovery.
    $repairImage=Join-Path $app 'findx2-service.exe';$repairIndex=Join-Path $data 'index.bin'
    $repairOwner=Assert-FxTxReceiptService $service $pipe $repairImage $repairIndex $originalServicePid
    $repairIdentity=Invoke-FxTxManagement $service $pipe $repairImage $repairIndex $repairOwner @{type='checkpoint_identity';protocol_version=2} 2000
    $beforeRepair=(Get-Content (Join-Path $saveV6 'published.json') -Raw|ConvertFrom-Json).manifest.generation
    Set-Acl $saveV6 $saveAcl;$saveAcl=$null
    $repairTimer=[Diagnostics.Stopwatch]::StartNew();$recovered=$false;$repairLast='';$repairGeneration=$beforeRepair
    while($repairTimer.Elapsed.TotalSeconds -lt 10){
     try{
      $repairCap=Invoke-FxTxManagement $service $pipe $repairImage $repairIndex $repairOwner @{type='upgrade_capabilities';protocol_version=2} 2000
      $repairNow=Invoke-FxTxManagement $service $pipe $repairImage $repairIndex $repairOwner @{type='checkpoint_identity';protocol_version=2} 2000
      Assert ($repairNow.engine_instance -ceq $repairIdentity.engine_instance -and $repairNow.service_pid -eq $originalServicePid) 'RAM owner changed during actual IO repair'
      $repairGeneration=(Get-Content (Join-Path $saveV6 'published.json') -Raw|ConvertFrom-Json).manifest.generation
      if($null -eq $repairCap.runtime_error -and $null -eq $repairCap.last_checkpoint_error -and $repairGeneration -gt $beforeRepair){Assert-FxTxUpgradeCapabilities $repairCap;$recovered=$true;break}
      $repairLast=$repairCap|ConvertTo-Json -Compress -Depth 10
     }catch{$repairLast=$_.Exception.Message}
     [Threading.Thread]::Sleep(200)
    }
    $observation=@{passed=$recovered;beforeGeneration=$beforeRepair;afterGeneration=$repairGeneration;engineInstance=$repairIdentity.engine_instance;owner=$repairOwner;seconds=$repairTimer.Elapsed.TotalSeconds;last=$repairLast;extraEvents=$false}
    $observation|ConvertTo-Json -Depth 12|Set-Content (Join-Path $root 'save-repair-observed.json')
    Assert $recovered ('Actual idle writer did not repair retained same-instance failure within bounded10s: '+$repairLast)
    $retryTx=Join-Path $root 'tx-save-retry';$retryArgs=$args.Replace(('/TXROOT="'+$tx+'"'),('/TXROOT="'+$retryTx+'"'))
    $savedRetry=Start-Process -FilePath $setup -ArgumentList $retryArgs -PassThru -Wait
    $rj=Journal $retryTx
    Write-Host ('SAVE_RETRY_ACTUAL_EXIT='+$savedRetry.ExitCode+' JOURNAL='+($rj|ConvertTo-Json -Compress -Depth 20))
    Assert ($savedRetry.ExitCode -eq 0 -and $rj.state -eq 'committed' -and $rj.backupVerified -and $rj.terminalHandoff.stage -eq 'stopped' -and $rj.terminalHandoff.offlineBeforeReplay) 'Real installer retry after OS write repair never completed durable handoff/backup/commit'
    AssertGuiHealth $retryTx;AssertIpcHealth $retryTx
   }else{
    Assert ($j.terminalHandoff -and $j.terminalHandoff.stage -eq 'stopped' -and $j.terminalHandoff.offlineBeforeReplay -and $j.terminalHandoff.token.published_generation -gt 0) 'Not an actual bound receipt/STOP/offline chain'
    if($case -eq 'running253-backup-failure'){Assert (-not $j.backupVerified -and $j.error -match 'Injected CI backup failure before any replacement' -and $j.state -eq 'rolled_back') 'Wrong backup refusal phase'}
    if($case -in @('running253-success','running253-health-rollback','running253-interrupted-recovery')){Assert $j.backupVerified 'No complete verified backup'}
    if($case -eq 'running253-copy-verification-rollback'){Assert ($j.backupVerified -and $j.state -eq 'rolled_back' -and $j.error -match 'Payload hash mismatch: resources[\\/]component-manifest.json' -and -not $j.candidateServiceStarted) 'Copied payload failed before intended integrity phase or candidate writer started prematurely';Assert (Test-Path (Join-Path $tx 'failed-new\app\resources\component-manifest.json')) 'Bad copied payload evidence lost'}
    if($case -eq 'running253-health-rollback'){Assert ($j.error -match 'Injected CI post-copy health failure' -and $j.state -eq 'rolled_back') 'Wrong post-copy health rollback';AssertGuiHealth $tx;AssertIpcHealth $tx;Assert (Test-Path (Join-Path $tx 'candidate-rollback-stop\installer-terminal-receipt.json')) 'Candidate was not durably stopped before old-set restoration'}
    $svc=Get-CimInstance Win32_Service -Filter ("Name='"+$service+"'");Assert ($svc.State -eq 'Running' -and $svc.StartMode -eq 'Manual') 'Original running/manual state not restored'
    if($case -eq 'running253-success'){AssertIpcHealth $tx;Assert ($svc.ProcessId -ne $originalServicePid) 'Candidate never actually restarted'}
   }
  }
  $rows.Add([ordered]@{case=$case;passed=$true;exit=$process.ExitCode;root=$root})
 }catch{$rows.Add([ordered]@{case=$case;passed=$false;error=$_.Exception.ToString();position=$_.InvocationInfo.PositionMessage;scriptStackTrace=$_.ScriptStackTrace;root=$root})}
 finally{
  if($saveAcl -and $saveV6){Set-Acl $saveV6 $saveAcl;$saveAcl=$null}
  # Only test-owned unique resource cleanup; release a deliberate stall normally.
  $hold=Join-Path $data 'index.bin.hold-stop';if(Test-Path -LiteralPath $hold){[IO.File]::Delete($hold)}
  if(Get-Service $service -ErrorAction SilentlyContinue){$created=$true}
  if($ownedUninstall){Remove-Item -LiteralPath $ownedUninstall -Recurse -Force}
  try{
   if($created){$controller=[ServiceProcess.ServiceController]::new($service);try{
    $controller.Refresh()
    if($controller.Status -ne [ServiceProcess.ServiceControllerStatus]::Stopped){
     if($controller.Status -ne [ServiceProcess.ServiceControllerStatus]::StopPending){$controller.Stop()}
     $controller.WaitForStatus([ServiceProcess.ServiceControllerStatus]::Stopped,[TimeSpan]::FromSeconds(20))
    }
    & "$env:SystemRoot\System32\sc.exe" delete $service | Out-Null
    if($LASTEXITCODE){throw 'Owned service deletion failed'}
   }finally{$controller.Dispose()}}
   if($ownedVolumeImage -and (Test-Path -LiteralPath $ownedVolumeImage)){
    $until=[DateTime]::UtcNow.AddSeconds(10)
    do{$writers=@(Get-CimInstance Win32_Process -Filter "Name='findx2-service.exe'" | Where-Object {$_.ExecutablePath -ieq (Join-Path $app 'findx2-service.exe')});if(-not $writers.Count){break};Start-Sleep -Milliseconds 100}while([DateTime]::UtcNow -lt $until)
    if($writers.Count){throw 'Owned engine still alive; volume retained, never detached underneath it'}
    $image=Get-DiskImage -ImagePath $ownedVolumeImage -ErrorAction Stop
    if($image.Attached){Dismount-DiskImage -ImagePath $ownedVolumeImage -ErrorAction Stop}
   }
  }catch{
   $rows[$rows.Count-1]['passed']=$false;$rows[$rows.Count-1]['cleanupError']=$_.Exception.ToString()
  }
  Write-NativeResults
  Write-Output ('NATIVE_CASE_END='+$case+' passed='+$rows[$rows.Count-1]['passed'])
 }
}
Write-NativeResults
Write-Output ('NATIVE_INSTALLER_EVIDENCE='+$base)
if(@($rows | Where-Object{-not $_.passed}).Count){throw 'Native package integration matrix failed; see retained evidence'}

param([Parameter(Mandatory=$true)][string]$ServiceExe)
Set-StrictMode -Version Latest
$ErrorActionPreference='Stop'
if($env:GITHUB_ACTIONS -ne 'true' -or $env:RUNNER_OS -ne 'Windows' -or $env:COMPUTERNAME -eq 'MZ' -or $env:USERPROFILE -eq 'C:\Users\chaoj'){throw 'Disposable Windows CI ONLY; never MZ'}
if(-not ([Security.Principal.WindowsPrincipal]::new([Security.Principal.WindowsIdentity]::GetCurrent())).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)){throw 'Existing CI SCM rights required; no UAC'}
$repo=[IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$ServiceExe=[IO.Path]::GetFullPath($ServiceExe)
if(-not $ServiceExe.StartsWith(($repo+'\'),[StringComparison]::OrdinalIgnoreCase) -or -not [IO.File]::Exists($ServiceExe)){throw 'Service must be built from this exact CI checkout'}
$name='FindX2Search'
if(Get-Service -Name $name -ErrorAction SilentlyContinue){throw 'Service already exists; refuse to touch any pre-existing registration'}
Import-Module (Join-Path $PSScriptRoot 'test-owned-ntfs-volume.psm1') -Force
Import-Module (Join-Path $repo 'installer\transaction-platform.psm1') -Force
$base=Join-Path $env:RUNNER_TEMP ('FindXUpgradeTest_'+[guid]::NewGuid().ToString('N'))
$null=[IO.Directory]::CreateDirectory($base)
$artifact=Join-Path $repo 'native-runtime-evidence'
$null=[IO.Directory]::CreateDirectory($artifact)
$rows=[Collections.Generic.List[object]]::new()
$owned=$null;$created=$false;$originalAcl=$null;$v6=$null
function Assert($ok,[string]$why){if(-not $ok){throw $why}}
function Wait-Condition([scriptblock]$Test,[int]$Seconds,[string]$Why){
 $timer=[Diagnostics.Stopwatch]::StartNew();$last=$null
 while($timer.Elapsed.TotalSeconds -lt $Seconds){try{if(& $Test){return}}catch{$last=$_.Exception.Message};[Threading.Thread]::Sleep(200)}
 throw ($Why+'; last='+$last)
}
function Service-State {return Get-CimInstance Win32_Service -Filter "Name='$name'"}
function Assert-OwnedService {
 $s=Service-State
 Assert ($s -and $s.PathName.Contains($base) -and $s.PathName.Contains($ServiceExe)) 'Service identity no longer owned by this fixture'
 return $s
}
function Rpc($request){
 $s=Assert-OwnedService
 Assert ($s.ProcessId -gt 0) 'No actual SCM process'
 $pipe=[IO.Pipes.NamedPipeClientStream]::new('.',$script:pipeName,[IO.Pipes.PipeDirection]::InOut,[IO.Pipes.PipeOptions]::Asynchronous)
 $reader=$null
 try{
  $pipe.Connect(1000);[uint32]$server=0
  Assert ([FxTxProcessNative]::GetNamedPipeServerProcessId($pipe.SafePipeHandle.DangerousGetHandle(),[ref]$server) -and $server -eq $s.ProcessId) 'RPC not owned by current SCM process'
  $bytes=[Text.Encoding]::UTF8.GetBytes(($request|ConvertTo-Json -Compress -Depth 10)+"`n");$pipe.Write($bytes,0,$bytes.Length);$pipe.Flush()
  $reader=[IO.StreamReader]::new($pipe,[Text.Encoding]::UTF8,$false,4096,$true);$task=$reader.ReadLineAsync()
  Assert ($task.Wait(1500)) 'RPC bounded timeout';$line=$task.Result
  Assert ($line -and $line.Length -lt 1048576) 'Invalid bounded IPC line'
  return $line|ConvertFrom-Json
 }finally{if($reader){$reader.Dispose()};$pipe.Dispose()}
}
function Ready {
 $status=Rpc @{type='status'}
 $status|ConvertTo-Json -Depth 10|Set-Content -Encoding UTF8 (Join-Path $artifact 'latest-status.json')
 $cap=Rpc @{type='upgrade_capabilities';protocol_version=1};$cap|ConvertTo-Json -Depth 10|Set-Content -Encoding UTF8 (Join-Path $artifact 'latest-capabilities.json')
 return ($status.type -eq 'status_result' -and -not $status.loading -and $status.entry_count -gt 0 -and $status.healthy -and $status.metadata_ready -and -not $status.watch_error -and -not $status.backfill_error -and -not $cap.runtime_error)
}
function Has-File([string]$File,[long]$Size){
 $r=Rpc @{type='search';query=$File;pinyin=$false;limit=100;offset=0}
 $r|ConvertTo-Json -Depth 10|Set-Content -Encoding UTF8 (Join-Path $artifact 'latest-search.json')
 return (@($r.hits|Where-Object {$_.name -ceq $File -and $_.size -eq $Size -and $_.path.StartsWith($owned.Root,[StringComparison]::OrdinalIgnoreCase)}).Count -eq 1)
}
function Stop-Owned {
 $null=Assert-OwnedService;$controller=Get-Service $name
 if($controller.Status -ne 'Stopped' -and $controller.Status -ne 'StopPending'){$controller.Stop()}
 $controller.WaitForStatus([ServiceProcess.ServiceControllerStatus]::Stopped,[TimeSpan]::FromSeconds(60))
 return Service-State
}
try{
 $owned=New-FxOwnedTestVolume (Join-Path $base 'owned-health.vhd') ('FXTX'+[guid]::NewGuid().ToString('N').Substring(0,8).ToUpperInvariant())
 Copy-Item (Join-Path $base 'owned-volume.json') $artifact
 # This is a REAL initial native scan and metadata backfill, with native USN watch enabled.
 $null=[IO.Directory]::CreateDirectory((Join-Path $owned.Root 'native-runtime'))
 [IO.File]::WriteAllText((Join-Path $owned.Root 'native-runtime\initial-native-proof.txt'),'native-initial-123',[Text.Encoding]::ASCII)
 $index=Join-Path $base 'index.bin';$v6=$index+'.v6';$script:pipeName='findx253native'+[guid]::NewGuid().ToString('N')
 $command='"'+$ServiceExe+'" --service --index "'+$index+'" --volume '+$owned.Volume+' --pipe '+$pipeName+' --no-everything-ipc --usage-memory-mib 64 --save-interval-secs 1'
 Assert ($command -notmatch '--no-watch|--no-backfill|--fixture-root|--fixture-events') 'Native acceptance cannot use fixture bypasses'
 $null=New-Service -Name $name -BinaryPathName $command -StartupType Manual;$created=$true
 $serviceKey='HKLM:\SYSTEM\CurrentControlSet\Services\'+$name
 $null=New-ItemProperty -Path $serviceKey -Name Environment -PropertyType MultiString -Value @('ProgramData='+(Join-Path $base 'programdata'))
 Start-Service $name
 Wait-Condition {Ready} 120 'Actual native whole-volume load/watch/backfill never became ready'
 $first=Assert-OwnedService
 Assert ($first.StartName -eq 'LocalSystem') 'OS ACL fault must target the actual SCM account'
 $caps=Rpc @{type='upgrade_capabilities';protocol_version=1}
 Assert ($caps.type -eq 'upgrade_capabilities_result' -and -not $caps.durable_prepare) 'Unproved positive ACK must remain rejected'
 $caps|ConvertTo-Json -Depth 10|Set-Content (Join-Path $artifact 'capabilities-not-ack.json')
 $rows.Add(@{test='native-scm-vhd-load-watch-backfill';passed=$true;pid=$first.ProcessId;noWatch=$false;noBackfill=$false;fixture=$false})
 # Deny only the exact owned VHD-index directory to LOCAL SYSTEM. No injected fake writer.
 Wait-Condition {Test-Path -LiteralPath (Join-Path $v6 'published.json')} 30 'No actual published generation'
 $before=Get-Content (Join-Path $v6 'published.json') -Raw|ConvertFrom-Json
 $originalAcl=Get-Acl -LiteralPath $v6;$denied=Get-Acl -LiteralPath $v6
 $system=[Security.Principal.SecurityIdentifier]::new('S-1-5-18')
 $deny=[Security.AccessControl.FileSystemAccessRule]::new($system,[Security.AccessControl.FileSystemRights]'Write,Delete,DeleteSubdirectoriesAndFiles',[Security.AccessControl.InheritanceFlags]'ContainerInherit,ObjectInherit',[Security.AccessControl.PropagationFlags]::None,[Security.AccessControl.AccessControlType]::Deny)
 $denied.AddAccessRule($deny);Set-Acl -LiteralPath $v6 -AclObject $denied
 $file='actual-os-denied-write-proof.txt';$body='native-ram-durable-retry-253'
 [IO.File]::WriteAllText((Join-Path $owned.Root ('native-runtime\'+$file)),$body,[Text.Encoding]::ASCII)
 Wait-Condition {Has-File $file $body.Length} 45 'Real USN/stat never committed expected RAM file; do not claim writer fault reached'
 Wait-Condition {$cap=Rpc @{type='upgrade_capabilities';protocol_version=1};$cap|ConvertTo-Json -Depth 10|Set-Content (Join-Path $artifact 'actual-os-writer-error.json');return ($cap.last_checkpoint_error -match 'denied|拒绝|os error 5|Access')} 30 'Did not reach actual OS writer access failure'
 $identity=Assert-OwnedService;$controller=Get-Service $name;$controller.Stop()
 Wait-Condition {(Service-State).State -eq 'Stop Pending'} 8 'Save failure did not retain SCM StopPending'
 [Threading.Thread]::Sleep(1500)
 $pending=Assert-OwnedService
 Assert ($pending.State -eq 'Stop Pending' -and $pending.ProcessId -eq $identity.ProcessId) 'Writer error was turned into successful exit or process replacement'
 $pending|Select-Object Name,State,ProcessId,ExitCode,ServiceSpecificExitCode|ConvertTo-Json|Set-Content (Join-Path $artifact 'save-failure-stop-pending.json')
 Set-Acl -LiteralPath $v6 -AclObject $originalAcl;$originalAcl=$null
 $stopped=Stop-Owned
 Assert ($stopped.ExitCode -eq 0 -and $stopped.ServiceSpecificExitCode -eq 0) 'Recovered actual save did not stop cleanly'
 $after=Get-Content (Join-Path $v6 'published.json') -Raw|ConvertFrom-Json
 Assert ($after.manifest.generation -ge $before.manifest.generation) 'Disk generation regressed'
 $after|ConvertTo-Json -Depth 20|Set-Content (Join-Path $artifact 'recovered-published-generation.json')
 # Verify actual DISK metadata BEFORE restarting: USN replay cannot repair a bad save into a false PASS.
 $verify=Join-Path $repo 'target\debug\examples\verify_persisted_file.exe'
 & $verify $index (Join-Path $owned.Root ('native-runtime\'+$file)) $body.Length $after.manifest.generation | Set-Content (Join-Path $artifact 'offline-no-replay-disk-proof.json')
 Assert ($LASTEXITCODE -eq 0) 'Actual saved generation lacks expected RAM mutation; restart/replay cannot count as durability'

 Start-Service $name
 Wait-Condition {Ready} 90 'Restart from saved native index not ready'
 Wait-Condition {Has-File $file $body.Length} 30 'RAM mutation not recovered from actual durable save'
 $restarted=Assert-OwnedService
 Assert ($restarted.ProcessId -ne $identity.ProcessId) 'Did not actually restart a service instance'
 $rows.Add(@{test='actual-os-write-failure-stop-pending-retry-native-recovery';passed=$true;beforeGeneration=$before.manifest.generation;afterGeneration=$after.manifest.generation;oldPid=$identity.ProcessId;newPid=$restarted.ProcessId;expectedBytes=$body.Length;upgradeAck=$false})
 $null=Stop-Owned
 # Actual SCM error propagation, explicitly a corrupt-index LOAD failure, not a fake save-failure claim.
 $bad=Join-Path $base 'corrupt-owned-index.bin';[IO.File]::WriteAllText($bad,'invalid-private-index',[Text.Encoding]::ASCII)
 $badCommand=$command.Replace($index,$bad)
 & sc.exe config $name binPath= $badCommand|Out-File (Join-Path $artifact 'configure-owned-corrupt-index.log')
 Assert ($LASTEXITCODE -eq 0) 'Could not configure owned service corrupt-index phase'
 try{Start-Service $name}catch{$_|Out-String|Set-Content (Join-Path $artifact 'expected-start-error.txt')}
 Wait-Condition {$s=Service-State;return ($s.State -eq 'Stopped' -and ($s.ExitCode -ne 0 -or $s.ServiceSpecificExitCode -ne 0))} 30 'Actual runtime load error never reached nonzero SCM state'
 $failed=Assert-OwnedService
 $failed|Select-Object Name,State,ProcessId,ExitCode,ServiceSpecificExitCode|ConvertTo-Json|Set-Content (Join-Path $artifact 'actual-scm-load-error.json')
 $rows.Add(@{test='actual-corrupt-index-load-error-to-scm';passed=$true;win32=$failed.ExitCode;serviceSpecific=$failed.ServiceSpecificExitCode;scope='load failure, NOT successful final RAM save or save-failure exit'})
}catch{
 $rows.Add(@{test='native-runtime-current-stage';passed=$false;error=$_.Exception.Message})
 throw
}finally{
 if($originalAcl -and $v6){Set-Acl -LiteralPath $v6 -AclObject $originalAcl}
 $safeDetach=$true
 if($created){
  try{$null=Stop-Owned;& sc.exe delete $name|Out-File (Join-Path $artifact 'delete-owned-service.log');Assert ($LASTEXITCODE -eq 0) 'Owned service deletion failed'}catch{$safeDetach=$false;$rows.Add(@{test='cleanup';passed=$false;error=$_.Exception.Message;forceUsed=$false})}
 }
 if($owned -and $safeDetach){Dismount-DiskImage -ImagePath $owned.Image -ErrorAction Stop}
 if(Test-Path (Join-Path $base 'programdata\FindX\service-win.log')){Copy-Item (Join-Path $base 'programdata\FindX\service-win.log') $artifact}
 $rows|ConvertTo-Json -Depth 20|Set-Content -Encoding UTF8 (Join-Path $artifact 'results.json')
 if(-not $safeDetach){throw 'Safe cleanup could not finish; no forced stop/detach attempted'}
}

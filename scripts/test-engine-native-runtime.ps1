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
Import-Module (Join-Path $repo 'installer\transaction-files.psm1') -Force
Import-Module (Join-Path $repo 'installer\transaction-receipt.psm1') -Force
$base=Join-Path $env:RUNNER_TEMP ('FindXUpgradeTest_'+[guid]::NewGuid().ToString('N'))
$null=[IO.Directory]::CreateDirectory($base)
$artifact=Join-Path $repo 'native-runtime-evidence'
$null=[IO.Directory]::CreateDirectory($artifact)
$rows=[Collections.Generic.List[object]]::new()
$owned=$null;$created=$false;$originalAcl=$null;$v6=$null;$receiptAcl=$null;$receiptFile=$null;$script:stage='initializing'
function Stage([string]$Name){$script:stage=$Name;[ordered]@{stage=$Name;time=(Get-Date -Format o)}|ConvertTo-Json|Set-Content -Encoding UTF8 (Join-Path $artifact 'stage.json');Write-Host ('NATIVE STAGE: '+$Name)}
function Capture-NativeDiagnostics {
 if($created){try{Service-State|Select-Object Name,State,ProcessId,ExitCode,ServiceSpecificExitCode,PathName,StartName|ConvertTo-Json|Set-Content -Encoding UTF8 (Join-Path $artifact 'last-owned-scm-state.json')}catch{}}
 foreach($name in @('latest-status.json','latest-capabilities.json','initial-index-state.json','last-owned-scm-state.json','stage.json')){
  $path=Join-Path $artifact $name;if(Test-Path -LiteralPath $path){Write-Host ('OWNED DIAGNOSTIC '+$name);Write-Host ([IO.File]::ReadAllText($path))}
 }
 $log=Join-Path $base 'programdata\FindX\service-win.log';if(Test-Path -LiteralPath $log){Copy-Item -LiteralPath $log -Destination $artifact -Force;Write-Host 'OWNED SERVICE LOG';Write-Host ([IO.File]::ReadAllText($log))}
}
function Assert($ok,[string]$why){if(-not $ok){throw $why}}
function Wait-Condition([scriptblock]$Test,[int]$Seconds,[string]$Why){
 $timer=[Diagnostics.Stopwatch]::StartNew();$last=$null
 while($timer.Elapsed.TotalSeconds -lt $Seconds){try{if(& $Test){return}}catch{$last=$_.Exception.Message;if($last.StartsWith('NativeTerminal:')){throw}};[Threading.Thread]::Sleep(200)}
 throw ($Why+'; last='+$last)
}
function Service-State {return Get-CimInstance Win32_Service -Filter "Name='$name'"}
function Assert-OwnedService {
 $s=Service-State
 Assert ($s -and $s.PathName.Contains($base) -and $s.PathName.Contains($ServiceExe)) 'Service identity no longer owned by this fixture'
 return $s
}
function Rpc($request,[Security.Principal.TokenImpersonationLevel]$Level=[Security.Principal.TokenImpersonationLevel]::Impersonation){
 $s=Assert-OwnedService
 Assert ($s.ProcessId -gt 0) 'No actual SCM process'
 $pipe=[IO.Pipes.NamedPipeClientStream]::new('.',$script:pipeName,[IO.Pipes.PipeDirection]::InOut,[IO.Pipes.PipeOptions]::Asynchronous,$Level)
 $reader=$null
 try{
  $pipe.Connect(1000);[uint32]$server=0
  Assert ([FxTxProcessNative]::GetNamedPipeServerProcessId($pipe.SafePipeHandle.DangerousGetHandle(),[ref]$server) -and $server -eq $s.ProcessId) 'RPC not owned by current SCM process'
  $bytes=[Text.Encoding]::UTF8.GetBytes(($request|ConvertTo-Json -Compress -Depth 10)+"`n");$pipe.Write($bytes,0,$bytes.Length);$pipe.Flush()
  $reader=[IO.StreamReader]::new($pipe,[Text.Encoding]::UTF8,$false,4096,$true);$task=$reader.ReadLineAsync()
  $rpcMs=if($request.type -eq 'prepare_stop'){[int]$request.timeout_ms+5000}elseif($request.type -eq 'commit_stop'){10000}else{1500}
  Assert ($task.Wait($rpcMs)) 'RPC bounded timeout';$line=$task.Result
  Assert ($line -and $line.Length -lt 1048576) 'Invalid bounded IPC line'
  return $line|ConvertFrom-Json
 }finally{if($reader){$reader.Dispose()};$pipe.Dispose()}
}
function Ready {
 $service=Assert-OwnedService
 if($service.State -eq 'Stopped'){throw ('NativeTerminal: owned SCM worker stopped before readiness; Win32='+$service.ExitCode+' serviceSpecific='+$service.ServiceSpecificExitCode)}
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
 Stage 'create-attested-owned-vhd'
 $owned=New-FxOwnedTestVolume (Join-Path $base 'owned-health.vhd') ('FXTX'+[guid]::NewGuid().ToString('N').Substring(0,8).ToUpperInvariant())
 Copy-Item (Join-Path $base 'owned-volume.json') $artifact
 # This is a REAL initial native scan and metadata backfill, with native USN watch enabled.
 $null=[IO.Directory]::CreateDirectory((Join-Path $owned.Root 'native-runtime'))
 $initialBody='native-initial-123';[IO.File]::WriteAllText((Join-Path $owned.Root 'native-runtime\initial-native-proof.txt'),$initialBody,[Text.Encoding]::ASCII)
 # The service intentionally REFUSES a missing index; preserve that product safety behavior.
 # Provision only this attested tiny VHD through the real supported native CLI, never fixture_root.
 Stage 'native-fast-index-provisioning'
 $index=Join-Path $base 'index.bin';$v6=$index+'.v6'
 $cli=Join-Path $repo 'target\debug\findx2.exe';Assert ([IO.File]::Exists($cli)) 'Actual native indexing CLI not built'
 & $cli index --volume $owned.Volume --output $index --max-scan-threads 1 *> (Join-Path $artifact 'native-initial-index.log')
 Assert ($LASTEXITCODE -eq 0 -and [IO.File]::Exists($index)) 'Actual CLI failed to provision owned VHD index'
 $verify=Join-Path $repo 'target\debug\examples\verify_persisted_file.exe'
 $initial=& $verify --inspect-index $index
 Assert ($LASTEXITCODE -eq 0) 'Cannot inspect actual native initial index'
 $initial|Set-Content -Encoding UTF8 (Join-Path $artifact 'initial-index-state.json');$initialState=$initial|ConvertFrom-Json
 Assert (-not $initialState.metadata_ready -and $initialState.entries -gt 0 -and @($initialState.volumes).Count -eq 1 -and $initialState.volumes[0].letter -ceq $owned.Volume.Substring(0,1) -and $initialState.volumes[0].root_prefix -ceq $owned.Root) 'Initial native index scope/fast-metadata state wrong; cannot claim service backfill'
 Stage 'actual-scm-start-and-native-backfill'
 $script:pipeName='findx253native'+[guid]::NewGuid().ToString('N')
 $command='"'+$ServiceExe+'" --service --index "'+$index+'" --volume '+$owned.Volume+' --pipe '+$pipeName+' --no-everything-ipc --usage-memory-mib 64 --save-interval-secs 1'
 Assert ($command -notmatch '--no-watch|--no-backfill|--fixture-root|--fixture-events') 'Native acceptance cannot use fixture bypasses'
 $null=New-Service -Name $name -BinaryPathName $command -StartupType Manual;$created=$true
 $serviceKey='HKLM:\SYSTEM\CurrentControlSet\Services\'+$name
 $null=New-ItemProperty -Path $serviceKey -Name Environment -PropertyType MultiString -Value @('ProgramData='+(Join-Path $base 'programdata'))
 Start-Service $name
 Wait-Condition {Ready} 120 'Actual native whole-volume load/watch/backfill never became ready'
 Stage 'verify-native-seed-backfill'
 Wait-Condition {Has-File 'initial-native-proof.txt' $initialBody.Length} 30 'Actual initial native seed metadata not backfilled; readiness flag alone is insufficient'
 $first=Assert-OwnedService
 Assert ($first.StartName -eq 'LocalSystem') 'OS ACL fault must target the actual SCM account'
 $caps=Rpc @{type='upgrade_capabilities';protocol_version=1}
 Assert ($caps.type -eq 'upgrade_capabilities_result' -and -not $caps.durable_prepare) 'Unproved positive ACK must remain rejected'
 $caps|ConvertTo-Json -Depth 10|Set-Content (Join-Path $artifact 'capabilities-not-ack.json')
 $rows.Add(@{test='native-scm-vhd-load-watch-backfill';passed=$true;pid=$first.ProcessId;noWatch=$false;noBackfill=$false;fixture=$false})
 Stage 'actual-os-write-denial'
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
 Stage 'actual-save-failure-stop-pending'
 $identity=Assert-OwnedService;$controller=Get-Service $name;$controller.Stop()
 Wait-Condition {(Service-State).State -eq 'Stop Pending'} 8 'Save failure did not retain SCM StopPending'
 [Threading.Thread]::Sleep(1500)
 $pending=Assert-OwnedService
 Assert ($pending.State -eq 'Stop Pending' -and $pending.ProcessId -eq $identity.ProcessId) 'Writer error was turned into successful exit or process replacement'
 $pending|Select-Object Name,State,ProcessId,ExitCode,ServiceSpecificExitCode|ConvertTo-Json|Set-Content (Join-Path $artifact 'save-failure-stop-pending.json')
 Stage 'restore-acl-and-real-save-retry'
 Set-Acl -LiteralPath $v6 -AclObject $originalAcl;$originalAcl=$null
 $stopped=Stop-Owned
 Assert ($stopped.ExitCode -eq 0 -and $stopped.ServiceSpecificExitCode -eq 0) 'Recovered actual save did not stop cleanly'
 $after=Get-Content (Join-Path $v6 'published.json') -Raw|ConvertFrom-Json
 Assert ($after.manifest.generation -ge $before.manifest.generation) 'Disk generation regressed'
 $after|ConvertTo-Json -Depth 20|Set-Content (Join-Path $artifact 'recovered-published-generation.json')
 Stage 'offline-disk-proof-before-restart'
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
 Stage 'protocol2-actual-writer-prepare-failure'
 Assert ((Rpc @{type='checkpoint_identity';protocol_version=2} ([Security.Principal.TokenImpersonationLevel]::Anonymous)).type -eq 'error') 'Anonymous pipe frame obtained elevated management'
 $id=Rpc @{type='checkpoint_identity';protocol_version=2}
 Assert ($id.type -eq 'checkpoint_identity_result' -and $id.service_pid -eq (Service-State).ProcessId) 'No authenticated current-instance identity'
 function Prepare([string]$Nonce,[int]$Ms=30000){return Rpc @{type='prepare_stop';protocol_version=2;expected_instance=$id.engine_instance;request_nonce=$Nonce;index_identity=$id.index_identity;timeout_ms=$Ms}}
 # No new mutation/persister is needed: freeze always invokes the actual full writer.
 $originalAcl=Get-Acl -LiteralPath $v6;$denied=Get-Acl -LiteralPath $v6;$denied.AddAccessRule($deny);Set-Acl -LiteralPath $v6 -AclObject $denied
 $failure=Prepare ([guid]::NewGuid().ToString('N'))
 $failure|ConvertTo-Json -Depth 20|Set-Content (Join-Path $artifact 'prepare-actual-writer-failure.json')
 Assert ($failure.type -eq 'error' -and $failure.message -match 'checkpoint writer:.*(denied|拒绝|os error 5|Access)') 'Prepare failure was not the actual writer stage'
 Set-Acl -LiteralPath $v6 -AclObject $originalAcl;$originalAcl=$null
 Assert (Has-File $file $body.Length) 'Failed prepare lost accepted RAM'
 $rows.Add(@{test='protocol2-native-prepare-actual-os-write-failure-retains-ram';passed=$true;failure=$failure.message})
 Stage 'protocol2-native-cancel-expire-retry'
 $nonce=[guid]::NewGuid().ToString('N');$cancelled=Prepare $nonce
 Assert ($cancelled.type -eq 'stop_prepared') 'Cancel test never reached real frozen preparation'
 $c=Rpc @{type='cancel_stop';token=$cancelled.token};Assert ($c.type -eq 'stop_cancelled') 'Exact cancellation failed'
 $stale=Rpc @{type='commit_stop';token=$cancelled.token};Assert ($stale.type -eq 'error') 'Cancelled token revived'
 $replay=Prepare $nonce;Assert ($replay.type -eq 'error') 'Cancelled nonce replay accepted'
 Assert (Has-File $file $body.Length) 'Cancellation did not release RAM admission'
 $expiring=Prepare ([guid]::NewGuid().ToString('N')) 2000
 Assert ($expiring.type -eq 'stop_prepared') 'Expiry test never reached actual frozen preparation'
 [Threading.Thread]::Sleep(2300)
 $expired=Rpc @{type='commit_stop';token=$expiring.token};Assert ($expired.type -eq 'error') 'Expired preparation was committed'
 Assert (Has-File $file $body.Length) 'Expired prepare lost or blocked RAM'
 $rows.Add(@{test='protocol2-native-cancel-timeout-nonce-replay-and-retry';passed=$true;cancelledNonce=$nonce})
 Stage 'protocol2-positive-freeze-commit-stop-offline-before-restart'
 $prepared=Prepare ([guid]::NewGuid().ToString('N'))
 Assert ($prepared.type -eq 'stop_prepared') 'Positive chain did not reach real writer freeze'
 $prepared|ConvertTo-Json -Depth 20|Set-Content (Join-Path $artifact 'actual-stop-prepared.json')
 $badToken=$prepared.token|ConvertTo-Json -Depth 20|ConvertFrom-Json;$badToken.index_identity+='-wrong-index'
 Assert ((Rpc @{type='commit_stop';token=$badToken}).type -eq 'error') 'Wrong-index token committed'
 $badToken=$prepared.token|ConvertTo-Json -Depth 20|ConvertFrom-Json;$badToken.published_generation++
 Assert ((Rpc @{type='commit_stop';token=$badToken}).type -eq 'error') 'Wrong-disk-generation token committed'
 # A real OS write occurs AFTER RAM was sealed; it must not enter the sealed disk snapshot.
 $lateFile='after-sealed-boundary.txt';$lateBody='journal-only-after-seal'
 [IO.File]::WriteAllText((Join-Path $owned.Root ('native-runtime\'+$lateFile)),$lateBody,[Text.Encoding]::ASCII)
 Stage 'durable-commit-client-reply-not-consumed-exact-retry'
 $receiptFile=Join-Path $v6 ('stop-'+$prepared.token.engine_instance+'-'+$prepared.token.request_nonce+'.json')
 $lost=[IO.Pipes.NamedPipeClientStream]::new('.',$script:pipeName,[IO.Pipes.PipeDirection]::InOut,[IO.Pipes.PipeOptions]::Asynchronous,[Security.Principal.TokenImpersonationLevel]::Impersonation)
 try{$lost.Connect(1000);[uint32]$lostServer=0;Assert ([FxTxProcessNative]::GetNamedPipeServerProcessId($lost.SafePipeHandle.DangerousGetHandle(),[ref]$lostServer) -and $lostServer -eq $prepared.token.service_pid) 'Lost reply pipe owner mismatch';$bytes=[Text.Encoding]::UTF8.GetBytes((@{type='commit_stop';token=$prepared.token}|ConvertTo-Json -Depth 10 -Compress)+"`n");$lost.Write($bytes,0,$bytes.Length);$lost.Flush();Wait-Condition {[IO.File]::Exists($receiptFile)} 5 'Did not really reach durable commit before dropping unread client reply'}finally{$lost.Dispose()}
 @{responseConsumed=$false;durableFileObserved=$true;requestNonce=$prepared.token.request_nonce;note='consumer loss, not claiming a forced server-side socket write failure'}|ConvertTo-Json|Set-Content (Join-Path $artifact 'actual-unconsumed-commit-response.json')
 $committed=Rpc @{type='commit_stop';token=$prepared.token}
 Assert ($committed.type -eq 'stop_committed' -and $committed.receipt.state -eq 'saved_stop_pending' -and -not $committed.receipt.process_stopped) 'SCM or observation was substituted for a durable receipt'
 $receipt=Get-Content -LiteralPath $committed.receipt_path -Raw|ConvertFrom-Json
 foreach($field in @('protocol_version','engine_instance','service_pid','request_nonce','index_identity','mutation_sequence','published_generation','outcome_epoch')){Assert ($receipt.token.$field -ceq $prepared.token.$field) ('Receipt binding mismatch: '+$field)}
 Assert ((Rpc @{type='commit_stop';token=$prepared.token}).type -eq 'stop_committed') 'Same-instance exact commit retry was not idempotent'
 Assert ((Rpc @{type='cancel_stop';token=$prepared.token}).type -eq 'error') 'Committed safe seal was reopened by cancellation'
 $committed|ConvertTo-Json -Depth 20|Set-Content (Join-Path $artifact 'actual-durable-stop-receipt.json')
 Stage 'postcommit-real-receipt-read-denial-stop-pending-retains-seal'
 $receiptAcl=Get-Acl -LiteralPath $receiptFile;$blocked=Get-Acl -LiteralPath $receiptFile
 $readDeny=[Security.AccessControl.FileSystemAccessRule]::new([Security.Principal.SecurityIdentifier]::new('S-1-5-18'),[Security.AccessControl.FileSystemRights]::ReadData,[Security.AccessControl.AccessControlType]::Deny)
 $blocked.AddAccessRule($readDeny);Set-Acl -LiteralPath $receiptFile -AclObject $blocked
 $late=Rpc @{type='commit_stop';token=$prepared.token};$late|ConvertTo-Json -Depth 12|Set-Content (Join-Path $artifact 'actual-postcommit-late-io-error.json')
 Assert ($late.type -eq 'error' -and $late.message -match 'terminal receipt (read|metadata):.*(denied|拒绝|os error 5|Access)') 'Postcommit failure did not reach actual receipt-read OS fault'
 $lateCap=Rpc @{type='upgrade_capabilities';protocol_version=1};Assert ($lateCap.runtime_error -match 'terminal receipt') 'Late IO failure hidden from capability diagnostics'
 $terminalPid=(Service-State).ProcessId;$controller=Get-Service $name;$controller.Stop()
 Wait-Condition {(Service-State).State -eq 'Stop Pending'} 8 'Late committed failure did not preserve StopPending'
 [Threading.Thread]::Sleep(1300);$held=Service-State
 Assert ($held.State -eq 'Stop Pending' -and $held.ProcessId -eq $terminalPid) 'Committed failure unwound Owner/destroyed RAM or exited success'
 $held|Select-Object State,ProcessId,ExitCode,ServiceSpecificExitCode|ConvertTo-Json|Set-Content (Join-Path $artifact 'actual-postcommit-same-pid-stop-pending.json')
 Set-Acl -LiteralPath $receiptFile -AclObject $receiptAcl;$receiptAcl=$null
 $terminal=Stop-Owned
 $rows.Add(@{test='actual-postcommit-late-os-read-failure-refuses-ack-retains-seal-and-retries';passed=$true;heldPid=$terminalPid;fault=$late.message;responseConsumedBeforeRetry=$false})
 Assert ($terminal.State -eq 'Stopped' -and $terminal.ProcessId -eq 0 -and $terminal.ExitCode -eq 0 -and $terminal.ServiceSpecificExitCode -eq 0) 'Receipt alone did not ensure normal exact-service successful STOP'
 & $verify $index (Join-Path $owned.Root ('native-runtime\'+$file)) $body.Length $prepared.token.published_generation (Join-Path $owned.Root ('native-runtime\'+$lateFile)) |Set-Content (Join-Path $artifact 'prepared-stop-offline-before-replay.json')
 Assert ($LASTEXITCODE -eq 0) 'Sealed receipt generation/data/boundary did not match actual disk BEFORE restart'
 Start-Service $name
 Wait-Condition {Ready} 45 'Native service restart after sealed stop failed'
 $newId=Rpc @{type='checkpoint_identity';protocol_version=2}
 Assert ($newId.engine_instance -cne $id.engine_instance -and $newId.service_pid -ne $terminalPid) 'Restart did not change actual instance/PID'
 Assert ((Rpc @{type='commit_stop';token=$prepared.token}).type -eq 'error') 'Previous-instance ACK accepted after restart'
 Wait-Condition {Has-File $file $body.Length} 30 'Sealed accepted RAM missing after restart'
 Wait-Condition {Has-File $lateFile $lateBody.Length} 30 'After-seal OS mutation was not resumed from retained USN cursor'
 $rows.Add(@{test='protocol2-native-freeze-durable-commit-normal-stop-offline-restart-concurrent-boundary';passed=$true;token=$prepared.token;processStoppedExternally=$true;offlineBeforeReplay=$true;lateMutationExcludedBeforeReplay=$true;newInstance=$newId.engine_instance;productionCapability=$false})
 $null=Stop-Owned
 Stage 'actual-corrupt-index-load-error-scm'
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
 Stage 'official-installer-real-new-engine-receipt-stop-backup-failure-rollback'
 # Separate private registration/app/data; run the ACTUAL official Prepare helper.
 & sc.exe delete $name|Out-File (Join-Path $artifact 'delete-first-native-service.log');Assert ($LASTEXITCODE -eq 0) 'Cannot retire owned first registration';$created=$false
 $case=Join-Path $base 'installer-native';$app=Join-Path $case 'app';$data=Join-Path $case 'data';$nativeConfig=Join-Path $case 'native-config';$legacyConfig=Join-Path $case 'legacy-config'
 foreach($dir in @($app,$data,$nativeConfig,$legacyConfig)){$null=[IO.Directory]::CreateDirectory($dir)}
 $builtImage=$ServiceExe;$builtCli=$cli;$ServiceExe=Join-Path $app 'findx2-service.exe';$cli=Join-Path $app 'findx2.exe'
 Copy-Item -LiteralPath $builtImage -Destination $ServiceExe;Copy-Item -LiteralPath $builtCli -Destination $cli
 Assert ((Get-FileHash $builtImage).Hash -ceq (Get-FileHash $ServiceExe).Hash) 'Installer engine is not exact tested source build'
 Copy-Item -LiteralPath $index -Destination (Join-Path $data 'index.bin');Copy-Item -LiteralPath $v6 -Destination (Join-Path $data 'index.bin.v6') -Recurse
 $index=Join-Path $data 'index.bin';$v6=$index+'.v6';$name='FindXUpgradeTest_'+[guid]::NewGuid().ToString('N');$script:pipeName='findx-upgrade-test-'+[guid]::NewGuid().ToString('N')
 $command='"'+$ServiceExe+'" --service --index "'+$index+'" --volume '+$owned.Volume+' --pipe '+$script:pipeName+' --no-everything-ipc --save-interval-secs 1'
 $null=New-Service -Name $name -BinaryPathName $command -StartupType Manual;$created=$true
 $null=New-ItemProperty -Path ('HKLM:\SYSTEM\CurrentControlSet\Services\'+$name) -Name Environment -PropertyType MultiString -Value @('ProgramData='+(Join-Path $base 'programdata'))
 Start-Service $name;Wait-Condition {Ready} 60 'Exact installer-source engine did not reach real native readiness'
 $appHash=(Get-FileHash $ServiceExe).Hash;$tx=Join-Path $case 'transaction-backup-failure'
 $helper=Join-Path $repo 'installer\upgrade-transaction.ps1';$uninstall='FindXUpgradeTest-'+$name+'_is1'
 $args=@('-NoProfile','-NonInteractive','-File',$helper,'-Phase','Prepare','-TransactionRoot',$tx,'-InstallRoot',$app,'-DataRoot',$data,'-NativeConfigRoot',$nativeConfig,'-LegacyConfigRoot',$legacyConfig,'-ServiceName',$name,'-PipeName',$script:pipeName,'-ExpectedVersion','2.5.3','-TimeoutSeconds','20','-UninstallKey',$uninstall,'-TestBuild','-Fault','backup')
 & powershell.exe @args *> (Join-Path $artifact 'official-installer-prepare-backup-failure.log');$helperExit=$LASTEXITCODE
 Assert ($helperExit -ne 0) 'Expected real official backup-stage failure did not happen'
 $journal=Get-Content -LiteralPath (Join-Path $tx 'journal.json') -Raw|ConvertFrom-Json
 Copy-Item -LiteralPath (Join-Path $tx 'journal.json') -Destination (Join-Path $artifact 'official-installer-rollback-journal.json')
 $failureText=Get-Content -LiteralPath (Join-Path $tx 'failure.txt') -Raw
 Assert ($failureText -match 'Injected CI backup failure before any replacement' -and $journal.terminalHandoff.stage -eq 'stopped' -and $journal.terminalHandoff.offlineBeforeReplay -and $journal.state -eq 'rolled_back') 'Failure occurred before actual receipt/STOP/offline/backup stage or rollback not complete'
 Wait-Condition {Ready} 45 'Official rollback did not restore originally running native service'
 Assert ((Get-FileHash $ServiceExe).Hash -ceq $appHash -and (Service-State).StartMode -eq 'Manual') 'Backup failure changed actual app/service start policy'
 Wait-Condition {Has-File $file $body.Length} 30 'Official rollback lost accepted original data'
 Copy-Item -LiteralPath (Join-Path $tx 'installer-terminal-receipt.json') -Destination (Join-Path $artifact 'official-installer-terminal-receipt.json')
 Copy-Item -LiteralPath (Join-Path $tx 'terminal-offline-index.log') -Destination (Join-Path $artifact 'official-installer-offline-index.log')
 $rows.Add(@{test='official-installer-new-engine-prepare-stop-offline-backup-failure-runtime-rollback';passed=$true;targetStage='backup';helperExit=$helperExit;terminal=$journal.terminalHandoff;rollback=$journal.state;realNativeWatch=$true;engineUnchanged=$true})

}catch{
 $rows.Add(@{test='native-runtime-current-stage';stage=$script:stage;passed=$false;error=$_.Exception.Message})
 Capture-NativeDiagnostics
 throw
}finally{
 if($receiptAcl -and $receiptFile){Set-Acl -LiteralPath $receiptFile -AclObject $receiptAcl}
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

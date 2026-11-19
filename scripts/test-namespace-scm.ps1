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
$artifact=Join-Path $repo 'native-namespace-evidence'
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
function Deny-Parent([string]$Path){
 $acl=Get-Acl -LiteralPath $Path;$deny=Get-Acl -LiteralPath $Path
 $rule=[Security.AccessControl.FileSystemAccessRule]::new([Security.Principal.SecurityIdentifier]::new('S-1-5-18'),[Security.AccessControl.FileSystemRights]::ReadAttributes,[Security.AccessControl.InheritanceFlags]::None,[Security.AccessControl.PropagationFlags]::None,[Security.AccessControl.AccessControlType]::Deny)
 $deny.AddAccessRule($rule);Set-Acl -LiteralPath $Path -AclObject $deny;return $acl
}
function Cursor { $text=& $verify --inspect-index $index;Assert ($LASTEXITCODE -eq 0) 'Cannot read persisted private index';$view=$text|ConvertFrom-Json;Assert ($view.source -eq 'segmented-v6') 'Cursor proof must read canonical v6, not stale anchor';return $view.volumes[0] }
function Epoch { $r=Rpc @{type='usage_children';path=$owned.Root;expected=$null;after=$null;max_bytes=4096};Assert ($r.type -eq 'usage_children_result') 'Actual usage epoch unavailable';return $r.page.epoch }
function Waiting-Parent {
 $s=Rpc @{type='status'};$s|ConvertTo-Json -Depth 12|Set-Content -Encoding UTF8 (Join-Path $artifact 'retry-status.json')
 return ($s.watch_error -match 'parent unavailable' -and $s.watch_error -match 'win32=5')
}
function Native-Tool([string]$Tool,[string]$Arguments){
 $id=[guid]::NewGuid().ToString('N');$p=Start-Process $Tool -ArgumentList $Arguments -PassThru -NoNewWindow -RedirectStandardOutput (Join-Path $artifact ($id+'.log')) -RedirectStandardError (Join-Path $artifact ($id+'.err'));$h=$p.Handle
 Assert ($p.WaitForExit(60000) -and $p.ExitCode -eq 0) 'Owned native command failed/timed out; no force'
}
$aclB=$null;$aclD=$null;$index=$null
try{
 Stage 'create-attested-owned-vhd-and-native-seed'
 $owned=New-FxOwnedTestVolume (Join-Path $base 'owned-health.vhd') ('FXTX'+[guid]::NewGuid().ToString('N').Substring(0,8).ToUpperInvariant())
 Copy-Item (Join-Path $base 'owned-volume.json') $artifact
 [IO.File]::WriteAllText((Join-Path $owned.Root '.findx-identity-ci-owned'),$env:GITHUB_SHA)
 $b=Join-Path $owned.Root 'lost-a\lost-b';$d=Join-Path $owned.Root 'lost-c\lost-d'
 $null=[IO.Directory]::CreateDirectory($b);$null=[IO.Directory]::CreateDirectory($d)
 [IO.File]::WriteAllText((Join-Path $owned.Root 'anchor.txt'),'anchor')
 $cli=Join-Path $repo 'target\debug\findx2.exe';$verify=Join-Path $repo 'target\debug\examples\verify_persisted_file.exe';$copy=Join-Path $repo 'target\debug\examples\namespace_index_copy.exe'
 $source=Join-Path $base 'source.bin';$index=Join-Path $base 'incomplete-copy.bin'
 Native-Tool $cli ('index --volume '+$owned.Volume+' --output "'+$source+'" --max-scan-threads 1')
 $initial=& $verify --inspect-index $source;Assert ($LASTEXITCODE -eq 0) 'Cannot inspect canonical native provision'
 $initial|Set-Content (Join-Path $artifact 'canonical-initial-index-state.json');$initialView=$initial|ConvertFrom-Json
 Assert ($initialView.source -eq 'legacy-anchor' -and -not $initialView.metadata_ready -and @($initialView.volumes).Count -eq 1 -and $initialView.volumes[0].root_prefix -ceq $owned.Root) 'Fresh authoritative monolithic CLI seed scope/partial metadata wrong'
 Native-Tool $copy ('"'+$source+'" "'+$index+'" '+$owned.Root)
 $script:pipeName='findx253ns'+[guid]::NewGuid().ToString('N')
 $command='"'+$ServiceExe+'" --service --index "'+$index+'" --volume '+$owned.Volume+' --pipe '+$pipeName+' --no-everything-ipc --save-interval-secs 1'
 Assert ($command -notmatch '--no-watch|--no-backfill|--fixture-root|--fixture-events') 'Real runtime cannot use fixture bypass'
 $null=New-Service -Name $name -BinaryPathName $command -StartupType Manual;$created=$true
 $null=New-ItemProperty -Path ('HKLM:\SYSTEM\CurrentControlSet\Services\'+$name) -Name Environment -PropertyType MultiString -Value @('ProgramData='+(Join-Path $base 'programdata'))
 Stage 'actual-scm-native-watch-and-backfill'
 Start-Service $name;Wait-Condition {Ready} 90 'Actual SCM did not load/watch/backfill'
 $first=Assert-OwnedService;$initialEpoch=Epoch
 Stage 'actual-parent-readattributes-denial-retains-prefix'
 $aclB=Deny-Parent $b
 [IO.File]::WriteAllText((Join-Path $b 'blocked.txt'),'blocked-one')
 Wait-Condition {Waiting-Parent} 30 'Native parent denial did not reach exact retry stage'
 [Threading.Thread]::Sleep(2200);$heldCursor=Cursor
 [IO.File]::WriteAllText((Join-Path $owned.Root 'later.txt'),'later-one')
 [Threading.Thread]::Sleep(2200)
 Assert (Waiting-Parent) 'Retry failure cleared without real repair'
 Assert (-not (Has-File 'later.txt' 9)) 'Consumed later namespace behind uncommitted failed prefix'
 $heldAgain=Cursor;Assert ($heldAgain.journal_id -eq $heldCursor.journal_id -and $heldAgain.last_usn -eq $heldCursor.last_usn) 'Uncommitted prefix advanced persisted cursor'
 Set-Acl -LiteralPath $b -AclObject $aclB;$aclB=$null
 Stage 'native-parent-chain-repair-without-rescan'
 Wait-Condition {Ready} 60 'Native repaired prefix never recovered'
 Wait-Condition {(Has-File 'blocked.txt' 11) -and (Has-File 'later.txt' 9)} 30 'Lost recovered child/later event'
 $hit=Rpc @{type='search';query='blocked.txt';pinyin=$false;limit=10;offset=0}
 Assert ($hit.hits[0].path -ceq (Join-Path $b 'blocked.txt')) 'Recovered parent chain produced wrong canonical path'
 Assert ((Epoch) -eq $initialEpoch -and (Assert-OwnedService).ProcessId -eq $first.ProcessId) 'Parent healing secretly replaced index or restarted service'
 $rows.Add(@{test='actual-scm-parent-denied-prefix-repair';passed=$true;heldJournal=$heldCursor.journal_id;heldCursor=$heldCursor.last_usn;pid=$first.ProcessId;epoch=$initialEpoch;noWatch=$false;noBackfill=$false;incompleteCopyFault=$true})
 Stage 'first-real-terminal-stop-and-offline-proof-before-replay'
 $terminal=Invoke-FxTxPreparedServiceStop $name $script:pipeName $ServiceExe $index $cli '2.5.3' $artifact 15
 & $verify $index (Join-Path $b 'blocked.txt') 11 $terminal.token.published_generation|Set-Content (Join-Path $artifact 'first-offline-proof.json');Assert ($LASTEXITCODE -eq 0) 'First offline payload missing'
 Start-Service $name;Wait-Condition {Ready} 60 'Recovered index restart failed';Assert ((Has-File 'blocked.txt' 11) -and (Has-File 'later.txt' 9)) 'Restart did not restore recovered namespace'
 $beforeSwap=Epoch;$samePid=(Assert-OwnedService).ProcessId
 Stage 'second-real-denial-before-journal-replacement'
 $aclD=Deny-Parent $d
 [IO.File]::WriteAllText((Join-Path $d 'blocked-two.txt'),'blocked-two')
 Wait-Condition {Waiting-Parent} 30 'Second denial did not reach retained native prefix'
 [IO.File]::WriteAllText((Join-Path $owned.Root 'later-two.txt'),'later-two')
 Stage 'actual-private-journal-gap-index-generation-replacement'
 Native-Tool "$env:SystemRoot\System32\fsutil.exe" ('usn deletejournal /d '+$owned.Volume)
 Native-Tool "$env:SystemRoot\System32\fsutil.exe" ('usn createjournal m=16777216 a=1048576 '+$owned.Volume)
 Set-Acl -LiteralPath $d -AclObject $aclD;$aclD=$null
 # Establish actual index replacement FIRST, then stimulate the NEW reader.
 # Demanding its verified recovery before its first post-swap event is circular:
 # the old JournalGap must remain visible until an accepted native read proves it.
 Wait-Condition {(Epoch) -ne $beforeSwap} 120 'Actual RAM index generation was not replaced'
 Assert ((Assert-OwnedService).ProcessId -eq $samePid) 'Journal replacement must exercise live process rebuild, not service restart'
 $rebuiltEpoch=Epoch
 Wait-Condition {(Has-File 'blocked-two.txt' 11) -and (Has-File 'later-two.txt' 9) -and (Has-File 'blocked.txt' 11)} 30 'Rebuild lost pre-probe files; sentinel must not hide missing events'
 Stage 'idle-new-journal-health-before-any-probe'
 Wait-Condition {Ready} 120 'Idle native reader must recover without extra filesystem writes'
 [IO.File]::WriteAllText((Join-Path $d 'post-rebuild.txt'),'post-rebuild')
 Wait-Condition {Has-File 'post-rebuild.txt' 12} 30 'New-generation actual USN listener did not resume'
 Assert ((Epoch) -eq $rebuiltEpoch) 'Post-rebuild event must not trigger another rescan to hide lost events'
 Wait-Condition {Ready} 120 'Same-volume native journal rebuild/replay never recovered'
 Wait-Condition {(Has-File 'blocked-two.txt' 11) -and (Has-File 'later-two.txt' 9) -and (Has-File 'blocked.txt' 11)} 30 'Lost old accepted or failed-prefix filesystem data during generation replacement'
 Stage 'final-terminal-stop-offline-cursor-and-restart'
 $final=Invoke-FxTxPreparedServiceStop $name $script:pipeName $ServiceExe $index $cli '2.5.3' $artifact 15
 & $verify $index (Join-Path $d 'post-rebuild.txt') 12 $final.token.published_generation|Set-Content (Join-Path $artifact 'final-offline-proof.json');Assert ($LASTEXITCODE -eq 0) 'Final offline proof failed'
 $offline=Cursor;$offline|ConvertTo-Json|Set-Content (Join-Path $artifact 'canonical-final-cursor.json');Assert ($offline.journal_id -ne $heldCursor.journal_id -and $offline.last_usn -gt 0) 'Offline generation still carries old journal cursor'
 Start-Service $name;Wait-Condition {Ready} 60 'New journal durable index restart failed';Assert ((Has-File 'post-rebuild.txt' 12) -and (Has-File 'later-two.txt' 9) -and (Has-File 'blocked.txt' 11)) 'Durable new generation lost namespace on restart'
 $null=Invoke-FxTxPreparedServiceStop $name $script:pipeName $ServiceExe $index $cli '2.5.3' $artifact 15
 $rows.Add(@{test='actual-scm-journal-gap-live-index-replace-offline-restart';passed=$true;beforeEpoch=$beforeSwap;oldJournal=$heldCursor.journal_id;newJournal=$offline.journal_id;diskCursor=$offline.last_usn;offlineBeforeReplay=$true;pid=$samePid;noWatch=$false;noBackfill=$false})
 Write-Host 'NAMESPACE_SCM_NATIVE_PASS parent_chain=true real_acl_retry=true later_prefix_retained=true live_index_replace=true offline_before_replay=true restart=true'
}catch{
 $rows.Add(@{test='native-namespace-current-stage';stage=$script:stage;passed=$false;error=$_.Exception.Message});Capture-NativeDiagnostics;throw
}finally{
 if($aclB){Set-Acl -LiteralPath $b -AclObject $aclB};if($aclD){Set-Acl -LiteralPath $d -AclObject $aclD}
 $rows|ConvertTo-Json -Depth 15|Set-Content -Encoding UTF8 (Join-Path $artifact 'results.json')
 # Retain only STOPPED disposable canonical index bytes for future corrected-reader replay.
 $safe=$true
 if($created){try{$s=Stop-Owned;if($s.State -ne 'Stopped'){$safe=$false}}catch{$safe=$false};if($safe){& sc.exe delete $name|Out-File (Join-Path $artifact 'delete-owned.log')}}
 if($safe -and $index -and (Test-Path -LiteralPath $index)){Copy-Item -LiteralPath $index -Destination (Join-Path $artifact 'retained-index.bin');if(Test-Path -LiteralPath ($index+'.v6')){Copy-Item -LiteralPath ($index+'.v6') -Destination (Join-Path $artifact 'retained-index.bin.v6') -Recurse}}
 if($owned -and $safe){Dismount-DiskImage -ImagePath $owned.Image -ErrorAction Stop}
}

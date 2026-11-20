# Shared by the OFFICIAL transaction, native CI and no other privileged fallback.
Set-StrictMode -Version Latest
$ErrorActionPreference='Stop'
. (Join-Path $PSScriptRoot 'stop-mode.ps1')
function Convert-FxTxIndexIdentity([string]$Path){
 if($Path.StartsWith('\\?\')){$Path=$Path.Substring(4)}
 return Get-FxTxPath $Path
}
function Assert-FxTxReceiptService([string]$ServiceName,[string]$PipeName,[string]$Image,[string]$Index,[int]$ExpectedPid=0,[long]$StartTicks=0){
 $svc=Get-CimInstance Win32_Service -Filter ("Name='"+$ServiceName+"'")
 if(-not $svc -or $svc.State -notin @('Running','Stop Pending') -or $svc.ServiceType -ne 'Own Process'){throw 'Exact running own-process service unavailable'}
 Assert-FxTxServiceScope $svc.PathName $Image $Index $PipeName
 $process=Get-Process -Id $svc.ProcessId -ErrorAction Stop
 if($process.Path -ine $Image -or ($ExpectedPid -and $svc.ProcessId -ne $ExpectedPid) -or ($StartTicks -and $process.StartTime.ToUniversalTime().Ticks -ne $StartTicks)){throw 'Service image/PID/creation identity changed; no STOP'}
 return [pscustomobject]@{pid=[int]$svc.ProcessId;startTicks=$process.StartTime.ToUniversalTime().Ticks}
}
function Invoke-FxTxManagement([string]$ServiceName,[string]$PipeName,[string]$Image,[string]$Index,$Owner,$Request,[int]$TimeoutMs=12000){
 $null=Assert-FxTxReceiptService $ServiceName $PipeName $Image $Index $Owner.pid $Owner.startTicks
 $pipe=[IO.Pipes.NamedPipeClientStream]::new('.',$PipeName,[IO.Pipes.PipeDirection]::InOut,[IO.Pipes.PipeOptions]::Asynchronous,[Security.Principal.TokenImpersonationLevel]::Impersonation)
 $reader=$null
 try{
  $pipe.Connect([Math]::Min($TimeoutMs,2000));[uint32]$server=0
  if(-not [FxTxProcessNative]::GetNamedPipeServerProcessId($pipe.SafePipeHandle.DangerousGetHandle(),[ref]$server) -or $server -ne $Owner.pid){throw 'Management pipe is not exact owned service PID'}
  $bytes=[Text.Encoding]::UTF8.GetBytes(($Request|ConvertTo-Json -Depth 12 -Compress)+"`n");$pipe.Write($bytes,0,$bytes.Length);$pipe.Flush()
  $reader=[IO.StreamReader]::new($pipe,[Text.Encoding]::UTF8,$false,4096,$true);$task=$reader.ReadLineAsync()
  if(-not $task.Wait($TimeoutMs)){throw 'Bounded management response timeout; state may remain sealed, no replacement'}
  $line=$task.Result;if(-not $line -or $line.Length -gt 65536){throw 'Invalid bounded management reply'}
  return $line|ConvertFrom-Json
 }finally{if($reader){$reader.Dispose()};$pipe.Dispose()}
}
function Assert-FxTxUpgradeCapabilities($Capabilities){
 if($Capabilities.type -cne 'upgrade_capabilities_result' -or ($Capabilities.protocol_version -isnot [int] -and $Capabilities.protocol_version -isnot [long]) -or $Capabilities.protocol_version -ne 2 -or $Capabilities.durable_prepare -isnot [bool] -or -not $Capabilities.durable_prepare){throw 'No authenticated supported protocol2 capability; refused BEFORE STOP'}
 if($null -ne $Capabilities.runtime_error){throw ('Runtime lifecycle unhealthy; refused BEFORE STOP: '+$Capabilities.runtime_error)}
 if($Capabilities.reason -isnot [string] -or -not $Capabilities.reason -or ($null -ne $Capabilities.last_checkpoint_error -and $Capabilities.last_checkpoint_error -isnot [string])){throw 'Malformed protocol2 capability diagnostic'}
 # last_checkpoint_error is historical diagnostic, not a successful-save claim.
 # Retrying after actual IO repair MUST still perform a fresh prepare/commit.
}
function Assert-FxTxStopToken($Token,$Identity,[string]$Nonce,[string]$Index){
 $fields=@('protocol_version','engine_instance','service_pid','request_nonce','index_identity','mutation_sequence','published_generation','outcome_epoch')
 if(@($Token.PSObject.Properties.Name).Count -ne $fields.Count){throw 'Unexpected complete stop token schema'}
 foreach($field in $fields){if($field -notin $Token.PSObject.Properties.Name){throw ('Missing stop binding: '+$field)}}
 if($Token.protocol_version -ne 2 -or $Token.engine_instance -cne $Identity.engine_instance -or $Token.service_pid -ne $Identity.service_pid -or $Token.request_nonce -cne $Nonce -or $Token.index_identity -cne $Identity.index_identity -or (Convert-FxTxIndexIdentity $Token.index_identity) -ine (Get-FxTxPath $Index)){throw 'Wrong version/instance/PID/nonce/index stop binding'}
 foreach($field in @('protocol_version','service_pid','mutation_sequence','published_generation','outcome_epoch')){if($Token.$field -isnot [long] -and $Token.$field -isnot [int] -and $Token.$field -isnot [uint64]){throw 'Non-integer stop generation/sequence'};if([decimal]$Token.$field -lt 0){throw 'Negative stop generation/sequence'}}
 if([decimal]$Token.published_generation -lt 1){throw 'No published generation'}
}
function Assert-FxTxReceiptFile($Committed,$Token,[string]$Index){
 if($Committed.type -ne 'stop_committed' -or $Committed.receipt.state -cne 'saved_stop_pending' -or $Committed.receipt.process_stopped -isnot [bool] -or $Committed.receipt.process_stopped){throw 'No exact saved-not-stopped commit response'}
 $expected=Join-Path ($Index+'.v6') ('stop-'+$Token.engine_instance+'-'+$Token.request_nonce+'.json')
 if((Get-FxTxPath $Committed.receipt_path) -ine (Get-FxTxPath $expected)){throw 'Receipt path outside exact instance/request/index'}
 if((Get-Item -LiteralPath $expected).Length -gt 16384){throw 'Receipt exceeds bound'}
 $disk=Get-Content -LiteralPath $expected -Raw -Encoding UTF8|ConvertFrom-Json
 foreach($receipt in @($Committed.receipt,$disk)){
  Assert-FxTxStopToken $receipt.token $Token $Token.request_nonce $Index
  if($receipt.state -cne 'saved_stop_pending' -or $receipt.process_stopped -isnot [bool] -or $receipt.process_stopped){throw 'Receipt incorrectly claims process stopped'}
  foreach($field in $Token.PSObject.Properties.Name){if($receipt.token.$field -cne $Token.$field){throw ('Durable/live receipt binding mismatch: '+$field)}}
 }
 return $expected
}
function Assert-FxTxOfflineGeneration([string]$Index,$Token,[string]$Cli,[string]$Evidence){
 $pointerPath=Join-Path ($Index+'.v6') 'published.json';if((Get-Item -LiteralPath $pointerPath).Length -gt 1048576){throw 'Published pointer bound'}
 $pointer=Get-Content -LiteralPath $pointerPath -Raw -Encoding UTF8|ConvertFrom-Json
 if($pointer.manifest.generation -ne $Token.published_generation){throw 'Actual disk generation differs from sealed receipt'}
 # Actual old engine CLI read-only v6 validation; never starts a service/watch or replays USN.
 $info=[Diagnostics.ProcessStartInfo]::new($Cli,('status -i "'+$Index+'"'));$info.UseShellExecute=$false;$info.CreateNoWindow=$true;$info.RedirectStandardOutput=$true;$info.RedirectStandardError=$true;$info.StandardOutputEncoding=[Text.UTF8Encoding]::new($false);$info.StandardErrorEncoding=[Text.UTF8Encoding]::new($false)
 $child=[Diagnostics.Process]::Start($info);$out=$child.StandardOutput.ReadToEndAsync();$err=$child.StandardError.ReadToEndAsync()
 if(-not $child.WaitForExit(30000)){throw 'Offline original-engine validation timeout; no force'}
 $text=$out.Result+"`n"+$err.Result;[IO.File]::WriteAllText((Join-Path $Evidence 'terminal-offline-index.log'),$text)
 if($child.ExitCode -ne 0 -or $text -match '\u56de\u9000|failed integrity|\u8d70\u5168\u91cf\u91cd\u5efa'){throw 'Receipt generation did not validate offline without fallback'}
 $again=Get-Content -LiteralPath $pointerPath -Raw -Encoding UTF8|ConvertFrom-Json
 if($again.manifest.generation -ne $Token.published_generation){throw 'Disk generation changed during offline read'}
}
function Invoke-FxTxVersionProbe([string]$Image){
 $exit=1;$text=''
 try{
  $raw=& $Image --version 2>&1
  if($null -ne $LASTEXITCODE){$exit=[int]$LASTEXITCODE}
  $text=@($raw|ForEach-Object{"$_"}) -join "`n"
 }catch{
  if($null -ne $LASTEXITCODE -and [int]$LASTEXITCODE -ne 0){$exit=[int]$LASTEXITCODE}
  $text=[string]$_.Exception.Message
 }
 return [pscustomobject]@{exitCode=$exit;text=$text}
}
function Test-FxTxCoherentGeneration([string]$Index,[string]$Label){
 $v6=$Index+'.v6'
 $pointerPath=Join-Path $v6 'published.json'
 if(-not (Test-Path -LiteralPath $pointerPath)){throw ($Label+'没有已发布的索引代，已拒绝。本次未替换。')}
 if((Get-Item -LiteralPath $pointerPath).Length -gt 1048576){throw ($Label+'published.json 超出上限，已拒绝。本次未替换。')}
 $pointer=Get-Content -LiteralPath $pointerPath -Raw -Encoding UTF8|ConvertFrom-Json
 if($pointer.format -ne 'v6-gen' -or $pointer.manifest.version -ne 6 -or [decimal]$pointer.manifest.generation -lt 1){throw ($Label+'published.json 不是完整 v6 代，已拒绝。本次未替换。')}
 $gen=[uint64]$pointer.manifest.generation
 $dir=Join-Path (Join-Path $v6 'gen') ([string]$gen)
 $manifestPath=Join-Path $dir 'manifest.json'
 if(-not (Test-Path -LiteralPath $manifestPath)){throw ($Label+'缺少代目录 manifest，已拒绝。本次未替换。')}
 $manifest=Get-Content -LiteralPath $manifestPath -Raw -Encoding UTF8|ConvertFrom-Json
 if([uint64]$manifest.generation -ne $gen){throw ($Label+'manifest 代数与指针不一致，已拒绝。本次未替换。')}
 $names=@($pointer.files.PSObject.Properties.Name)
 if($names.Count -lt 6){throw ($Label+'指针文件表不完整，已拒绝。本次未替换。')}
 foreach($name in $names){
  $path=Join-Path $dir $name
  $item=Get-Item -LiteralPath $path
  $expect=[uint64]$pointer.files.PSObject.Properties[$name].Value.size
  if([uint64]$item.Length -ne $expect){throw ($Label+'文件长度不符: '+$name+'。本次未替换。')}
 }
 $ready=$false
 if(@($pointer.manifest.PSObject.Properties.Name) -contains 'metadata_ready' -and $null -ne $pointer.manifest.metadata_ready){$ready=[bool]$pointer.manifest.metadata_ready}
 return [pscustomobject]@{generation=$gen;metadataReady=$ready;files=$names.Count;directory=$dir}
}
function Invoke-FxTxAcceptedServiceStop([string]$ServiceName,[string]$PipeName,[string]$Image,[string]$Index,[string]$Cli,[string]$Evidence,$Owner,[int]$ScmTimeoutSeconds,[bool]$PrepareCalled,[string]$PrepareError,[string]$ObservedVersion,[scriptblock]$OnState){
 $owner=Assert-FxTxReceiptService $ServiceName $PipeName $Image $Index $Owner.pid $Owner.startTicks
 if($owner.pid -ne $Owner.pid){throw '服务身份已变化，停止前已拒绝。本次未替换。'}
 if(-not [IO.File]::Exists($Cli)){throw '离线核对程序不存在，停止前已拒绝。本次未替换。'}
 $before=Test-FxTxCoherentGeneration $Index '停止前'
 $running=Get-CimInstance Win32_Service -Filter ("Name='"+$ServiceName+"'")
 if($running.State -ne 'Running'){throw '服务不在 Running，未再次停止。本次未替换。'}
 $controller=[ServiceProcess.ServiceController]::new($ServiceName);try{$controller.Stop()}finally{$controller.Dispose()}
 $timer=[Diagnostics.Stopwatch]::StartNew();$stopped=$null
 do{
  $svc=Get-CimInstance Win32_Service -Filter ("Name='"+$ServiceName+"'")
  $alive=Get-Process -Id $owner.pid -ErrorAction SilentlyContinue
  if($svc.State -eq 'Stopped' -and -not $alive){$stopped=$svc;break}
  if($svc.State -eq 'Running' -and [int]$svc.ProcessId -ne $owner.pid){throw '停止过程中服务被重新拉起。本次未替换，未结束进程。'}
  [Threading.Thread]::Sleep(200)
 }while($timer.Elapsed.TotalSeconds -lt $ScmTimeoutSeconds)
 if(-not $stopped){throw '停止超时，未结束进程。本次未替换。'}
 $exitCode=[int]$stopped.ExitCode;$specific=[int]$stopped.ServiceSpecificExitCode
 $verdict=Test-FxTxAcceptedStopExit $exitCode $specific
 if(-not $verdict.accepted -or $verdict.protocol2Receipt){throw ('停止退出码不是已保存状态，本次未替换。win32='+$exitCode+' specific='+$specific)}
 if(Get-Process -Id $owner.pid -ErrorAction SilentlyContinue){throw '原服务进程又出现了。本次未替换，未结束进程。'}
 $after=Test-FxTxCoherentGeneration $Index '停止后'
 $statusText=''
 if([IO.File]::Exists($Cli)){
  $info=[Diagnostics.ProcessStartInfo]::new($Cli,('status -i "'+$Index+'"'));$info.UseShellExecute=$false;$info.CreateNoWindow=$true;$info.RedirectStandardOutput=$true;$info.RedirectStandardError=$true;$info.StandardOutputEncoding=[Text.UTF8Encoding]::new($false);$info.StandardErrorEncoding=[Text.UTF8Encoding]::new($false)
  $child=[Diagnostics.Process]::Start($info);$out=$child.StandardOutput.ReadToEndAsync();$err=$child.StandardError.ReadToEndAsync()
  if(-not $child.WaitForExit(30000)){throw '离线索引核对超时，未替换。'}
  $statusText=$out.Result+"`n"+$err.Result
  [IO.File]::WriteAllText((Join-Path $Evidence 'accepted-offline-index.log'),$statusText)
  if($statusText -match '回退|failed integrity|走全量重建'){throw '已保存的索引代未通过离线完整性核对，本次未替换。'}
 }
 $result=[pscustomobject]@{
  stage='stopped';protocol2Receipt=$false;prepareStopCalled=$PrepareCalled;prepareError=$PrepareError;observedVersion=$ObservedVersion
  savedWithSourceErrors=[bool]$verdict.savedWithSourceErrors;metadataReady=[bool]$after.metadataReady
  generationBefore=$before.generation;generationAfter=$after.generation
  token=$null;owner=$owner;receiptPath=$null;offlineBeforeReplay=$false
  scm=[pscustomobject]@{state=$stopped.State;pid=$stopped.ProcessId;exitCode=$exitCode;serviceExitCode=$specific}
 }
 $result|ConvertTo-Json -Depth 20|Set-Content -Encoding UTF8 (Join-Path $Evidence 'installer-accepted-stop.json')
 & $OnState $result
 return $result
}
function Invoke-FxTxPreparedServiceStop([string]$ServiceName,[string]$PipeName,[string]$Image,[string]$Index,[string]$Cli,[string]$ExpectedVersion,[string]$Evidence,[int]$TimeoutSeconds=12,[int]$ScmTimeoutSeconds=0,[scriptblock]$OnState={param($State)}){
 if(-not ([Security.Principal.WindowsPrincipal]::new([Security.Principal.WindowsIdentity]::GetCurrent())).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)){throw 'Existing elevated administrator token required; no new UAC'}
 $owner=Assert-FxTxReceiptService $ServiceName $PipeName $Image $Index
 $scmTimeout=if($ScmTimeoutSeconds -gt 0){$ScmTimeoutSeconds}else{$TimeoutSeconds}
 if($scmTimeout -lt 1){$scmTimeout=1}
 # Service prepare rejects timeout_ms above 120s. SCM wait is separate and may be longer.
 $prepareMs=[Math]::Min(120000,[Math]::Max(50,[int]$TimeoutSeconds*1000))
 $probe=Invoke-FxTxVersionProbe $Image
 $observed=(($probe.text -split '\r?\n' | Where-Object { -not [string]::IsNullOrWhiteSpace($_) } | Select-Object -First 1))
 @{exitCode=$probe.exitCode;text=$probe.text;observedVersion=[string]$observed;installerVersion=$ExpectedVersion;protocol2Proof=$false}|ConvertTo-Json|Set-Content -Encoding UTF8 (Join-Path $Evidence 'installer-version-probe.json')
 $probeOk=Test-FxTxServiceVersionProbe $probe.text $probe.exitCode
 if(-not $probeOk){throw 'Legacy running service has no verified protocol2; refused BEFORE STOP'}
 $call={param($Request) Invoke-FxTxManagement $ServiceName $PipeName $Image $Index $owner $Request ($prepareMs+5000)}
 $capabilities=$null;$capError=''
 try{$capabilities=& $call @{type='upgrade_capabilities';protocol_version=2}}catch{$capError=$_.Exception.Message}
 if($capabilities){$capabilities|ConvertTo-Json -Depth 10|Set-Content -Encoding UTF8 (Join-Path $Evidence 'installer-capabilities.json')}
 $mode=Get-FxTxPreStopMode -ProbeOk $true -Capabilities $capabilities -CapabilitiesError $capError -InstallerVersion $ExpectedVersion
 if($mode -eq 'refuse-identity'){[IO.File]::WriteAllText((Join-Path $Evidence 'stop-refused.txt'),$capError);throw '服务身份已变化，停止前已拒绝。本次未替换。'}
 if($mode -eq 'refuse-malformed'){throw '协议2能力应答不完整，停止前已拒绝。本次未替换。'}
 $prepareError='';$prepareCalled=$false;$fellThrough=$false
 if($mode -eq 'protocol2-attempt'){
  Assert-FxTxUpgradeCapabilities $capabilities
  $identity=& $call @{type='checkpoint_identity';protocol_version=2}
  if($identity.type -ne 'checkpoint_identity_result' -or $identity.protocol_version -ne 2 -or $identity.service_pid -ne $owner.pid -or $identity.engine_instance -notmatch '^[a-f0-9]{32}$' -or (Convert-FxTxIndexIdentity $identity.index_identity) -ine (Get-FxTxPath $Index)){throw '协议2身份与该服务或索引不一致，停止前已拒绝。本次未替换。'}
  $nonce=[guid]::NewGuid().ToString('N');$token=$null;$committed=$false
  try{
   $prepareCalled=$true
   $prepared=& $call @{type='prepare_stop';protocol_version=2;expected_instance=$identity.engine_instance;request_nonce=$nonce;index_identity=$identity.index_identity;timeout_ms=$prepareMs}
   if($prepared.type -ne 'stop_prepared'){
    $prepareError='Real prepare refused: '+($prepared|ConvertTo-Json -Depth 10 -Compress)
    $outcome=Get-FxTxPrepareOutcome -PrepareError $prepareError -TokenIssued $false
    if($outcome -ne 'accepted-partial'){throw $prepareError}
    $fellThrough=$true
   }else{
    $token=$prepared.token;Assert-FxTxStopToken $token $identity $nonce $Index
    & $OnState ([pscustomobject]@{stage='prepared';token=$token;owner=$owner;protocol2Receipt=$true})
    $reply=& $call @{type='commit_stop';token=$token};$receiptPath=Assert-FxTxReceiptFile $reply $token $Index;$committed=$true
    & $OnState ([pscustomobject]@{stage='committed';token=$token;owner=$owner;receiptPath=$receiptPath;protocol2Receipt=$true})
    $null=Assert-FxTxReceiptService $ServiceName $PipeName $Image $Index $owner.pid $owner.startTicks
    $controller=[ServiceProcess.ServiceController]::new($ServiceName);try{$controller.Stop()}finally{$controller.Dispose()}
    $timer=[Diagnostics.Stopwatch]::StartNew();$stopped=$null
    do{$svc=Get-CimInstance Win32_Service -Filter ("Name='"+$ServiceName+"'");$old=Get-Process -Id $owner.pid -ErrorAction SilentlyContinue;if($svc.State -eq 'Stopped' -and -not $old){$stopped=$svc;break};if($svc.State -eq 'Running' -and [int]$svc.ProcessId -ne $owner.pid){throw '停止过程中服务被重新拉起。回执已保留，本次未替换，未结束进程。'};[Threading.Thread]::Sleep(100)}while($timer.Elapsed.TotalSeconds -lt $scmTimeout)
    if(-not $stopped -or $stopped.ProcessId -ne 0 -or $stopped.ExitCode -ne 0 -or $stopped.ServiceSpecificExitCode -ne 0){throw 'Exact receipt service did not stop successfully; RAM may remain sealed/StopPending; no replacement'}
    $null=Assert-FxTxReceiptFile $reply $token $Index
    Assert-FxTxOfflineGeneration $Index $token $Cli $Evidence
    $result=[pscustomobject]@{stage='stopped';protocol2Receipt=$true;prepareStopCalled=$true;observedVersion=[string]$observed;token=$token;owner=$owner;receiptPath=$receiptPath;scm=[pscustomobject]@{state=$stopped.State;pid=$stopped.ProcessId;exitCode=$stopped.ExitCode;serviceExitCode=$stopped.ServiceSpecificExitCode};offlineBeforeReplay=$true;savedWithSourceErrors=$false}
    $result|ConvertTo-Json -Depth 20|Set-Content -Encoding UTF8 (Join-Path $Evidence 'installer-terminal-receipt.json');& $OnState $result;return $result
   }
  }catch{
   if($token -and -not $committed){try{$cancel=& $call @{type='cancel_stop';token=$token};if($cancel.type -eq 'stop_cancelled'){& $OnState ([pscustomobject]@{stage='cancelled';token=$token;owner=$owner;protocol2Receipt=$false})}else{& $OnState ([pscustomobject]@{stage='uncertain-sealed';token=$token;owner=$owner;protocol2Receipt=$false})}}catch{}}
   throw
  }
 }
 if($mode -eq 'accepted-partial' -or $fellThrough){
  return Invoke-FxTxAcceptedServiceStop -ServiceName $ServiceName -PipeName $PipeName -Image $Image -Index $Index -Cli $Cli -Evidence $Evidence -Owner $owner -ScmTimeoutSeconds $scmTimeout -PrepareCalled $prepareCalled -PrepareError $prepareError -ObservedVersion ([string]$observed) -OnState $OnState
 }
 throw '升级停机没有可接受的路径，停止前已拒绝。本次未替换。'
}
Export-ModuleMember -Function Assert-FxTxUpgradeCapabilities,Invoke-FxTxPreparedServiceStop,Invoke-FxTxManagement,Assert-FxTxReceiptService,Assert-FxTxStopToken,Assert-FxTxReceiptFile,Assert-FxTxOfflineGeneration,Test-FxTxServiceVersionProbe,Get-FxTxPreStopMode,Get-FxTxPrepareOutcome,Test-FxTxAcceptedStopExit

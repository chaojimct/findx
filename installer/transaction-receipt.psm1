# Shared by the OFFICIAL transaction, native CI and no other privileged fallback.
Set-StrictMode -Version Latest
$ErrorActionPreference='Stop'
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
function Invoke-FxTxPreparedServiceStop([string]$ServiceName,[string]$PipeName,[string]$Image,[string]$Index,[string]$Cli,[string]$ExpectedVersion,[string]$Evidence,[int]$TimeoutSeconds=12,[scriptblock]$OnState={param($State)}){
 if(-not ([Security.Principal.WindowsPrincipal]::new([Security.Principal.WindowsIdentity]::GetCurrent())).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)){throw 'Existing elevated administrator token required; no new UAC'}
 $owner=Assert-FxTxReceiptService $ServiceName $PipeName $Image $Index
 # Safe CLI version probe exits in argument parsing, BEFORE any runtime/index load.
 $version=& $Image --version;if($LASTEXITCODE -ne 0 -or ($version -join ' ') -cne ('findx2-service '+$ExpectedVersion) -or $ExpectedVersion -ne '2.5.3'){throw 'Legacy running service has no verified protocol2; refused BEFORE STOP'}
 $call={param($Request) Invoke-FxTxManagement $ServiceName $PipeName $Image $Index $owner $Request ($TimeoutSeconds*1000+5000)}
 $identity=& $call @{type='checkpoint_identity';protocol_version=2}
 if($identity.type -ne 'checkpoint_identity_result' -or $identity.protocol_version -ne 2 -or $identity.service_pid -ne $owner.pid -or $identity.engine_instance -notmatch '^[a-f0-9]{32}$' -or (Convert-FxTxIndexIdentity $identity.index_identity) -ine (Get-FxTxPath $Index)){throw 'Legacy running service has no exact protocol2 identity; refused BEFORE STOP'}
 $nonce=[guid]::NewGuid().ToString('N');$token=$null;$committed=$false
 try{
  $prepared=& $call @{type='prepare_stop';protocol_version=2;expected_instance=$identity.engine_instance;request_nonce=$nonce;index_identity=$identity.index_identity;timeout_ms=$TimeoutSeconds*1000}
  if($prepared.type -ne 'stop_prepared'){throw ('Real prepare refused: '+($prepared|ConvertTo-Json -Depth 10 -Compress))}
  $token=$prepared.token;Assert-FxTxStopToken $token $identity $nonce $Index
  & $OnState ([pscustomobject]@{stage='prepared';token=$token;owner=$owner})
  $reply=& $call @{type='commit_stop';token=$token};$receiptPath=Assert-FxTxReceiptFile $reply $token $Index;$committed=$true
  & $OnState ([pscustomobject]@{stage='committed';token=$token;owner=$owner;receiptPath=$receiptPath})
  $null=Assert-FxTxReceiptService $ServiceName $PipeName $Image $Index $owner.pid $owner.startTicks
  $controller=[ServiceProcess.ServiceController]::new($ServiceName);try{$controller.Stop()}finally{$controller.Dispose()}
  $timer=[Diagnostics.Stopwatch]::StartNew();$stopped=$null
  do{$svc=Get-CimInstance Win32_Service -Filter ("Name='"+$ServiceName+"'");$old=Get-Process -Id $owner.pid -ErrorAction SilentlyContinue;if($svc.State -eq 'Stopped' -and -not $old){$stopped=$svc;break};[Threading.Thread]::Sleep(100)}while($timer.Elapsed.TotalSeconds -lt $TimeoutSeconds)
  if(-not $stopped -or $stopped.ProcessId -ne 0 -or $stopped.ExitCode -ne 0 -or $stopped.ServiceSpecificExitCode -ne 0){throw 'Exact receipt service did not stop successfully; RAM may remain sealed/StopPending; no replacement'}
  $null=Assert-FxTxReceiptFile $reply $token $Index
  Assert-FxTxOfflineGeneration $Index $token $Cli $Evidence
  $result=[pscustomobject]@{stage='stopped';token=$token;owner=$owner;receiptPath=$receiptPath;scm=[pscustomobject]@{state=$stopped.State;pid=$stopped.ProcessId;exitCode=$stopped.ExitCode;serviceExitCode=$stopped.ServiceSpecificExitCode};offlineBeforeReplay=$true}
  $result|ConvertTo-Json -Depth 20|Set-Content -Encoding UTF8 (Join-Path $Evidence 'installer-terminal-receipt.json');& $OnState $result;return $result
 }catch{
  if($token -and -not $committed){try{$cancel=& $call @{type='cancel_stop';token=$token};if($cancel.type -eq 'stop_cancelled'){& $OnState ([pscustomobject]@{stage='cancelled';token=$token;owner=$owner})}else{& $OnState ([pscustomobject]@{stage='uncertain-sealed';token=$token;owner=$owner})}}catch{}}
  throw
 }
}
Export-ModuleMember -Function Invoke-FxTxPreparedServiceStop,Invoke-FxTxManagement,Assert-FxTxReceiptService,Assert-FxTxStopToken,Assert-FxTxReceiptFile,Assert-FxTxOfflineGeneration

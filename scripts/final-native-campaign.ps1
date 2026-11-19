param([Parameter(Mandatory=$true)][string]$Payload)
Set-StrictMode -Version Latest;$ErrorActionPreference='Stop'
if($env:GITHUB_ACTIONS-ne'true'-or$env:RUNNER_OS-ne'Windows'-or$env:COMPUTERNAME-eq'MZ'-or$env:USERPROFILE-eq'C:\Users\chaoj'){throw 'Disposable existing-admin Windows CI only'}
if(-not([Security.Principal.WindowsPrincipal]::new([Security.Principal.WindowsIdentity]::GetCurrent())).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)){throw 'No UAC; existing runner rights required'}
$repo=[IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'));$name='FindX2Search'
if(Get-Service $name -ErrorAction SilentlyContinue){throw 'Never touch pre-existing service'}
Import-Module (Join-Path $PSScriptRoot 'test-owned-ntfs-volume.psm1') -Force
foreach($n in @('transaction-platform.psm1','transaction-files.psm1','transaction-receipt.psm1')){Import-Module (Join-Path $repo ('installer/'+$n)) -Force}
$base=Join-Path $env:RUNNER_TEMP ('FindXUpgradeTest_final_'+[guid]::NewGuid().ToString('N'));$artifact=Join-Path $repo 'final-native-evidence';$null=New-Item -ItemType Directory $base,$artifact
$ServiceExe=Join-Path $Payload 'findx2-service.exe';$cli=Join-Path $Payload 'findx2.exe';$verify=Join-Path $repo 'target/release/examples/verify_persisted_file.exe';$volumes=[Collections.Generic.List[object]]::new();$created=$false;$guis=[Collections.Generic.List[object]]::new();$rows=[Collections.Generic.List[object]]::new();$script:pipeName='findx2';$owned=$null
function Stage($n){Write-Host ('FINAL NATIVE STAGE: '+$n);@{stage=$n;time=(Get-Date -Format o)}|ConvertTo-Json|Set-Content (Join-Path $artifact 'stage.json')}
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

function Inspect { $s=& $verify --inspect-index $index;if($LASTEXITCODE){throw 'Canonical live disk inspect failed'};return $s|ConvertFrom-Json }
function Launch-Gui {
 $psi=[Diagnostics.ProcessStartInfo]::new();$psi.FileName=Join-Path $Payload 'FindX.exe';$psi.UseShellExecute=$false
 foreach($kv in @{LOCALAPPDATA=(Join-Path $base 'gui/local');APPDATA=(Join-Path $base 'gui/roaming');USERPROFILE=$owned.Root;HOME=$owned.Root;ProgramData=(Join-Path $base 'programdata')}.GetEnumerator()){$psi.EnvironmentVariables[$kv.Key]=$kv.Value}
 $p=[Diagnostics.Process]::Start($psi);$null=$p.Handle;$guis.Add($p);return $p
}
Add-Type -AssemblyName UIAutomationClient,UIAutomationTypes,Accessibility
Add-Type -ReferencedAssemblies Accessibility @'
using System;using System.Text;using System.Runtime.InteropServices;using Accessibility;public class FinalGuiExit{public delegate bool CB(IntPtr h,IntPtr p);[DllImport("user32.dll")]public static extern bool EnumWindows(CB c,IntPtr l);[DllImport("user32.dll")]public static extern uint GetWindowThreadProcessId(IntPtr h,out uint p);[DllImport("user32.dll",CharSet=CharSet.Unicode)]public static extern int GetClassName(IntPtr h,StringBuilder b,int n);[DllImport("user32.dll")]public static extern bool PostMessage(IntPtr h,uint m,IntPtr w,IntPtr l);[DllImport("user32.dll",CharSet=CharSet.Unicode)]public static extern uint RegisterWindowMessage(string s);[DllImport("oleacc.dll")]public static extern int AccessibleObjectFromWindow(IntPtr h,uint id,ref Guid g,[MarshalAs(UnmanagedType.Interface)]out object o);public static IntPtr Menu(uint p){IntPtr f=IntPtr.Zero;EnumWindows((h,l)=>{uint id;GetWindowThreadProcessId(h,out id);if(id==p){var b=new StringBuilder(100);GetClassName(h,b,100);if(b.ToString()=="#32768")f=h;}return true;},IntPtr.Zero);return f;}public static void Exit(uint p,IntPtr h){PostMessage(h,RegisterWindowMessage("FindX.Native.Tray.v1"),new IntPtr(1),new IntPtr(0x7b));IntPtr m=IntPtr.Zero;System.Threading.SpinWait.SpinUntil(()=>{m=Menu(p);return m!=IntPtr.Zero;},3000);if(m==IntPtr.Zero)throw new Exception("Owned tray menu missing");Guid g=new Guid("618736e0-3c3d-11cf-810c-00aa00389b71");object o;Marshal.ThrowExceptionForHR(AccessibleObjectFromWindow(m,0xfffffffc,ref g,out o));var a=(IAccessible)o;if(a.accChildCount!=2||!a.get_accName(2).StartsWith("退出 FindX"))throw new Exception("Not own Exit");a.accDoDefaultAction(2);}}
'@
function Close-Gui($p){if(-not$p.HasExited){$p.Refresh();[FinalGuiExit]::Exit([uint32]$p.Id,$p.MainWindowHandle);Assert ($p.WaitForExit(15000)) 'Own GUI did not exit normally; no force'} }
function Invoke-Gui($p,$label){$p.Refresh();$a=[Windows.Automation.AutomationElement]::FromHandle($p.MainWindowHandle);$c=[Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::NameProperty,$label);$script:foundControl=$null;Wait-Condition {$script:foundControl=$a.FindFirst([Windows.Automation.TreeScope]::Descendants,$c);return $null-ne$script:foundControl} 15 ('Actual GUI control did not render: '+$label);$e=$script:foundControl;Assert ($null-ne$e) ('Missing actual GUI control '+$label);([Windows.Automation.InvokePattern]$e.GetCurrentPattern([Windows.Automation.InvokePattern]::Pattern)).Invoke()}
try{
 Stage 'one-shared-real-two-volume-dataset'
 foreach($v in 0..1){$dir=Join-Path $base ('v'+$v);$null=New-Item -ItemType Directory $dir;$vol=New-FxOwnedTestVolume (Join-Path $dir 'owned-health.vhd') ('FXTX'+[guid]::NewGuid().ToString('N').Substring(0,8).ToUpperInvariant());$volumes.Add($vol)
  $body='native233body';for($i=0;$i-lt24000;$i++){[IO.File]::WriteAllText((Join-Path $vol.Root ('fx233-v'+$v+'-'+$i.ToString('D6')+'.bin')),$body,[Text.Encoding]::ASCII)}
  $deep=$vol.Root;for($d=0;$d-lt128;$d++){$deep+="d\";$null=[IO.Directory]::CreateDirectory(('\\?\'+$deep))};[IO.File]::WriteAllText(('\\?\'+$deep+'fx233-deep-v'+$v+'.txt'),'deep',[Text.Encoding]::ASCII)
 }
 $owned=$volumes[0];$null=New-Item -ItemType Directory (Join-Path $base 'programdata/FindX');$index=Join-Path $base 'programdata/FindX/index.bin'
 $scan=Start-Process $cli -ArgumentList ('index --volumes '+($volumes.Volume-join',')+' --output "'+$index+'" --max-scan-threads 1') -PassThru -NoNewWindow -RedirectStandardOutput (Join-Path $artifact 'initial-index.log') -RedirectStandardError (Join-Path $artifact 'initial-index.err');$null=$scan.Handle;Assert ($scan.WaitForExit(120000)-and$scan.ExitCode-eq0) 'Real two-volume native scan failed/timed out; no force'
 $seed=Inspect;Assert (-not$seed.metadata_ready-and$seed.volumes.Count-eq2-and$seed.entries-ge48002) 'Actual partial native scan scope incorrect'
 Stage 'native-full-volume-backfill-and-partial-periodic-commit'
 $command='"'+$ServiceExe+'" --service --index "'+$index+'" --volume '+$owned.Volume+' --pipe findx2 --no-everything-ipc --usage-memory-mib 64 --save-interval-secs 1'
 $null=New-Service -Name $name -BinaryPathName $command -StartupType Manual;$created=$true
 $null=New-ItemProperty ('HKLM:\SYSTEM\CurrentControlSet\Services\'+$name) -Name Environment -PropertyType MultiString -Value @('ProgramData='+(Join-Path $base 'programdata'))
 Start-Service $name;$timer=[Diagnostics.Stopwatch]::StartNew();$partial=$null;$peak=0L;$mutation=Join-Path $owned.Root 'periodic-live-mutation.txt';[IO.File]::WriteAllText($mutation,'during-backfill',[Text.Encoding]::ASCII)
 while($timer.Elapsed.TotalSeconds-lt180){try{$s=Assert-OwnedService;if($s.ProcessId){$peak=[Math]::Max($peak,(Get-Process -Id $s.ProcessId).WorkingSet64)};if(Test-Path ($index+'.v6/published.json')){$before=Get-Content ($index+'.v6/published.json') -Raw|ConvertFrom-Json;$disk=Inspect;$after=Get-Content ($index+'.v6/published.json') -Raw|ConvertFrom-Json;if($before.manifest.generation-eq$after.manifest.generation-and-not$disk.metadata_ready){$proof=& $verify $index $mutation 15 $after.manifest.generation;if($LASTEXITCODE-eq0){$partial=@{generation=$after.manifest.generation;disk=$disk;actualMutationProof=($proof|ConvertFrom-Json)};$partial|ConvertTo-Json -Depth 12|Set-Content (Join-Path $artifact 'actual-partial-periodic.json')}}};if(Ready){break}}catch{Write-Host $_.Exception.Message};[Threading.Thread]::Sleep(100)}
 Assert (Ready) 'Actual native backfill/watch never became fully healthy';$rows.Add(@{test='real-full-two-volume-backfill';passed=$true;knownFiles=48002;maxDepth=128;peakWorkingSetBytes=$peak;seconds=$timer.Elapsed.TotalSeconds})
 # Independent partial durability result: if the real partial window was missed,
 # record unverified/fail; never manufacture false metadata or use no-backfill.
 $rows.Add(@{test='actual-partial-periodic-disk-generation';passed=($null-ne$partial);partial=$partial})
 Stage 'all-known-records-canonical-metadata-and-deep-parent-proof'
 $count=0;$seen=[Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
 do{$r=Rpc @{type='search';query='fx233-v';pinyin=$false;limit=1000;offset=$count};$hits=@($r.hits);foreach($hit in $hits){Assert ($hit.size-eq13) 'Full metadata size not actually backfilled';Assert ($seen.Add($hit.path)) 'Duplicate/cross-volume full path';Assert (@($volumes|Where-Object {$hit.path.StartsWith($_.Root,[StringComparison]::OrdinalIgnoreCase)}).Count-eq1) 'Wrong volume identity'};$count+=$hits.Count;Assert ($count-le48000) 'Unexpected indexed fixture count'}while($hits.Count-gt0)
 Assert ($count-eq48000) 'A known full-volume record was omitted'
 foreach($v in 0..1){$q=Rpc @{type='search';query=('fx233-deep-v'+$v+'.txt');pinyin=$false;limit=10;offset=0};Assert (@($q.hits).Count-eq1-and$q.hits[0].path-ceq($volumes[$v].Root+('d\'*128)+'fx233-deep-v'+$v+'.txt')) 'Deep/cross-volume canonical path mismatch'}
 $rows.Add(@{test='all48000-known-records-and-real128-parent-paths';passed=$true})
 Stage 'real-both-volume-live-usn-create-rename-delete'
 foreach($v in $volumes){$a=Join-Path $v.Root 'fx233-live-create.txt';[IO.File]::WriteAllText($a,'watch-proof',[Text.Encoding]::ASCII);Wait-Condition {$r=Rpc @{type='search';query='fx233-live-create.txt';pinyin=$false;limit=10;offset=0};return @($r.hits|Where-Object {$_.path-ceq$a-and$_.size-eq11}).Count-eq1} 45 'Actual watched volume omitted new file';$b=Join-Path $v.Root 'fx233-live-renamed.txt';[IO.File]::Move($a,$b);Wait-Condition {$r=Rpc @{type='search';query='fx233-live-renamed.txt';pinyin=$false;limit=10;offset=0};return @($r.hits|Where-Object {$_.path-ceq$b}).Count-eq1} 45 'Native USN rename missing';[IO.File]::Delete($b);Wait-Condition {$r=Rpc @{type='search';query='fx233-live-renamed.txt';pinyin=$false;limit=10;offset=0};return @($r.hits|Where-Object {$_.path-ceq$b}).Count-eq0} 45 'Independently absent native deletion not applied'}
 $rows.Add(@{test='native-both-volume-create-rename-confirmed-delete';passed=$true})
 Stage 'real-competing-service-owner-rejection'
 $dup=Start-Process $ServiceExe -ArgumentList ('--index "'+$index+'" --volume '+$owned.Volume+' --pipe findx2 --no-everything-ipc --shutdown-file "'+(Join-Path $base 'duplicate.stop')+'"') -PassThru -NoNewWindow -RedirectStandardError (Join-Path $artifact 'duplicate.err') -RedirectStandardOutput (Join-Path $artifact 'duplicate.out');$null=$dup.Handle
 $rejected=$dup.WaitForExit(10000);if(-not$rejected){[IO.File]::WriteAllText((Join-Path $base 'duplicate.stop'),'stop');$null=$dup.WaitForExit(30000)};Assert ($rejected-and$dup.ExitCode-ne0) 'Actual duplicate owner was not rejected';Assert (Ready) 'Competing process disturbed actual owner';$rows.Add(@{test='real-service-single-owner';passed=$true})
 Stage 'exact-final-gui-official-automatic-import-and-live-session'
 foreach($dir in @('gui/local','gui/roaming/tools.findx.gui')){$null=New-Item -ItemType Directory (Join-Path $base $dir)}
 & (Join-Path $repo 'target/release/final-legacy-export.exe') (Join-Path $base 'gui/local');Assert ($LASTEXITCODE-eq0) 'Legacy closed-source generator failed'
 $oldSettings=Join-Path $base 'gui/roaming/tools.findx.gui/findx2-gui-settings.json';$legacySettings=@{indexPath='old';volume=$owned.Volume;pipeName='old';pinyinDefault=$true;serviceExePath=$ServiceExe;searchLimit=200;autoStartService=$false;autoCheckUpdate=$false;futureKept=233};[IO.File]::WriteAllText($oldSettings,($legacySettings|ConvertTo-Json),[Text.UTF8Encoding]::new($false))
 $sourceHash=(Get-FileHash $oldSettings).Hash;$gui=Launch-Gui;Wait-Condition {$gui.Refresh();return $gui.MainWindowHandle-ne0} 40 'Actual official GUI did not open'
 $nativePrefs=Join-Path $base 'gui/local/FindX-native-dev/prefs.json';$nativeSettings=Join-Path $base 'gui/local/FindX-native-dev/settings.json'
 Wait-Condition {Test-Path $nativePrefs} 20 'Official automatic WebView import did not run';$prefs=Get-Content $nativePrefs -Raw|ConvertFrom-Json;Assert ($prefs.theme-eq'dark'-and-not$prefs.previewOpen-and$prefs.legacyWebView.findx2_search_history-eq'["original legacy query"]') 'Actual automatic legacy preferences import wrong'
 $settings=Get-Content $nativeSettings -Raw|ConvertFrom-Json;Assert ($settings.pipeName-eq'findx2'-and$settings.futureKept-eq233-and$settings.indexPath-ceq$index-and(Get-FileHash $oldSettings).Hash-ceq$sourceHash) 'Automatic settings migration redirected/patched source'
 Invoke-Gui $gui '文件';Invoke-Gui $gui '新建目录标签';Close-Gui $gui
 $saved=Get-Content $nativePrefs -Raw|ConvertFrom-Json;$prefsHash=(Get-FileHash $nativePrefs).Hash;$gui=Launch-Gui;Wait-Condition {$gui.Refresh();return $gui.MainWindowHandle-ne0} 40 'Actual GUI restart failed'
 # Gate5 is the existing SERVICE/IPC owner and live GUI generation handoff,
 # not a newly imposed GUI-process singleton feature. Keep this real GUI alive across SCM restart.
 $current=Get-Content $nativePrefs -Raw|ConvertFrom-Json;Assert ($current.workspaceResume.tabs.Count-eq$saved.workspaceResume.tabs.Count-and$current.legacyWebView.findx2_search_history-eq$saved.legacyWebView.findx2_search_history) 'Actual restart lost session/legacy data'
 $rows.Add(@{test='official-auto-import-and-normal-session-restart';passed=$true;tabs=$saved.workspaceResume.tabs.Count})
 Stage 'actual-stop-and-offline-final-generation-before-any-replay'
 $null=Stop-Owned;$pub=Get-Content ($index+'.v6/published.json') -Raw|ConvertFrom-Json
 & $verify $index $mutation 15 $pub.manifest.generation|Set-Content (Join-Path $artifact 'offline-final-mutation.json');Assert ($LASTEXITCODE-eq0) 'Latest accepted mutation not actually durable'
 Start-Service $name;Wait-Condition {Ready} 90 'Actual full state did not recover after SCM restart'
 Invoke-Gui $gui '搜索';$gui.Refresh();$a=[Windows.Automation.AutomationElement]::FromHandle($gui.MainWindowHandle);$edit=$a.FindFirst([Windows.Automation.TreeScope]::Descendants,[Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::ControlTypeProperty,[Windows.Automation.ControlType]::Edit));Assert ($null-ne$edit) 'Actual live GUI search input absent';([Windows.Automation.ValuePattern]$edit.GetCurrentPattern([Windows.Automation.ValuePattern]::Pattern)).SetValue('fx233-v0-000000.bin')
 Wait-Condition {$items=$a.FindAll([Windows.Automation.TreeScope]::Descendants,[Windows.Automation.Condition]::TrueCondition);return @($items|Where-Object {$_.Current.Name-like'*fx233-v0-000000.bin*'-and$_.Current.ControlType-eq[Windows.Automation.ControlType]::DataItem}).Count-gt0} 30 'Actual GUI did not display new-owner live search result after SCM restart'
 Close-Gui $gui;$rows.Add(@{test='real-live-gui-rebind-new-scm-instance';passed=$true});$null=Stop-Owned
 $rows.Add(@{test='current-engine-stop-offline-and-restart';passed=$true;generation=$pub.manifest.generation})
}finally{
 foreach($g in $guis){try{Close-Gui $g}catch{Write-Host ('Owned GUI cleanup incomplete, no force: '+$_.Exception.Message)}}
 if($created){try{$null=Stop-Owned;sc.exe delete $name|Out-Null}catch{Write-Host ('Owned SCM cleanup incomplete, no force: '+$_.Exception.Message)}}
 foreach($v in $volumes){try{Dismount-DiskImage -ImagePath $v.Image -ErrorAction Stop}catch{Write-Host 'Do not force detach active volume'}}
 $rows.ToArray()|ConvertTo-Json -Depth 20|Set-Content (Join-Path $artifact 'results.json')
}
Assert (@($rows|Where-Object {-not$_.passed}).Count-eq0-and$rows.Count-ge6) 'At least one independent acceptance missing/failed; never publication-ready'

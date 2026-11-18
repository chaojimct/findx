param([Parameter(Mandatory=$true)][string]$PublishedPayload,[Parameter(Mandatory=$true)][string]$Iscc)
Set-StrictMode -Version Latest
$ErrorActionPreference='Stop'
if($env:GITHUB_ACTIONS -ne 'true' -or $env:RUNNER_OS -ne 'Windows'){throw 'Only disposable Windows CI runner allowed'}
if(-not ([Security.Principal.WindowsPrincipal]::new([Security.Principal.WindowsIdentity]::GetCurrent())).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)){throw 'Runner has no native SCM rights; no UAC/fallback'}
$repo=[IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$base=Join-Path $env:RUNNER_TEMP ('FindXUpgradeTest_'+[guid]::NewGuid().ToString('N'))
$null=[IO.Directory]::CreateDirectory($base)
$rows=[Collections.Generic.List[object]]::new()
function Assert($condition,[string]$reason){if(-not $condition){throw $reason}}
function Hash([string]$p){return (Get-FileHash -LiteralPath $p -Algorithm SHA256).Hash}
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
foreach($case in @('fresh-no-service','published252-stopped-service','backup-failure','health-rollback','legacy-running-refusal','stop-timeout','reentry-refusal')){
 $id=[guid]::NewGuid().ToString('N');$service='FindXUpgradeTest_'+$id;$pipe='findx-upgrade-test-'+$id
 $root=Join-Path $base $case;$app=Join-Path $root 'app 中文 space';$data=Join-Path $root 'index state';$native=Join-Path $root 'native config';$legacy=Join-Path $root 'legacy config';$tx=Join-Path $root 'tx';$stage=Join-Path $root 'payload';$build=Join-Path $root 'build'
 foreach($p in @($root,$data,$native,$legacy,$stage,$build)){ $null=[IO.Directory]::CreateDirectory($p) }
 $created=$false
 try{
  foreach($name in @('FindX.exe','findx2.exe','fx.exe','findx2-service.exe')){Copy-Item -LiteralPath (Join-Path $PublishedPayload $name) -Destination (Join-Path $stage $name)}
  [IO.File]::WriteAllBytes((Join-Path $stage 'FindX.installed'),[byte[]](0x31,13,10))
  $null=[IO.Directory]::CreateDirectory((Join-Path $stage 'resources'));[IO.File]::WriteAllText((Join-Path $stage 'resources\transaction-added.txt'),'candidate-owned addition')
  $files=@(Get-ChildItem -LiteralPath $stage -Recurse -File | ForEach-Object{[ordered]@{name=$_.FullName.Substring($stage.Length+1);length=$_.Length;sha256=(Hash $_.FullName).ToLowerInvariant()}})
  [ordered]@{version='2.5.2';gui='native';files=$files} | ConvertTo-Json -Depth 8 | Set-Content -Encoding UTF8 (Join-Path $stage 'stage-manifest.json')
  # Create a real small public index with the UNCHANGED published engine, no watch/backfill/host.
  $owned=Join-Path $root 'owned public fixture';$null=[IO.Directory]::CreateDirectory($owned);[IO.File]::WriteAllText((Join-Path $owned 'owned.txt'),'installer safety fixture')
  $stop=Join-Path $root 'foreground-stop.signal';[IO.File]::WriteAllText($stop,'stop')
  # PS5 promotes informational native stderr into terminating ErrorRecords under
  # ErrorActionPreference=Stop. Capture native streams, judge actual exit status.
  $pi=[Diagnostics.ProcessStartInfo]::new((Join-Path $PublishedPayload 'findx2-service.exe'),('--fixture-root "'+$owned+'" --index "'+(Join-Path $data 'index.bin')+'" --volume C: --pipe '+$pipe+' --no-watch --no-backfill --no-everything-ipc --shutdown-file "'+$stop+'"'))
  $pi.UseShellExecute=$false;$pi.CreateNoWindow=$true;$pi.RedirectStandardOutput=$true;$pi.RedirectStandardError=$true
  $child=[Diagnostics.Process]::Start($pi);$stdout=$child.StandardOutput.ReadToEndAsync();$stderr=$child.StandardError.ReadToEndAsync()
  Assert ($child.WaitForExit(30000)) 'Published engine fixture creation timed out; no force-stop'
  [IO.File]::WriteAllText((Join-Path $root 'fixture-build.log'),($stdout.Result+"`n"+$stderr.Result))
  Assert ($child.ExitCode -eq 0) 'Published engine fixture creation failed'
  [IO.File]::WriteAllText((Join-Path $native 'prefs.json'),'{"theme":"dark","unknown":"preserve"}')
  [IO.File]::WriteAllText((Join-Path $legacy 'old-settings.json'),'{"unknown":"preserve legacy"}')
  $oldIndex=Hash (Join-Path $data 'index.bin');$oldConfig=Hash (Join-Path $native 'prefs.json')
  $oldGui=''
  if($case -ne 'fresh-no-service'){
   $null=[IO.Directory]::CreateDirectory($app)
   foreach($name in @('FindX.exe','findx2.exe','fx.exe','findx2-service.exe')){Copy-Item -LiteralPath (Join-Path $PublishedPayload $name) -Destination (Join-Path $app $name)}
   [IO.File]::WriteAllBytes((Join-Path $app 'FindX.installed'),[byte[]](0x31,13,10));$oldGui=Hash (Join-Path $app 'FindX.exe')
  }
  if($case -in @('published252-stopped-service','legacy-running-refusal','stop-timeout')){
   if($case -ne 'published252-stopped-service'){Copy-Item -LiteralPath $fixtureHost -Destination (Join-Path $app 'findx2-service.exe') -Force}
   $command='"'+(Join-Path $app 'findx2-service.exe')+'" --service --index "'+(Join-Path $data 'index.bin')+'" --pipe '+$pipe
   $null=New-Service -Name $service -BinaryPathName $command -StartupType Manual;$created=$true
   if($case -in @('legacy-running-refusal','stop-timeout')){if($case -eq 'stop-timeout'){[IO.File]::WriteAllText((Join-Path $data 'index.bin.hold-stop'),'hold')};Start-Service $service}
  }
  & $Iscc '/DTxTestBuild=1' (('/DMyServiceName=')+$service) (('/DTxPipeName=')+$pipe) ('/DPublishDir='+$stage) ('/O'+$build) (Join-Path $repo 'installer\FindX.iss') *> (Join-Path $root 'compile.log')
  Assert ($LASTEXITCODE -eq 0) 'Official ISS test-mode compilation failed'
  $setup=Join-Path $build 'FindX-2.5.2-fixture-setup.exe';$fault=if($case -eq 'backup-failure'){'backup'}elseif($case -eq 'health-rollback'){'health'}elseif($case -eq 'stop-timeout'){'stop-timeout'}else{'none'}
  $args='/VERYSILENT /SUPPRESSMSGBOXES /NORESTART /NOICONS /COMPONENTS="main,clicfg" /TASKS="" /DIR="'+$app+'" /TXROOT="'+$tx+'" /TXDATA="'+$data+'" /TXNATIVE="'+$native+'" /TXLEGACY="'+$legacy+'" /TXFAULT='+$fault+' /LOG="'+(Join-Path $root 'setup.log')+'"'
  $process=Start-Process -FilePath $setup -ArgumentList $args -PassThru -Wait
  $expectedSuccess=$case -in @('fresh-no-service','published252-stopped-service','reentry-refusal')
  if($expectedSuccess){Assert ($process.ExitCode -eq 0) ('Expected success, got '+$process.ExitCode);Assert ((Get-Content -LiteralPath (Join-Path $tx 'journal.json') -Raw -Encoding UTF8 | ConvertFrom-Json).state -eq 'committed') 'No commit record'}else{Assert ($process.ExitCode -ne 0) 'Fault did not refuse package'}
  Assert ((Hash (Join-Path $native 'prefs.json')) -eq $oldConfig) 'Config changed'
  Assert ((Hash (Join-Path $data 'index.bin')) -eq $oldIndex) 'Index not restored exactly'
  if($case -in @('backup-failure','health-rollback','legacy-running-refusal','stop-timeout')){Assert ((Hash (Join-Path $app 'FindX.exe')) -eq $oldGui) 'Old GUI binary changed on refusal/rollback'}
  if($case -eq 'health-rollback'){Assert ((Get-Content -LiteralPath (Join-Path $tx 'journal.json') -Raw -Encoding UTF8 | ConvertFrom-Json).state -eq 'rolled_back') 'Rollback not confirmed';Assert (-not (Test-Path -LiteralPath (Join-Path $app 'resources\transaction-added.txt'))) 'Candidate addition left active';Assert (Test-Path -LiteralPath (Join-Path $tx 'failed-new\app\resources\transaction-added.txt')) 'Failed new evidence discarded'}
  if($case -eq 'published252-stopped-service'){Assert ((Get-Service $service).Status -eq 'Stopped') 'Previously stopped service was started'}
  if($case -eq 'legacy-running-refusal'){Assert ((Get-Service $service).Status -eq 'Running') 'Old running service not restored'}
  if($case -eq 'reentry-refusal'){$again=Start-Process -FilePath $setup -ArgumentList $args -PassThru -Wait;Assert ($again.ExitCode -ne 0) 'Committed transaction reused implicitly'}
  $rows.Add([ordered]@{case=$case;passed=$true;exit=$process.ExitCode;root=$root})
 }catch{$rows.Add([ordered]@{case=$case;passed=$false;error=$_.Exception.ToString();root=$root})}
 finally{
  # Only test-owned unique resource cleanup; release a deliberate stall normally.
  $hold=Join-Path $data 'index.bin.hold-stop';if(Test-Path -LiteralPath $hold){[IO.File]::Delete($hold)}
  if($created){$controller=[ServiceProcess.ServiceController]::new($service);try{if($controller.Status -ne 'Stopped'){$controller.Stop();$controller.WaitForStatus([ServiceProcess.ServiceControllerStatus]::Stopped,[TimeSpan]::FromSeconds(20))}; & "$env:SystemRoot\System32\sc.exe" delete $service | Out-Null}finally{$controller.Dispose()}}
 }
}
[ordered]@{scope='actual official ISS with isolated test AppId/paths, published2.5.2 payload; synthetic native service only for adverse running/timeout cases';cases=@($rows.ToArray());passed=@($rows | Where-Object{$_.passed}).Count;failed=@($rows | Where-Object{-not $_.passed}).Count;candidateGUIIntegrated=$false;productionTouched=$false} | ConvertTo-Json -Depth 10 | Set-Content -Encoding UTF8 (Join-Path $base 'results.json')
Write-Output ('NATIVE_INSTALLER_EVIDENCE='+$base)
if(@($rows | Where-Object{-not $_.passed}).Count){throw 'Native package integration matrix failed; see retained evidence'}

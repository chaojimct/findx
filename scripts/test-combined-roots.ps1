# One native two-volume dataset, current segmented and supported monolithic carrier.
# This is scoped root/read-only compatibility, NOT historic old-binary/full-scale acceptance.
Set-StrictMode -Version Latest
$ErrorActionPreference='Stop'
if($env:GITHUB_ACTIONS -ne 'true' -or $env:RUNNER_OS -ne 'Windows' -or $env:COMPUTERNAME -eq 'MZ' -or $env:USERPROFILE -eq 'C:\Users\chaoj'){throw 'Disposable Windows CI only'}
$repo=[IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
Import-Module (Join-Path $PSScriptRoot 'test-owned-ntfs-volume.psm1') -Force
$base=Join-Path $env:RUNNER_TEMP ('FindXUpgradeTest_roots_'+[guid]::NewGuid().ToString('N'))
$artifact=Join-Path $repo 'native-combined-evidence';$null=New-Item -ItemType Directory $base,$artifact
$volumes=[Collections.Generic.List[object]]::new()
function Assert($ok,$why){if(-not $ok){throw $why}}
function Fingerprint($directory){return (@(Get-ChildItem -LiteralPath $directory -Recurse -File|Sort-Object FullName|ForEach-Object {$_.FullName+':'+(Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash}) -join "`n")}
try{
 foreach($i in 1..2){
  $dir=Join-Path $base ('volume'+$i);$null=New-Item -ItemType Directory $dir
  $v=New-FxOwnedTestVolume (Join-Path $dir 'owned-health.vhd') ('FXTX'+[guid]::NewGuid().ToString('N').Substring(0,8).ToUpperInvariant());$volumes.Add($v)
  Copy-Item (Join-Path $dir 'owned-volume.json') (Join-Path $artifact ('owned-volume'+$i+'.json'))
  $path=Join-Path $v.Root 'root-proof.txt';[IO.File]::WriteAllText($path,('volume'+$i))
 }
 Assert ($volumes[0].Volume -ne $volumes[1].Volume) 'Two distinct attested volumes required'
 $indexRoot=Join-Path $base 'indexes';$null=New-Item -ItemType Directory $indexRoot;$index=Join-Path $indexRoot 'combined.bin';$cli=Join-Path $repo 'target\debug\findx2.exe';$verify=Join-Path $repo 'target\debug\examples\verify_persisted_file.exe'
 & $cli index --volumes ($volumes.Volume -join ',') --output $index --max-scan-threads 1 *> (Join-Path $artifact 'native-two-volume-build.log');Assert ($LASTEXITCODE -eq 0) 'Explicit owned two-volume scan failed'
 $view=& $verify --inspect-index $index;Assert ($LASTEXITCODE -eq 0) 'Canonical combined index read failed'
 $view|Set-Content (Join-Path $artifact 'canonical-two-volume.json');$state=$view|ConvertFrom-Json
 Assert ($state.source -eq 'segmented-v6' -and @($state.volumes).Count -eq 2) 'Actual canonical two-volume source required'
 foreach($v in $volumes){Assert (@($state.volumes|Where-Object {$_.root_prefix -ceq $v.Root -and $_.letter -ceq $v.Volume.Substring(0,1)}).Count -eq 1) 'Wrong canonical root/letter'}
 # Preserve the actual generated legacy carrier, do not fabricate an old release.
 $legacy=Join-Path $indexRoot 'legacy-carrier.bin';Copy-Item -LiteralPath $index -Destination $legacy
 $before=Fingerprint $indexRoot
 foreach($inputIndex in @($index,$legacy)){
  foreach($restart in 1..2){
   $json=& $cli search --index $inputIndex root-proof.txt --json;Assert ($LASTEXITCODE -eq 0) 'Offline fresh-process search failed'
   $response=$json|ConvertFrom-Json;$hits=@($response.hits)
   foreach($v in $volumes){Assert (@($hits|Where-Object {$_.path -ceq (Join-Path $v.Root 'root-proof.txt')}).Count -eq 1) 'Cross-volume root collision or missing exact file'}
  }
 }
 Assert ((Fingerprint $indexRoot) -ceq $before) 'Read-only compatibility search modified index or fixture bytes'
 [ordered]@{passed=$true;test='actual-two-volume-canonical-and-legacy-carrier-readonly-fresh-process';currentSource='segmented-v6';independentVolumes=2;historicalOldBinary=$false;fullGate8=$false}|ConvertTo-Json|Set-Content (Join-Path $artifact 'results.json')
}finally{foreach($v in $volumes){Dismount-DiskImage -ImagePath $v.Image -ErrorAction Stop}}

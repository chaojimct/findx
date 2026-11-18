# No cargo/npm/build of runtime binaries. No installer execution.
param([Parameter(Mandatory=$true)][string]$PayloadRoot,[Parameter(Mandatory=$true)][string]$EvidenceRoot,[Parameter(Mandatory=$true)][string]$OutputRoot,[switch]$ValidateOnly,[switch]$BuildFixture,[switch]$Production,[string]$Iscc='',[string]$Extractor='')
Set-StrictMode -Version Latest
$ErrorActionPreference='Stop'
$repo=[IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$lock=Get-Content -LiteralPath (Join-Path $repo 'release/2.5.3-seal-lock.json') -Raw -Encoding UTF8 | ConvertFrom-Json
function Require($ok,[string]$why){if(-not $ok){throw $why}}
function Hash([string]$p){(Get-FileHash -LiteralPath $p -Algorithm SHA256).Hash.ToLowerInvariant()}
function Invoke-SealGit([string[]]$arguments){$result=& git -C $repo @arguments;if($LASTEXITCODE){throw ('Git failed: '+($arguments -join ' '))};return $result}
function CheckFiles([string]$root){
 foreach($f in $lock.files){$path=Join-Path $root $f.name;Require ([IO.File]::Exists($path) -and (Get-Item -LiteralPath $path).Length -eq $f.length -and (Hash $path) -eq $f.sha256) ('Sealed payload mismatch: '+$f.name)}
}
Require (-not ($Production -and $BuildFixture)) 'Cannot mix official approval with fixture mode'
Require ($lock.version -eq '2.5.3' -and $lock.testedCommit -eq 'aea0adc1a88e472457c14cf806a82950a87dc9cb' -and $lock.testedRun -eq 44334214875) 'Not the reviewed successful candidate'
$expected=@('FindX.exe','FindX.installed','findx2-service.exe','findx2.exe','fx.exe','resources\component-manifest.json')
Require (@($lock.files).Count -eq 6 -and @($lock.files | Where-Object {$_.name -cnotin $expected}).Count -eq 0) 'Exact payload allowlist required'
$payload=[IO.Path]::GetFullPath($PayloadRoot)
# Reject reparse points BEFORE descending, including directories and hidden files.
$queue=[Collections.Generic.Queue[string]]::new();$queue.Enqueue($payload);$inventory=@()
while($queue.Count){$d=$queue.Dequeue();Require (((Get-Item -LiteralPath $d -Force).Attributes -band [IO.FileAttributes]::ReparsePoint) -eq 0) 'Reparse payload directory';foreach($v in @(Get-ChildItem -LiteralPath $d -Force)){Require (($v.Attributes -band [IO.FileAttributes]::ReparsePoint) -eq 0) 'Reparse payload item';if($v.PSIsContainer){$queue.Enqueue($v.FullName)}else{$inventory+=$v.FullName.Substring($payload.TrimEnd('\').Length+1)}}}
Require ($inventory.Count -eq 7 -and @($inventory | Where-Object {$_ -cnotin ($expected+@('stage-manifest.json'))}).Count -eq 0) 'Extra/missing candidate files are forbidden'
Require ((Hash (Join-Path $payload 'stage-manifest.json')) -eq $lock.stageManifestSha256) 'Original stage manifest bytes changed'
$m=Get-Content -LiteralPath (Join-Path $payload 'stage-manifest.json') -Raw -Encoding UTF8 | ConvertFrom-Json
Require ($m.sourceCommit -eq $lock.testedCommit -and $m.engineBaseline -eq 'v2.5.2' -and $m.version -eq '2.5.3') 'Stage source/version/baseline mismatch'
CheckFiles $payload
Require ((Get-Item -LiteralPath (Join-Path $payload 'FindX.exe')).VersionInfo.ProductVersion -eq '2.5.3') 'GUI PE version mismatch'
$proofs=@(Get-ChildItem -LiteralPath $EvidenceRoot -Filter results.json -File -Recurse | Where-Object {(Hash $_.FullName) -eq $lock.matrixSha256})
Require ($proofs.Count -eq 1) 'Exact successful matrix bytes missing or ambiguous'
$matrix=Get-Content -LiteralPath $proofs[0].FullName -Raw -Encoding UTF8 | ConvertFrom-Json
Require ($matrix.passed -eq 12 -and $matrix.failed -eq 0 -and @($matrix.cases).Count -eq 12 -and @($matrix.cases | Where-Object {-not $_.passed}).Count -eq 0 -and $matrix.candidateGuiSha256 -ieq ($lock.files | Where-Object name -eq 'FindX.exe').sha256) 'Actual final12 matrix mismatch'
$head=Invoke-SealGit -arguments @('rev-parse','HEAD')
foreach($path in @('crates','gui','installer','Cargo.toml','Cargo.lock','scripts/check_release.py')){Require ((Invoke-SealGit -arguments @('rev-parse',('HEAD:'+$path))) -eq (Invoke-SealGit -arguments @('rev-parse',($lock.testedCommit+':'+$path)))) ('Untested runtime/installer/checker source change: '+$path)}
$helperPaths=@('installer/FindX.iss','installer/transaction-files.psm1','installer/transaction-platform.psm1','installer/upgrade-transaction.ps1','installer/Languages/ChineseSimplified.isl','installer/Languages/Default.isl','gui/src-tauri/icons/icon.ico','scripts/check_release.py')
Require (@($lock.helpers).Count -eq $helperPaths.Count) 'Helper lock incomplete'
foreach($h in $lock.helpers){Require ($h.path -cin $helperPaths -and (Invoke-SealGit -arguments @('rev-parse',($lock.testedCommit+':'+$h.path))) -eq $h.gitBlob) 'Exact tested helper Git blob mismatch'}
if($ValidateOnly){Write-Output 'SEALED_INPUT_VALIDATED_6_HASHES_TESTED_SOURCE_AND_12_CASES';return}
Require (-not (Invoke-SealGit -arguments @('status','--porcelain'))) 'Only a clean committed packaging source may be sealed'
if($Production){
 Require ($env:GITHUB_ACTIONS -eq 'true' -and $env:GITHUB_REPOSITORY -eq 'chaojimct/findx' -and $env:GITHUB_REF -eq 'refs/tags/v2.5.3') 'Official compilation only in the approved tag workflow'
 Require ((Invoke-SealGit -arguments @('rev-parse','v2.5.3^{commit}')) -eq $head) 'Official source tag differs from checked-out source'
 & python (Join-Path $repo 'scripts/check_release.py') --expected-version 2.5.3 --require-ready
 Require ($LASTEXITCODE -eq 0) 'Publication policy is not approved; production compilation forbidden'
}
if($BuildFixture){Require ($env:GITHUB_ACTIONS -eq 'true' -and $env:RUNNER_OS -eq 'Windows') 'Fixture compilation is disposable Windows CI only'}
$out=[IO.Path]::GetFullPath($OutputRoot);Require (-not (Test-Path -LiteralPath $out)) 'Output must be new; no overwriting sealed assets'
$null=New-Item -ItemType Directory -Path $out
$bundle=Join-Path $out 'sealed';$null=New-Item -ItemType Directory -Path (Join-Path $bundle 'payload')
Copy-Item -Path (Join-Path $payload '*') -Destination (Join-Path $bundle 'payload') -Recurse
$archive=Join-Path $out 'tested-source.zip';$null=Invoke-SealGit -arguments (@('archive','--format=zip','-o',$archive,$lock.testedCommit,'--')+$helperPaths)
Expand-Archive -LiteralPath $archive -DestinationPath (Join-Path $bundle 'source')
Copy-Item -LiteralPath $proofs[0].FullName -Destination (Join-Path $bundle 'matrix12.json')
Copy-Item -LiteralPath (Join-Path $repo 'release/2.5.3-seal-lock.json') -Destination (Join-Path $bundle 'seal-lock.json')
Copy-Item -LiteralPath $PSCommandPath -Destination (Join-Path $bundle 'pack-sealed-release.ps1')
$receipt=[ordered]@{version='2.5.3';runtimeSourceCommit=$lock.testedCommit;packagingSourceCommit=$head;testedRun=$lock.testedRun;engineBaseline='published v2.5.2 exact binaries';guiSha256=($lock.files | Where-Object name -eq 'FindX.exe').sha256;matrixPassed=12;matrixFailed=0;officialApproval=[bool]$Production;fixtureOnly=[bool]$BuildFixture;helperGitBlobs=$lock.helpers;files=@()}
if($Production -or $BuildFixture){
 Require ([IO.File]::Exists($Iscc)) 'Explicit Inno compiler missing'
 $compile=Join-Path $out $(if($Production){'official'}else{'fixture'});$null=New-Item -ItemType Directory -Path $compile
 $args=@('/DMyAppVersion=2.5.3',('/DPublishDir='+(Join-Path $bundle 'payload')),('/O'+$compile))
 if($Production){$args+='/DUpgradeGatesApproved=1'}else{$id=[guid]::NewGuid().ToString('N');$args+=@('/DTxTestBuild=1',('/DMyServiceName=FindXUpgradeTest_'+$id),('/DTxPipeName=findx-upgrade-test-'+$id))}
 & $Iscc @args (Join-Path $bundle 'source/installer/FindX.iss') *> (Join-Path $out 'compile.log');Require ($LASTEXITCODE -eq 0) 'Sealed ISCC compilation failed'
 if($BuildFixture){
  Require ([IO.File]::Exists($Extractor)) 'Pinned read-only unpacker required for fixture content verification'
  $fixture=Join-Path $compile 'FindX-2.5.3-fixture-setup.exe';$unpacked=Join-Path $out 'fixture-content'
  & $Extractor '-x' '-y' ('-d'+$unpacked) $fixture *> (Join-Path $out 'fixture-unpack.log');Require ($LASTEXITCODE -eq 0) 'Fixture read-only extraction failed'
  CheckFiles (Join-Path $unpacked '{app}')
  Require ((Hash (Join-Path $unpacked '{app}/payload-manifest.json')) -eq $lock.stageManifestSha256) 'Embedded stage manifest changed'
  foreach($name in @('transaction-files.psm1','transaction-platform.psm1','upgrade-transaction.ps1')){$found=@(Get-ChildItem -LiteralPath $unpacked -Recurse -File -Filter $name);Require ($found.Count -eq 1 -and (Hash $found[0].FullName) -eq (Hash (Join-Path $bundle ('source/installer/'+$name)))) ('Embedded helper differs: '+$name)}
  Copy-Item -LiteralPath $fixture -Destination (Join-Path $bundle 'FindX-2.5.3-fixture-setup.exe')
  $receipt['fixtureSha256']=Hash $fixture
 }
}
foreach($file in @(Get-ChildItem -LiteralPath $bundle -File -Recurse)){$receipt.files+=@{name=$file.FullName.Substring($bundle.Length+1);length=$file.Length;sha256=(Hash $file.FullName)}}
$receipt | ConvertTo-Json -Depth 12 | Set-Content -Encoding UTF8 (Join-Path $bundle 'seal.json')
$zip=Join-Path $out 'FindX-2.5.3-sealed-candidate.zip';Compress-Archive -Path (Join-Path $bundle '*') -DestinationPath $zip -CompressionLevel Optimal
((Hash $zip)+'  '+[IO.Path]::GetFileName($zip)) | Set-Content -Encoding ASCII (Join-Path $out 'SHA256SUMS.txt')
if($Production){$setup=Join-Path $out 'official/FindX-2.5.3-setup.exe';((Hash $setup)+'  FindX-2.5.3-setup.exe') | Set-Content -Encoding ASCII (Join-Path $out 'official/SHA256SUMS.txt');Copy-Item -LiteralPath (Join-Path $bundle 'seal.json') -Destination (Join-Path $out 'official/seal.json')}
Write-Output ('SEALED_PACKAGE='+$zip);Write-Output ('SEALED_SHA256='+(Hash $zip))

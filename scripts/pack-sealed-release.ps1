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
Require ($lock.version -eq '2.5.3' -and $lock.schema -ceq 'rebuilt-engine-composite-v2' -and $lock.testedCommit -match '^[0-9a-f]{40}$' -and $lock.testedRun -gt 0) 'Pinned final rebuilt payload/composite proof lock required'
$rebuilt=$true
$expected=@('FindX.exe','FindX.installed','findx2-service.exe','findx2.exe','fx.exe','resources\component-manifest.json')
Require (@($lock.files).Count -eq 6 -and @($lock.files | Where-Object {$_.name -cnotin $expected}).Count -eq 0) 'Exact payload allowlist required'
$payload=[IO.Path]::GetFullPath($PayloadRoot)
# Reject reparse points BEFORE descending, including directories and hidden files.
$queue=[Collections.Generic.Queue[string]]::new();$queue.Enqueue($payload);$inventory=@()
while($queue.Count){$d=$queue.Dequeue();Require (((Get-Item -LiteralPath $d -Force).Attributes -band [IO.FileAttributes]::ReparsePoint) -eq 0) 'Reparse payload directory';foreach($v in @(Get-ChildItem -LiteralPath $d -Force)){Require (($v.Attributes -band [IO.FileAttributes]::ReparsePoint) -eq 0) 'Reparse payload item';if($v.PSIsContainer){$queue.Enqueue($v.FullName)}else{$inventory+=$v.FullName.Substring($payload.TrimEnd('\').Length+1)}}}
Require ($inventory.Count -eq 7 -and @($inventory | Where-Object {$_ -cnotin ($expected+@('stage-manifest.json'))}).Count -eq 0) 'Extra/missing candidate files are forbidden'
Require ((Hash (Join-Path $payload 'stage-manifest.json')) -eq $lock.stageManifestSha256) 'Original stage manifest bytes changed'
$m=Get-Content -LiteralPath (Join-Path $payload 'stage-manifest.json') -Raw -Encoding UTF8 | ConvertFrom-Json
Require ($m.sourceCommit -eq $lock.testedCommit -and $m.version -eq '2.5.3') 'Stage source/version mismatch'
if($rebuilt){Require ($m.engineUnchanged -eq $false -and $m.guiSourceCommit -eq 'b72219bc844de5eb141bb99a708494c956f53073') 'Wrong final rebuilt engine/attested GUI origin'}else{Require ($m.engineBaseline -eq 'v2.5.2') 'Historical baseline mismatch'}
CheckFiles $payload
Require ((Get-Item -LiteralPath (Join-Path $payload 'FindX.exe')).VersionInfo.ProductVersion -eq '2.5.3') 'GUI PE version mismatch'
# Proofs remain raw, including failed/unselected historical rows. Do not relabel
# these as one 18/18 run on identical runtime bytes. Each reuse has explicit
# component source applicability; the final changed service has native + ISS proof.
$proofFiles=@{};$covered=@{};$nativeCovered=@{};$freshCases=@{};$freshNative=@{}
Require (@($lock.proofs).Count -gt 0) 'Independent raw proofs required'
foreach($p in $lock.proofs){
 Require (-not $proofFiles.ContainsKey($p.id)) 'Duplicate proof id'
 $matches=@(Get-ChildItem -LiteralPath $EvidenceRoot -File -Recurse -Filter $p.fileName | Where-Object {(Hash $_.FullName) -ceq $p.sha256})
 Require ($matches.Count -eq 1) ('Exact raw proof missing/ambiguous: '+$p.id)
 $file=$matches[0].FullName;$proofFiles[$p.id]=$file
 $run=gh run view $p.run --repo chaojimct/findx --json headSha,status,conclusion|ConvertFrom-Json
 Require ($LASTEXITCODE -eq 0 -and $run.headSha -ceq $p.commit -and $run.status -ceq 'completed' -and $run.conclusion -ceq $p.runConclusion) ('Actual run identity/outcome mismatch: '+$p.id)
 if($p.payloadCommit -cne $lock.testedCommit){
  Require ($p.reuseReason.Length -gt 0 -and @($p.unchangedPaths).Count -gt 0) ('Historical applicability missing: '+$p.id)
  foreach($path in $p.unchangedPaths){Require ((Invoke-SealGit -arguments @('rev-parse',('HEAD:'+$path))) -ceq (Invoke-SealGit -arguments @('rev-parse',($p.payloadCommit+':'+$path)))) ('Reused component changed: '+$path)}
  # An explicit reviewed delta accompanies, rather than hides, changed service
  # bytes. The current native finalization + affected ISS proofs remain mandatory.
  $delta=Join-Path $env:TEMP ('findx-seal-delta-'+[guid]::NewGuid().ToString('N')+'.txt')
  try{& git -C $repo diff --no-ext-diff --binary $p.payloadCommit $lock.testedCommit -- crates/findx2-service | Out-File -LiteralPath $delta -Encoding utf8;Require ($LASTEXITCODE -eq 0 -and (Hash $delta) -ceq $p.reviewedServiceDeltaSha256) 'Reviewed service delta changed'}finally{Remove-Item -LiteralPath $delta -ErrorAction SilentlyContinue}
 }
 if($p.kind -ceq 'matrix'){
  $v=Get-Content -LiteralPath $file -Raw -Encoding UTF8|ConvertFrom-Json
  Require ($v.payloadSourceCommit -ceq $p.payloadCommit -and $v.candidateGuiSha256 -ieq ($lock.files|Where-Object name -ceq 'FindX.exe').sha256 -and $v.productionTouched -eq $false) 'Matrix payload/safety provenance differs'
  foreach($id in $p.selected){$rows=@($v.cases|Where-Object {$_.case -ceq $id -and $_.passed -eq $true});Require ($rows.Count -eq 1) ('Unpassed actual installer case: '+$id);$covered[$id]=$true;if($p.payloadCommit -ceq $lock.testedCommit){$freshCases[$id]=$true}}
 }elseif($p.kind -ceq 'native'){
  $v=Get-Content -LiteralPath $file -Raw -Encoding UTF8|ConvertFrom-Json
  foreach($id in $p.selected){$rows=@($v|Where-Object {$_.test -ceq $id -and $_.passed -eq $true});Require ($rows.Count -eq 1) ('Unpassed actual native row: '+$id);$nativeCovered[$id]=$true;if($p.payloadCommit -ceq $lock.testedCommit){$freshNative[$id]=$true}}
 }elseif($p.kind -ceq 'text'){
  $v=Get-Content -LiteralPath $file -Raw -Encoding UTF8
  foreach($pattern in $p.requiredLiterals){Require ($v.Contains($pattern)) ('Missing raw benchmark/compatibility result: '+$pattern)}
 }else{throw 'Unknown evidence kind'}
}
Require (@($lock.installerCases).Count -eq 18 -and @($lock.installerCases|Sort-Object -Unique).Count -eq 18 -and $covered.Count -eq 18) 'All 18 distinct existing installer cases required'
foreach($id in $lock.installerCases){Require ($covered.ContainsKey($id)) ('Uncovered required installer case: '+$id)}
foreach($id in @('running253-success','running253-save-refusal')){Require ($freshCases.ContainsKey($id)) 'Current payload actual prepare success/refusal integration required'}
foreach($id in @('real-full-two-volume-backfill','actual-partial-periodic-disk-generation','official-auto-import-and-normal-session-restart','real-live-gui-rebind-new-scm-instance','current-engine-stop-offline-and-restart')){Require ($freshNative.ContainsKey($id)) ('Current finalizer native proof missing: '+$id)}
$requiredNative=@('real-full-two-volume-backfill','actual-partial-periodic-disk-generation','all48000-known-records-and-real128-parent-paths','native-both-volume-create-rename-confirmed-delete','real-service-single-owner','official-auto-import-and-normal-session-restart','real-live-gui-rebind-new-scm-instance','current-engine-stop-offline-and-restart','actual-scm-parent-denied-prefix-repair','actual-scm-journal-gap-live-index-replace-offline-restart')
foreach($id in $requiredNative){Require ($nativeCovered.ContainsKey($id)) ('Missing independent native acceptance: '+$id)}
Require (@($lock.gates).Count -eq 8 -and @($lock.gates|Where-Object {$_.required -ne $true -or $_.passed -ne $true}).Count -eq 0) 'Eight independently satisfied existing gates required'
foreach($g in $lock.gates){Require ($g.applicability.Length -gt 0 -and @($g.proofIds).Count -gt 0) 'Gate applicability/proof required';foreach($id in $g.proofIds){Require ($proofFiles.ContainsKey($id)) 'Gate references missing raw proof'}}
$current=gh run view $lock.acceptanceRun --repo chaojimct/findx --json headSha,status,conclusion|ConvertFrom-Json
Require ($LASTEXITCODE -eq 0 -and $current.headSha -ceq $lock.acceptanceCommit -and $current.status -ceq 'completed' -and $current.conclusion -ceq 'success') 'Final fixture-only acceptance must truly succeed; failed raw rows are never selected'
$caseCount=18

Require ($lock.acceptanceCommit -match '^[0-9a-f]{40}$') 'Explicit final acceptance source required'
foreach($path in @('crates','gui','installer','Cargo.toml','Cargo.lock','scripts/check_release.py')){Require ((Invoke-SealGit -arguments @('rev-parse',($lock.acceptanceCommit+':'+$path))) -ceq (Invoke-SealGit -arguments @('rev-parse',($lock.testedCommit+':'+$path)))) 'Final fixture acceptance runtime source differs from payload'}
$head=Invoke-SealGit -arguments @('rev-parse','HEAD')
foreach($path in @('crates','gui','installer','Cargo.toml','Cargo.lock','scripts/check_release.py')){Require ((Invoke-SealGit -arguments @('rev-parse',('HEAD:'+$path))) -eq (Invoke-SealGit -arguments @('rev-parse',($lock.testedCommit+':'+$path)))) ('Untested runtime/installer/checker source change: '+$path)}
$helperPaths=@('installer/FindX.iss','installer/transaction-files.psm1','installer/transaction-platform.psm1','installer/transaction-receipt.psm1','installer/upgrade-transaction.ps1','installer/Languages/ChineseSimplified.isl','installer/Languages/Default.isl','gui/src-tauri/icons/icon.ico','scripts/check_release.py')
Require (@($lock.helpers).Count -eq $helperPaths.Count) 'Helper lock incomplete'
foreach($h in $lock.helpers){Require ($h.path -cin $helperPaths -and (Invoke-SealGit -arguments @('rev-parse',($lock.testedCommit+':'+$h.path))) -eq $h.gitBlob) 'Exact tested helper Git blob mismatch'}
if($ValidateOnly){Write-Output 'SEALED_INPUT_VALIDATED_6_HASHES_AND_EXPLICIT_COMPONENT_SCOPED_PROOFS';return}
Require (-not (Invoke-SealGit -arguments @('status','--porcelain'))) 'Only a clean committed packaging source may be sealed'
if($Production){
 Require ($rebuilt) 'Production 2.5.3 now requires the new-engine final lock, not the historical252 fixture'
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
$null=New-Item -ItemType Directory (Join-Path $bundle 'proofs');foreach($id in $proofFiles.Keys){Copy-Item -LiteralPath $proofFiles[$id] -Destination (Join-Path $bundle ('proofs/'+$id+'.'+[IO.Path]::GetExtension($proofFiles[$id]).TrimStart('.')))}
Copy-Item -LiteralPath (Join-Path $repo 'release/2.5.3-seal-lock.json') -Destination (Join-Path $bundle 'seal-lock.json')
Copy-Item -LiteralPath $PSCommandPath -Destination (Join-Path $bundle 'pack-sealed-release.ps1')
$receipt=[ordered]@{version='2.5.3';runtimeSourceCommit=$lock.testedCommit;packagingSourceCommit=$head;testedRun=$lock.testedRun;acceptanceRun=$lock.acceptanceRun;acceptanceCommit=$lock.acceptanceCommit;engineBaseline=$(if($rebuilt){'v2.5.2 source plus authorized engine durability/identity fixes'}else{'historical published v2.5.2 binaries'});guiSha256=($lock.files | Where-Object name -eq 'FindX.exe').sha256;matrixCoverage='18 distinct cases across explicitly component-scoped runs, NOT one identical-payload 18-case run';installerCases=$lock.installerCases;proofProvenance=$lock.proofs;independentGates=$lock.gates;officialApproval=[bool]$Production;fixtureOnly=[bool]$BuildFixture;helperGitBlobs=$lock.helpers;files=@()}
if($Production -or $BuildFixture){
 Require ([IO.File]::Exists($Iscc)) 'Explicit Inno compiler missing'
 $compile=Join-Path $out $(if($Production){'official'}else{'fixture'});$null=New-Item -ItemType Directory -Path $compile
 $args=@('/DMyAppVersion=2.5.3',('/DPublishDir='+(Join-Path $bundle 'payload')),('/O'+$compile))
 if($Production){$args+='/DUpgradeGatesApproved=1'}else{$id=[guid]::NewGuid().ToString('N');$args+=@('/DTxTestBuild=1',('/DMyServiceName=FindXUpgradeTest_'+$id),('/DTxPipeName=findx-upgrade-test-'+$id))}
 & $Iscc @args (Join-Path $bundle 'source/installer/FindX.iss') *> (Join-Path $out 'compile.log');Require ($LASTEXITCODE -eq 0) 'Sealed ISCC compilation failed'
 if($Production -or $BuildFixture){
  Require ([IO.File]::Exists($Extractor)) 'Pinned read-only unpacker required for fixture content verification'
  $fixture=Join-Path $compile $(if($Production){'FindX-2.5.3-setup.exe'}else{'FindX-2.5.3-fixture-setup.exe'});$unpacked=Join-Path $out 'verified-setup-content'
  & $Extractor '-x' '-y' ('-d'+$unpacked) $fixture *> (Join-Path $out 'fixture-unpack.log');Require ($LASTEXITCODE -eq 0) 'Fixture read-only extraction failed'
  CheckFiles (Join-Path $unpacked '{app}')
  Require ((Hash (Join-Path $unpacked '{app}/payload-manifest.json')) -eq $lock.stageManifestSha256) 'Embedded stage manifest changed'
  foreach($name in @('transaction-files.psm1','transaction-platform.psm1','transaction-receipt.psm1','upgrade-transaction.ps1')){$found=@(Get-ChildItem -LiteralPath $unpacked -Recurse -File -Filter $name);Require ($found.Count -eq 1 -and (Hash $found[0].FullName) -eq (Hash (Join-Path $bundle ('source/installer/'+$name)))) ('Embedded helper differs: '+$name)}
  Copy-Item -LiteralPath $fixture -Destination (Join-Path $bundle ([IO.Path]::GetFileName($fixture)))
  $receipt['setupSha256']=Hash $fixture
 }
}
foreach($file in @(Get-ChildItem -LiteralPath $bundle -File -Recurse)){$receipt.files+=@{name=$file.FullName.Substring($bundle.Length+1);length=$file.Length;sha256=(Hash $file.FullName)}}
$receipt | ConvertTo-Json -Depth 12 | Set-Content -Encoding UTF8 (Join-Path $bundle 'seal.json')
$zip=Join-Path $out 'FindX-2.5.3-sealed-candidate.zip';Compress-Archive -Path (Join-Path $bundle '*') -DestinationPath $zip -CompressionLevel Optimal
((Hash $zip)+'  '+[IO.Path]::GetFileName($zip)) | Set-Content -Encoding ASCII (Join-Path $out 'SHA256SUMS.txt')
if($Production){$setup=Join-Path $out 'official/FindX-2.5.3-setup.exe';((Hash $setup)+'  FindX-2.5.3-setup.exe') | Set-Content -Encoding ASCII (Join-Path $out 'official/SHA256SUMS.txt');Copy-Item -LiteralPath (Join-Path $bundle 'seal.json') -Destination (Join-Path $out 'official/seal.json')}
Write-Output ('SEALED_PACKAGE='+$zip);Write-Output ('SEALED_SHA256='+(Hash $zip))

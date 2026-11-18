param([Parameter(Mandatory=$true)][string]$PayloadRoot,[Parameter(Mandatory=$true)][string]$EvidenceRoot,[string]$ScratchRoot='',[switch]$ValidationOnlyTests)
Set-StrictMode -Version Latest
$ErrorActionPreference='Stop'
$repo=[IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
if(-not $ScratchRoot){if($env:GITHUB_ACTIONS -ne 'true'){throw 'Explicit new owned local scratch required'};$ScratchRoot=Join-Path $env:RUNNER_TEMP ('sealed-negative-'+[guid]::NewGuid().ToString('N'))}
if(Test-Path -LiteralPath $ScratchRoot){throw 'No overwriting prior test evidence'}
$null=New-Item -ItemType Directory -Path $ScratchRoot
$pack=Join-Path $PSScriptRoot 'pack-sealed-release.ps1';$rows=[Collections.Generic.List[object]]::new()
function Case([string]$name,[scriptblock]$body,[string]$expected=''){
 $actual='';try{$null=& $body;if($expected){throw 'Expected refusal did not occur'}}catch{$actual=$_.Exception.Message;if(-not $expected -or $actual -notlike ('*'+$expected+'*')){throw}}
 $rows.Add(@{case=$name;passed=$true;actualRefusal=$actual})
}
Case 'all6bytes-provenance-matrix-and-source' {& $pack -PayloadRoot $PayloadRoot -EvidenceRoot $EvidenceRoot -OutputRoot (Join-Path $ScratchRoot 'not-created') -ValidateOnly}
$mutated=Join-Path $ScratchRoot 'mutated-payload';$null=New-Item -ItemType Directory -Path $mutated;Copy-Item -Path (Join-Path $PayloadRoot '*') -Destination $mutated -Recurse
[IO.File]::AppendAllText((Join-Path $mutated 'FindX.installed'),'X')
Case 'one-byte-payload-tamper-refused' {& $pack -PayloadRoot $mutated -EvidenceRoot $EvidenceRoot -OutputRoot (Join-Path $ScratchRoot 'not-created2') -ValidateOnly} 'Sealed payload mismatch'
$extra=Join-Path $ScratchRoot 'extra-payload';$null=New-Item -ItemType Directory -Path $extra;Copy-Item -Path (Join-Path $PayloadRoot '*') -Destination $extra -Recurse
[IO.File]::WriteAllText((Join-Path $extra 'unapproved.dll'),'must not enter package')
Case 'unapproved-extra-dll-refused' {& $pack -PayloadRoot $extra -EvidenceRoot $EvidenceRoot -OutputRoot (Join-Path $ScratchRoot 'not-created3') -ValidateOnly} 'Extra/missing candidate files'
$empty=Join-Path $ScratchRoot 'missing-proof';$null=New-Item -ItemType Directory -Path $empty
Case 'missing-exact-successful-matrix-refused' {& $pack -PayloadRoot $PayloadRoot -EvidenceRoot $empty -OutputRoot (Join-Path $ScratchRoot 'not-created4') -ValidateOnly} 'Exact successful matrix bytes'
if(-not $ValidationOnlyTests){
 Case 'official-compilation-off-tag-refused' {& $pack -PayloadRoot $PayloadRoot -EvidenceRoot $EvidenceRoot -OutputRoot (Join-Path $ScratchRoot 'not-official') -Production} 'Official compilation only'
 $plan=Get-Content -LiteralPath (Join-Path $repo 'release/2.5.3.json') -Raw -Encoding UTF8 | ConvertFrom-Json
 $expectedReady=$plan.publish_ready -and @($plan.gates | Where-Object {$_.required -and -not $_.passed}).Count -eq 0
 $policy=& python (Join-Path $repo 'scripts/check_release.py') --expected-version 2.5.3 --require-ready
 if(($LASTEXITCODE -eq 0) -ne $expectedReady){throw 'Publication checker no longer enforces unchanged policy'}
 $rows.Add(@{case='unchanged-required-gates-enforced';passed=$true;ready=$expectedReady;checker=$policy})
}
$result=[ordered]@{scope='sealed packaging tests only, no installer execution';cases=@($rows.ToArray());passed=$rows.Count;failed=0}
$save=if($env:GITHUB_ACTIONS -eq 'true'){Join-Path $env:RUNNER_TEMP 'sealed-test-results.json'}else{Join-Path $ScratchRoot 'results.json'}
$result | ConvertTo-Json -Depth 8 | Set-Content -Encoding UTF8 $save
Write-Output ('SEALED_NEGATIVE_REGRESSIONS_PASSED='+$rows.Count)

param([Parameter(Mandatory=$true)][string]$BuildRoot,[Parameter(Mandatory=$true)][string]$Destination)
Set-StrictMode -Version Latest
$ErrorActionPreference='Stop'
if($env:GITHUB_ACTIONS -ne 'true' -or $env:RUNNER_OS -ne 'Windows' -or $env:COMPUTERNAME -eq 'MZ'){throw 'Disposable Windows CI only'}
if(Test-Path $Destination){throw 'New payload destination required'}
$head=git rev-parse HEAD;if($LASTEXITCODE -or $head -cne $env:GITHUB_SHA -or @(git status --porcelain --untracked-files=no).Count){throw 'Clean exact source SHA required'}
$null=[IO.Directory]::CreateDirectory($Destination)
foreach($mapping in @(@('findx2-gui-native.exe','FindX.exe'),@('findx2-service.exe','findx2-service.exe'),@('findx2.exe','findx2.exe'),@('fx.exe','fx.exe'))){
 $src=Join-Path $BuildRoot $mapping[0];$dst=Join-Path $Destination $mapping[1];Copy-Item $src $dst
 if((Get-FileHash $src).Hash -cne (Get-FileHash $dst).Hash){throw 'Rebuilt payload copy changed'}
}
if((Get-Item (Join-Path $Destination 'FindX.exe')).VersionInfo.ProductVersion -cne '2.5.3'){throw 'Wrong final GUI version'}
$version=& (Join-Path $Destination 'findx2-service.exe') --version;if($LASTEXITCODE -or $version -cne 'findx2-service 2.5.3'){throw 'Wrong rebuilt engine version'}
[IO.File]::WriteAllBytes((Join-Path $Destination 'FindX.installed'),[byte[]](0x31,13,10))
$resources=Join-Path $Destination 'resources';$null=[IO.Directory]::CreateDirectory($resources)
$components=@(foreach($name in @('FindX.exe','findx2.exe','fx.exe','findx2-service.exe')){[ordered]@{name=$name;sourceCommit=$head;sha256=(Get-FileHash (Join-Path $Destination $name)).Hash.ToLowerInvariant()}})
[ordered]@{version='2.5.3';sourceCommit=$head;engineBaseline='v2.5.2 plus authorized durability changes';engineUnchanged=$false;components=$components}|ConvertTo-Json -Depth 10|Set-Content -Encoding UTF8 (Join-Path $resources 'component-manifest.json')
$files=@(Get-ChildItem $Destination -Recurse -File|ForEach-Object{[ordered]@{name=$_.FullName.Substring($Destination.Length+1);length=$_.Length;sha256=(Get-FileHash $_.FullName).Hash.ToLowerInvariant()}})
[ordered]@{version='2.5.3';gui='native-refined-r4';sourceCommit=$head;engineUnchanged=$false;files=$files}|ConvertTo-Json -Depth 10|Set-Content -Encoding UTF8 (Join-Path $Destination 'stage-manifest.json')

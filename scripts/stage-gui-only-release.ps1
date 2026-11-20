param([Parameter(Mandatory=$true)][string]$PublishedPayload,[Parameter(Mandatory=$true)][string]$Gui,[Parameter(Mandatory=$true)][string]$Destination)
Set-StrictMode -Version Latest
$ErrorActionPreference='Stop'
if($env:GITHUB_ACTIONS -ne 'true' -or $env:RUNNER_OS -ne 'Windows'){throw 'Release candidate staging is restricted to the authorized isolated Windows CI'}
if(Test-Path -LiteralPath $Destination){throw 'Candidate destination must be new, never overlay unknown files'}
$null=[IO.Directory]::CreateDirectory($Destination)
foreach($name in @('findx2.exe','fx.exe','findx2-service.exe')){
 Copy-Item -LiteralPath (Join-Path $PublishedPayload $name) -Destination (Join-Path $Destination $name)
 if((Get-FileHash (Join-Path $Destination $name)).Hash -ne (Get-FileHash (Join-Path $PublishedPayload $name)).Hash){throw 'Released engine/CLI bytes not identical'}
}
foreach($file in @(Get-ChildItem -LiteralPath $PublishedPayload -Filter '*.dll' -File)){Copy-Item -LiteralPath $file.FullName -Destination $Destination}
if(Test-Path -LiteralPath (Join-Path $PublishedPayload 'resources')){Copy-Item -LiteralPath (Join-Path $PublishedPayload 'resources') -Destination $Destination -Recurse}
Copy-Item -LiteralPath $Gui -Destination (Join-Path $Destination 'FindX.exe')
$guiVersion=[string](Get-Item -LiteralPath (Join-Path $Destination 'FindX.exe')).VersionInfo.ProductVersion
if($guiVersion -notmatch '^\d+\.\d+\.\d+$'){throw 'GUI ProductVersion is not x.y.z'}
[IO.File]::WriteAllBytes((Join-Path $Destination 'FindX.installed'),[byte[]](0x31,13,10))
$resources=Join-Path $Destination 'resources';$null=[IO.Directory]::CreateDirectory($resources)
$components=@(foreach($n in @('FindX.exe','findx2.exe','fx.exe','findx2-service.exe')){$p=Join-Path $Destination $n;[ordered]@{name=$n;version=(Get-Item $p).VersionInfo.ProductVersion;sha256=(Get-FileHash $p -Algorithm SHA256).Hash.ToLowerInvariant()}})
[ordered]@{version=$guiVersion;sourceCommit=$env:GITHUB_SHA;engineBaseline='v2.5.2';engineUnchanged=$true;guiDefault='installed marker: production findx2, never development snapshot';components=$components} | ConvertTo-Json -Depth 8 | Set-Content -Encoding UTF8 (Join-Path $resources 'component-manifest.json')
$files=@(Get-ChildItem -LiteralPath $Destination -Recurse -File | ForEach-Object{[ordered]@{name=$_.FullName.Substring($Destination.Length+1);length=$_.Length;sha256=(Get-FileHash $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant()}})
[ordered]@{version=$guiVersion;gui='native';sourceCommit=$env:GITHUB_SHA;engineBaseline='v2.5.2';files=$files} | ConvertTo-Json -Depth 8 | Set-Content -Encoding UTF8 (Join-Path $Destination 'stage-manifest.json')

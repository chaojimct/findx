# Pack an isolated-fixture 2.5.0 setup. Never installs, never touches official paths,
# and never compiles with /DUpgradeGatesApproved.
param(
  [string] $RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path,
  [string] $StageDir = "",
  [string] $OutputDir = "",
  [string] $FreshDefaultDir = "",
  [string] $Iscc = ""
)
$ErrorActionPreference = "Stop"
$RepoRoot = [IO.Path]::GetFullPath($RepoRoot).TrimEnd('\')
$free = (Get-PSDrive C).Free
if ($free -lt 1GB) { throw "C: free space $free is too low to pack an isolated setup safely." }

$versionMatch = [regex]::Match([IO.File]::ReadAllText((Join-Path $RepoRoot 'Cargo.toml')), '(?ms)\[workspace\.package\].*?^version\s*=\s*"([^"]+)"')
if (-not $versionMatch.Success) { throw 'Missing workspace product version' }
$version = $versionMatch.Groups[1].Value

if (-not $StageDir) { $StageDir = Join-Path $RepoRoot 'target\t156-packaging\candidate-stage' }
$StageDir = [IO.Path]::GetFullPath($StageDir)
if (-not $StageDir.StartsWith($RepoRoot + '\', [StringComparison]::OrdinalIgnoreCase)) {
  throw 'StageDir must be inside this checkout.'
}
foreach ($blocked in @(
    "${env:ProgramFiles}\FindX",
    "${env:ProgramFiles(x86)}\FindX",
    "$env:ProgramData\FindX"
  )) {
  if ($blocked -and ($StageDir.StartsWith(([IO.Path]::GetFullPath($blocked)).TrimEnd('\') + '\', [StringComparison]::OrdinalIgnoreCase) -or $StageDir.Equals([IO.Path]::GetFullPath($blocked), [StringComparison]::OrdinalIgnoreCase))) {
    throw "StageDir is an official path: $StageDir"
  }
}
foreach ($req in @('FindX.exe','findx2.exe','fx.exe','findx2-service.exe','pdf-worker.exe','FindX.installed')) {
  $p = Join-Path $StageDir $req
  if (-not (Test-Path -LiteralPath $p -PathType Leaf)) { throw "Stage is missing $req" }
}
$gui = Get-Item -LiteralPath (Join-Path $StageDir 'FindX.exe')
if ($gui.VersionInfo.ProductVersion -ne $version) {
  throw "Staged FindX.exe ProductVersion '$($gui.VersionInfo.ProductVersion)' does not match workspace $version"
}
foreach ($name in @('findx2.exe','fx.exe','findx2-service.exe','FindX.exe')) {
  $item = Get-Item -LiteralPath (Join-Path $StageDir $name)
  if ($item.VersionInfo.ProductVersion -ne $version -or $item.VersionInfo.FileVersion -ne $version) {
    throw "Staged $name is missing consistent PE version $version (File='$($item.VersionInfo.FileVersion)' Product='$($item.VersionInfo.ProductVersion)'). Rebuild companions with version resources before packing."
  }
}

if (-not $OutputDir) { $OutputDir = Join-Path $RepoRoot 'target\t156-packaging\setup' }
$OutputDir = [IO.Path]::GetFullPath($OutputDir)
if (-not $OutputDir.StartsWith($RepoRoot + '\', [StringComparison]::OrdinalIgnoreCase)) {
  throw 'OutputDir must be inside this checkout.'
}
if (-not $FreshDefaultDir) { $FreshDefaultDir = Join-Path $RepoRoot 'target\t156-packaging\fresh-install' }
$FreshDefaultDir = [IO.Path]::GetFullPath($FreshDefaultDir)
if (-not $FreshDefaultDir.StartsWith($RepoRoot + '\', [StringComparison]::OrdinalIgnoreCase)) {
  throw 'FreshDefaultDir must be inside this checkout; never Program Files.'
}

if (-not $Iscc) {
  $Iscc = Join-Path ${env:ProgramFiles(x86)} 'Inno Setup 6\ISCC.exe'
}
if (-not (Test-Path -LiteralPath $Iscc)) { throw "ISCC not found at $Iscc" }

$null = New-Item -ItemType Directory -Force -Path $OutputDir, $FreshDefaultDir
$iss = Join-Path $RepoRoot 'installer\FindX-isolated-fixture.iss'
$publishArg = '/DPublishDir=' + $StageDir.Replace('\', '/')
$outArg = '/DIsolatedOutputDir=' + $OutputDir.Replace('\', '/')
$defArg = '/DIsolatedDefaultDir=' + $FreshDefaultDir.Replace('\', '/')
$verArg = "/DMyAppVersion=$version"
Write-Host "ISCC isolated pack: version=$version stage=$StageDir output=$OutputDir"
& $Iscc /Q /DIsolatedFixturePackaging=1 $verArg $publishArg $outArg $defArg $iss
if ($LASTEXITCODE -ne 0) { throw "ISCC failed with exit $LASTEXITCODE" }

$setup = Join-Path $OutputDir "FindX-$version-isolated-fixture-setup.exe"
if (-not (Test-Path -LiteralPath $setup)) { throw "ISCC did not produce $setup" }
$hash = (Get-FileHash -LiteralPath $setup -Algorithm SHA256).Hash.ToLower()
$payload = @()
Get-ChildItem -LiteralPath $StageDir -File | ForEach-Object {
  $payload += [ordered]@{name=$_.Name; sha256=(Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash.ToLower(); length=$_.Length}
}
$manifest = [ordered]@{
  kind = 'isolated-fixture-setup'
  version = $version
  isolated = $true
  publish_ready = $false
  upgrade_gates_approved = $false
  installation_performed = $false
  official_appid_used = $false
  setup = [ordered]@{
    name = [IO.Path]::GetFileName($setup)
    path = $setup
    sha256 = $hash
    length = (Get-Item -LiteralPath $setup).Length
  }
  stage_dir = $StageDir
  default_dir = $FreshDefaultDir
  files = $payload
}
$manifestPath = Join-Path $OutputDir 'isolated-setup-manifest.json'
[IO.File]::WriteAllText($manifestPath, ($manifest | ConvertTo-Json -Depth 6), [Text.UTF8Encoding]::new($false))
Write-Host "Packed isolated setup $setup sha256=$hash length=$((Get-Item $setup).Length); not installed and not approved for publication."
$manifestPath

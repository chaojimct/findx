# Stage only; never installs, stops processes, or changes services.
param(
  [string] $RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path,
  [string] $ReleaseDir = "",
  [string] $StageDir = "",
  [switch] $PlanOnly
)
$ErrorActionPreference = "Stop"
$RepoRoot = [IO.Path]::GetFullPath($RepoRoot).TrimEnd('\')
$versionMatch = [regex]::Match([IO.File]::ReadAllText((Join-Path $RepoRoot 'Cargo.toml')), '(?ms)\[workspace\.package\].*?^version\s*=\s*"([^"]+)"')
if (-not $versionMatch.Success) { throw 'Missing workspace product version' }
$version = $versionMatch.Groups[1].Value
if ($ReleaseDir) {
  $candidates = @([IO.Path]::GetFullPath($ReleaseDir)) # Explicit means strict: never mix another build.
} else {
  $candidates = @((Join-Path $RepoRoot 'target\release'),(Join-Path $RepoRoot 'target\x86_64-pc-windows-msvc\release'))
}
$guiNames = @('findx2-gui-native.exe')
$rel=$null; $main=$null
foreach ($candidate in $candidates) {
  foreach ($name in $guiNames) {
    $file=Join-Path $candidate $name
    if (Test-Path -LiteralPath $file -PathType Leaf) { $rel=$candidate; $main=$file; break }
  }
  if ($rel) { break }
}
if (-not $rel) { throw "Missing native GUI in requested build directories; requires findx2-gui-native.exe (pinyin default feature)." }
$files=@(@{source=$main;name='FindX.exe'})
foreach ($name in @('findx2.exe','fx.exe','findx2-service.exe','pdf-worker.exe')) {
  $file=Join-Path $rel $name
  if (-not (Test-Path -LiteralPath $file -PathType Leaf)) { throw "Missing companion in SAME build directory: $file" }
  $files+=@{source=$file;name=$name}
}
Get-ChildItem -LiteralPath $rel -Filter '*.dll' -File | ForEach-Object { $files+=@{source=$_.FullName;name=$_.Name} }
# Preview workers re-exec FindX.exe; preserve any explicitly generated resources.
$res=Join-Path $rel 'resources'
if (Test-Path -LiteralPath $res -PathType Container) {
  Get-ChildItem -LiteralPath $res -Recurse -File | ForEach-Object { $files+=@{source=$_.FullName;name=('resources\'+$_.FullName.Substring($res.Length+1))} }
}
if (-not $StageDir) { $StageDir=Join-Path $RepoRoot 'installer\stage' }
$StageDir=[IO.Path]::GetFullPath($StageDir)
if (-not $StageDir.StartsWith($RepoRoot+'\',[StringComparison]::OrdinalIgnoreCase)) { throw 'StageDir must be inside this checkout; never target an installation or index directory.' }
$payload=@($files | ForEach-Object { [ordered]@{name=$_.name;sha256=(Get-FileHash -LiteralPath $_.source -Algorithm SHA256).Hash.ToLower();length=(Get-Item -LiteralPath $_.source).Length} })
# Official Inno writes this marker next to FindX.exe; staging stays inside the checkout and never installs.
$markerName='FindX.installed'
$markerBytes=[byte[]](0x31,0x0D,0x0A)
$markerSha=([BitConverter]::ToString([Security.Cryptography.SHA256]::Create().ComputeHash($markerBytes))).Replace('-','').ToLower()
$payload+=[ordered]@{name=$markerName;sha256=$markerSha;length=$markerBytes.Length;synthesized=$true}
$manifest=[ordered]@{version=$version;gui=$Gui;main_source=(Split-Path $main -Leaf);files=$payload;installation_performed=$false}
if ($PlanOnly) { $manifest | ConvertTo-Json -Depth 6; return }
if ($Gui -eq 'native' -and (Get-Item -LiteralPath $main).VersionInfo.ProductVersion -ne $version) { throw 'Native Windows PE ProductVersion does not match workspace version; rebuild candidate first.' }
if ($Gui -eq 'native') {
  foreach ($pair in @(@('findx2-service.exe', "findx2-service $version"), @('findx2.exe', "findx2 $version"), @('fx.exe', "findx2 $version"))) {
    $probe = & (Join-Path $rel $pair[0]) --version
    if ($LASTEXITCODE -ne 0 -or (($probe | Out-String).Trim() -cne $pair[1])) { throw "$($pair[0]) --version is not $($pair[1]); rebuild that binary from this commit." }
  }
}
if (Test-Path -LiteralPath $StageDir) {
  $old=Join-Path $StageDir 'stage-manifest.json'
  if (Test-Path -LiteralPath $old) {
    $previous=Get-Content -LiteralPath $old -Raw | ConvertFrom-Json
    $same=$previous.version -eq $version -and $previous.gui -eq $Gui -and $previous.files.Count -eq $payload.Count
    foreach ($f in $payload) {
      $at=Join-Path $StageDir $f.name
      if (-not (Test-Path -LiteralPath $at -PathType Leaf)) { $same=$false; break }
      if ((Get-FileHash -LiteralPath $at -Algorithm SHA256).Hash.ToLower() -ne $f.sha256) { $same=$false;break }
    }
    if ($same) { Write-Host "Verified existing identical stage: $StageDir"; return }
  }
  throw 'Stage already exists or is incomplete/different. Choose a fresh StageDir; no existing artifact is deleted or overwritten.'
}
$null=New-Item -ItemType Directory -Path $StageDir
foreach ($file in $files) {
  $dest=Join-Path $StageDir $file.name
  $null=New-Item -ItemType Directory -Force -Path (Split-Path $dest -Parent)
  # CreateNew is fail-closed even under a concurrent staging attempt.
  $input=[IO.File]::OpenRead($file.source)
  try { $output=[IO.File]::Open($dest,[IO.FileMode]::CreateNew);try {$input.CopyTo($output);$output.Flush($true)} finally {$output.Dispose()} } finally {$input.Dispose()}
}
$markerDest=Join-Path $StageDir $markerName
$markerOut=[IO.File]::Open($markerDest,[IO.FileMode]::CreateNew)
try {$markerOut.Write($markerBytes,0,$markerBytes.Length);$markerOut.Flush($true)} finally {$markerOut.Dispose()}
if ((Get-FileHash -LiteralPath $markerDest -Algorithm SHA256).Hash.ToLower() -ne $markerSha) { throw 'FindX.installed hash mismatch after staging' }
[IO.File]::WriteAllText((Join-Path $StageDir 'stage-manifest.json'),($manifest | ConvertTo-Json -Depth 6),[Text.UTF8Encoding]::new($false))
Write-Host "Staged $Gui $version at $StageDir; not installed and not approved for publication."

# Build FindXNative.dll (x64) via MSBuild — same pattern as ClipboardX native\ShellNavigate\build.ps1.
# Optional: cmake fallback if vcxproj fails (toolset mismatch).
param(
    [switch]$InstallBuildTools,
    [switch]$UseCmake
)

$ErrorActionPreference = 'Stop'
$here = $PSScriptRoot
$proj = Join-Path $here 'FindXNative.vcxproj'
$repoRoot = (Resolve-Path (Join-Path $here '..\..')).Path
$outDirs = @(
    (Join-Path $repoRoot 'src\FindX.Service\bin\Release\net8.0-windows'),
    (Join-Path $repoRoot 'src\FindX.Service\bin\Debug\net8.0-windows')
)

function Find-MsBuild {
    $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
    if (-not (Test-Path $vswhere)) { return $null }
    $inst = & $vswhere -latest -products * -requires Microsoft.Component.MSBuild -property installationPath 2>$null
    if (-not $inst) { return $null }
    foreach ($rel in @(
            'MSBuild\Current\Bin\MSBuild.exe',
            'MSBuild\17.0\Bin\MSBuild.exe',
            'MSBuild\16.0\Bin\MSBuild.exe')) {
        $p = Join-Path $inst $rel
        if (Test-Path $p) { return $p }
    }
    return $null
}

function Find-CMake {
    $cmd = Get-Command cmake -ErrorAction SilentlyContinue
    if ($cmd) { return $cmd.Source }
    foreach ($p in @(
            (Join-Path ${env:ProgramFiles} 'CMake\bin\cmake.exe'),
            (Join-Path ${env:ProgramFiles(x86)} 'CMake\bin\cmake.exe'))) {
        if (Test-Path $p) { return $p }
    }
    return $null
}

$msb = Find-MsBuild
if (-not $msb -and $InstallBuildTools) {
    Write-Host 'Installing VS 2022 Build Tools (C++) via winget...'
    winget install -e --id Microsoft.VisualStudio.2022.BuildTools --accept-package-agreements --accept-source-agreements `
        --override '--wait --passive --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended'
    $msb = Find-MsBuild
}

if (-not $msb) {
    Write-Host 'MSBuild not found. Install Visual Studio or Build Tools with C++, or run:'
    Write-Host '  .\build.ps1 -InstallBuildTools'
    exit 1
}

Write-Host "MSBuild: $msb"

$built = $false
if (-not $UseCmake) {
    & $msb $proj /p:Configuration=Release /p:Platform=x64 /v:m
    if ($LASTEXITCODE -eq 0) { $built = $true }
    else {
        Write-Host 'Release|x64 MSBuild failed. If PlatformToolset v143 is missing, try v142 in FindXNative.vcxproj or use -UseCmake.'
    }
}

if (-not $built -and $UseCmake) {
    $cmake = Find-CMake
    if (-not $cmake) {
        Write-Host 'cmake not found for -UseCmake.'
        exit 1
    }
    $srcRoot = Join-Path $repoRoot 'src\FindX.Native'
    $buildDir = Join-Path $srcRoot 'build'
    foreach ($gen in @('Visual Studio 18 2026', 'Visual Studio 17 2022', 'Visual Studio 16 2019')) {
        & $cmake -S $srcRoot -B $buildDir -G $gen -A x64 2>&1 | Out-Host
        if ($LASTEXITCODE -eq 0) {
            & $cmake --build $buildDir --config Release --parallel
            $dllC = Join-Path $buildDir 'Release\FindXNative.dll'
            if (Test-Path $dllC) {
                $outNative = Join-Path $here 'bin\x64\Release'
                New-Item -ItemType Directory -Force -Path $outNative | Out-Null
                Copy-Item $dllC (Join-Path $outNative 'FindXNative.dll') -Force
                $built = $true
            }
            break
        }
        if (Test-Path $buildDir) { Remove-Item -Recurse -Force $buildDir }
    }
}

$dll64 = Join-Path $here 'bin\x64\Release\FindXNative.dll'
if (-not (Test-Path $dll64)) {
    Write-Host "Build failed: $dll64 not found."
    exit 1
}

foreach ($outNet in $outDirs) {
    New-Item -ItemType Directory -Force -Path $outNet | Out-Null
    Copy-Item $dll64 (Join-Path $outNet 'FindXNative.dll') -Force
    Write-Host "Copied to: $outNet"
}

Write-Host "OK: $dll64"

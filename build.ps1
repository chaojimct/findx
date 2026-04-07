param(
    [string]$Version = "0.1.0",
    [switch]$SkipRust,
    [switch]$SkipInstaller,
    [switch]$Clean
)

$ErrorActionPreference = "Stop"
$Root = $PSScriptRoot
$PublishDir = Join-Path $Root "publish"
$DistDir = Join-Path $Root "dist"

Write-Host "=== FindX Build Script v$Version ===" -ForegroundColor Cyan

# ── 清理 ──
if ($Clean -or !(Test-Path $PublishDir)) {
    if (Test-Path $PublishDir) { Remove-Item -Recurse -Force $PublishDir }
    if (Test-Path $DistDir) { Remove-Item -Recurse -Force $DistDir }
}
New-Item -ItemType Directory -Force -Path $PublishDir | Out-Null
New-Item -ItemType Directory -Force -Path $DistDir | Out-Null

# ── 1. Rust Engine ──
if (-not $SkipRust) {
    Write-Host "`n[1/4] Building Rust engine..." -ForegroundColor Yellow
    Push-Location (Join-Path $Root "native\findx-engine")
    cargo build --release
    if ($LASTEXITCODE -ne 0) { throw "Cargo build failed" }
    Pop-Location
} else {
    Write-Host "`n[1/4] Skipping Rust engine (--SkipRust)" -ForegroundColor DarkGray
}

# ── 2. dotnet publish: Service ──
Write-Host "`n[2/4] Publishing FindX.Service..." -ForegroundColor Yellow
$ServiceOut = Join-Path $PublishDir "service"
dotnet publish (Join-Path $Root "src\FindX.Service\FindX.Service.csproj") `
    -c Release `
    -o $ServiceOut `
    -p:Version=$Version `
    --no-self-contained
if ($LASTEXITCODE -ne 0) { throw "Service publish failed" }

# ── 3. dotnet publish: CLI ──
Write-Host "`n[3/4] Publishing FindX.Cli..." -ForegroundColor Yellow
$CliOut = Join-Path $PublishDir "cli"
dotnet publish (Join-Path $Root "src\FindX.Cli\FindX.Cli.csproj") `
    -c Release `
    -o $CliOut `
    -p:Version=$Version `
    --no-self-contained
if ($LASTEXITCODE -ne 0) { throw "CLI publish failed" }

# ── 4. Inno Setup ──
if (-not $SkipInstaller) {
    Write-Host "`n[4/4] Building installer..." -ForegroundColor Yellow
    $Iscc = $null
    $candidates = @(
        "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe",
        "${env:ProgramFiles}\Inno Setup 6\ISCC.exe",
        "ISCC.exe"
    )
    foreach ($c in $candidates) {
        if (Get-Command $c -ErrorAction SilentlyContinue) { $Iscc = $c; break }
        if (Test-Path $c) { $Iscc = $c; break }
    }

    if ($Iscc) {
        & $Iscc /DMyAppVersion=$Version (Join-Path $Root "installer\FindX.iss")
        if ($LASTEXITCODE -ne 0) { throw "Inno Setup build failed" }
        Write-Host "Installer: $DistDir\FindX-$Version-setup.exe" -ForegroundColor Green
    } else {
        Write-Host "Inno Setup 未安装，跳过安装包生成。可手动运行: iscc /DMyAppVersion=$Version installer\FindX.iss" -ForegroundColor DarkYellow
    }
} else {
    Write-Host "`n[4/4] Skipping installer (--SkipInstaller)" -ForegroundColor DarkGray
}

Write-Host "`n=== Build complete ===" -ForegroundColor Green
Write-Host "Service: $ServiceOut"
Write-Host "CLI:     $CliOut"

# Build the PlayDrop Windows installer.
#
# Usage (from any directory, in PowerShell 5.1 or 7):
#   packaging\windows\build-windows.ps1            # cargo build, then iscc
#   packaging\windows\build-windows.ps1 -NoBuild   # reuse existing binaries
#
# Needs: Rust with the x86_64-pc-windows-msvc target (default on Windows with
# the MSVC build tools), and Inno Setup 6.3+ (winget install JRSoftware.InnoSetup).
# Output: packaging\windows\dist\PlayDrop-<version>-x64-setup.exe

[CmdletBinding()]
param(
    [switch]$NoBuild
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$Here = $PSScriptRoot
$Repo = (Resolve-Path (Join-Path $Here '..\..')).Path
$Target = 'x86_64-pc-windows-msvc'
$BinDir = Join-Path $Repo "target\$Target\release"

# Version from [workspace.package] in the root Cargo.toml.
$cargoToml = Get-Content (Join-Path $Repo 'Cargo.toml') -Raw
$section = [regex]::Match($cargoToml, '(?ms)^\[workspace\.package\](.*?)(^\[|\z)')
$versionMatch = [regex]::Match($section.Groups[1].Value, '(?m)^version\s*=\s*"([^"]+)"')
if (-not $versionMatch.Success) { throw 'Could not read the workspace version from Cargo.toml' }
$Version = $versionMatch.Groups[1].Value

if (-not $NoBuild) {
    & cargo build --release --locked --target $Target `
        --manifest-path (Join-Path $Repo 'Cargo.toml') `
        -p playdropd -p playdrop-desktop
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed ($LASTEXITCODE)" }
}

foreach ($exe in 'playdropd.exe', 'playdrop-desktop.exe') {
    if (-not (Test-Path (Join-Path $BinDir $exe))) { throw "Missing $BinDir\$exe" }
}

# Find ISCC.exe: PATH first, then the default install folders.
$iscc = (Get-Command 'iscc.exe' -ErrorAction SilentlyContinue).Source
if (-not $iscc) {
    $candidates = @(
        (Join-Path ${env:ProgramFiles(x86)} 'Inno Setup 6\ISCC.exe'),
        (Join-Path $env:ProgramFiles 'Inno Setup 6\ISCC.exe'),
        (Join-Path $env:LOCALAPPDATA 'Programs\Inno Setup 6\ISCC.exe')
    )
    $iscc = $candidates | Where-Object { $_ -and (Test-Path $_) } | Select-Object -First 1
}
if (-not $iscc) { throw 'ISCC.exe not found. Install Inno Setup 6: winget install JRSoftware.InnoSetup' }

& $iscc "/DAppVersion=$Version" "/DBinDir=$BinDir" (Join-Path $Here 'playdrop.iss')
if ($LASTEXITCODE -ne 0) { throw "iscc failed ($LASTEXITCODE)" }

Write-Host "Built $(Join-Path $Here "dist\PlayDrop-$Version-x64-setup.exe")"

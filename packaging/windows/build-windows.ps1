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
    # The repo's .cargo/config.toml links the C runtime into every .exe (+crt-static); without
    # it the programs need vcruntime140.dll, which a fresh Windows lacks. A RUSTFLAGS or
    # CARGO_ENCODED_RUSTFLAGS variable replaces those flags without a word, so refuse to build
    # with one set rather than ship programs that do not start.
    foreach ($name in 'RUSTFLAGS', 'CARGO_ENCODED_RUSTFLAGS') {
        # Set at all, even empty, is enough for cargo to ignore the config's flags.
        if (Test-Path "Env:$name") {
            throw "$name is set; it would replace the flags in .cargo/config.toml (+crt-static). Unset it and build again."
        }
    }
    # cargo reads .cargo/config.toml from the current directory upwards, not from the folder
    # of --manifest-path, so build from the repo root whatever folder this was started in.
    Push-Location $Repo
    try {
        & cargo build --release --locked --target $Target `
            --manifest-path (Join-Path $Repo 'Cargo.toml') `
            -p playdropd -p playdrop-desktop -p playdrop-cli
        if ($LASTEXITCODE -ne 0) { throw "cargo build failed ($LASTEXITCODE)" }
    } finally {
        Pop-Location
    }
}

# playdrop.exe is the command line from the playdrop-cli crate (docs/guide/cli.md).
foreach ($exe in 'playdropd.exe', 'playdrop-desktop.exe', 'playdrop.exe') {
    if (-not (Test-Path (Join-Path $BinDir $exe))) { throw "Missing $BinDir\$exe" }
}

# Find ISCC.exe: PATH first, then the default install folders.
# (Reading .Source off a null result throws under Set-StrictMode, so test the command first.)
$isccCommand = Get-Command 'iscc.exe' -ErrorAction SilentlyContinue
$iscc = if ($isccCommand) { $isccCommand.Source } else { $null }
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

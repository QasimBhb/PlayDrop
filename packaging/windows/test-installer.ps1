# Install the PlayDrop installer silently, check what it put on the machine, then uninstall
# and check that it took everything back out.
#
# Usage (elevated PowerShell 5.1 or 7, after build-windows.ps1):
#   packaging\windows\test-installer.ps1
#   packaging\windows\test-installer.ps1 -Installer path\to\setup.exe
#
# CI runs this on GitHub's Windows runner after every installer build. It changes the
# machine (Program Files, the system PATH, the firewall), so do not run it on a machine
# where PlayDrop is installed and in use: the last step uninstalls it.
#
# What it proves: the three programs land where the guides say, `playdrop` is on the system
# PATH and runs, the firewall rules are scoped to playdropd.exe (SR-31), the Start Menu
# shortcut carries the AppUserModelID the daemon's toasts use, and uninstall removes the
# files, the PATH entry, the rules, the shortcut and the sign-in entry. What it cannot prove:
# anything that needs a person at the screen (the wizard pages, SmartScreen, the tray).

[CmdletBinding()]
param(
    [string]$Installer
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$Here = $PSScriptRoot
$Repo = (Resolve-Path (Join-Path $Here '..\..')).Path

# Version from [workspace.package] in the root Cargo.toml.
$cargoToml = Get-Content (Join-Path $Repo 'Cargo.toml') -Raw
$section = [regex]::Match($cargoToml, '(?ms)^\[workspace\.package\](.*?)(^\[|\z)')
$versionMatch = [regex]::Match($section.Groups[1].Value, '(?m)^version\s*=\s*"([^"]+)"')
if (-not $versionMatch.Success) { throw 'Could not read the workspace version from Cargo.toml' }
$Version = $versionMatch.Groups[1].Value

if (-not $Installer) { $Installer = Join-Path $Here "dist\PlayDrop-$Version-x64-setup.exe" }
if (-not (Test-Path $Installer)) { throw "Missing installer: $Installer" }

$App = Join-Path $env:ProgramFiles 'PlayDrop'
$Bin = Join-Path $App 'bin'
$Cli = Join-Path $Bin 'playdrop.exe'
$RunKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'
$Log = Join-Path ([System.IO.Path]::GetTempPath()) 'playdrop-install.log'
# The all-users Start Menu (an admin install), and the toast ID from crates/playdropd/src/notify.rs.
$Shortcut = Join-Path ([Environment]::GetFolderPath('CommonPrograms')) 'PlayDrop\PlayDrop.lnk'
$Aumid = 'org.playdrop.PlayDrop'

$script:Failures = @()
function Check([string]$What, [bool]$Ok) {
    if ($Ok) {
        Write-Host "ok    $What"
    } else {
        Write-Host "FAIL  $What"
        $script:Failures += $What
    }
}

function Test-OnSystemPath([string]$Dir) {
    $entries = [Environment]::GetEnvironmentVariable('Path', 'Machine') -split ';'
    return [bool]($entries | Where-Object { $_ -and ($_.TrimEnd('\') -ieq $Dir.TrimEnd('\')) })
}

# The firewall rules as netsh prints them, or an empty string when none is named PlayDrop.
# netsh exits non-zero for "no rule matches", and Windows PowerShell 5.1 turns anything a
# program writes to stderr into an error under 'Stop', so both run with 'Continue' here.
function Get-FirewallRules {
    $ErrorActionPreference = 'Continue'
    $out = & netsh advfirewall firewall show rule name=PlayDrop verbose 2>&1 | Out-String
    if ($LASTEXITCODE -ne 0) { return '' }
    return $out
}

# Runs the installed command line and returns its exit code; its output is not needed.
function Get-CliExit([string[]]$Arguments) {
    $ErrorActionPreference = 'Continue'
    & $Cli @Arguments 2>&1 | Out-Null
    return $LASTEXITCODE
}

# The System.AppUserModel.ID property of a shortcut, read through the Shell, or '' when the
# shortcut or the property is missing.
function Get-ShortcutAumid([string]$Path) {
    if (-not (Test-Path $Path)) { return '' }
    $shell = New-Object -ComObject Shell.Application
    $item = $shell.NameSpace((Split-Path $Path)).ParseName((Split-Path $Path -Leaf))
    if (-not $item) { return '' }
    $id = $item.ExtendedProperty('System.AppUserModel.ID')
    if ($null -eq $id) { return '' }
    return [string]$id
}

function Test-RunValue {
    $props = Get-ItemProperty -Path $RunKey -ErrorAction SilentlyContinue
    return [bool]($props -and ($props.PSObject.Properties.Name -contains 'PlayDrop'))
}

if (Test-Path $App) { throw "$App already exists; this test only runs on a machine without PlayDrop" }

Write-Host "Installing $Installer"
$setup = Start-Process -FilePath $Installer -Wait -PassThru `
    -ArgumentList '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', "/LOG=`"$Log`""
if ($setup.ExitCode -ne 0) {
    if (Test-Path $Log) { Get-Content $Log -Tail 60 }
    throw "The installer exited with $($setup.ExitCode)"
}

Check 'playdropd.exe is installed' (Test-Path (Join-Path $App 'playdropd.exe'))
Check 'playdrop-desktop.exe is installed' (Test-Path (Join-Path $App 'playdrop-desktop.exe'))
Check 'bin\playdrop.exe is installed' (Test-Path $Cli)
Check 'bin is on the system PATH' (Test-OnSystemPath $Bin)
Check 'the daemon and the window are not on PATH' (-not (Test-OnSystemPath $App))

if (Test-Path $Cli) {
    $shown = (& $Cli --version | Out-String).Trim()
    Check "playdrop --version prints the release version (got '$shown')" `
        (($LASTEXITCODE -eq 0) -and ($shown -eq "playdrop $Version"))

    # A silent install does not start the daemon, so the CLI must report exactly that:
    # exit code 3, "daemon not running" (docs/guide/cli.md).
    $statusExit = Get-CliExit @('status')
    Check "playdrop status exits 3 with no daemon (got $statusExit)" ($statusExit -eq 3)
}

$rules = Get-FirewallRules
$daemonPath = [regex]::Escape((Join-Path $App 'playdropd.exe'))
Check 'two firewall rules are scoped to playdropd.exe' `
    ([regex]::Matches($rules, $daemonPath, 'IgnoreCase').Count -eq 2)
Check 'the sign-in entry is set' (Test-RunValue)
Check 'the Start Menu shortcut exists' (Test-Path $Shortcut)
$shownAumid = Get-ShortcutAumid $Shortcut
Check "the Start Menu shortcut carries AppUserModelID $Aumid (got '$shownAumid')" ($shownAumid -eq $Aumid)

# A second install over the first must not add the PATH entry twice.
$again = Start-Process -FilePath $Installer -Wait -PassThru `
    -ArgumentList '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART'
Check "installing again succeeds (exit $($again.ExitCode))" ($again.ExitCode -eq 0)
$count = @([Environment]::GetEnvironmentVariable('Path', 'Machine') -split ';' |
    Where-Object { $_ -and ($_.TrimEnd('\') -ieq $Bin) }).Count
Check "bin is on the system PATH once after a second install (found $count)" ($count -eq 1)

Write-Host 'Uninstalling'
$uninstaller = Join-Path $App 'unins000.exe'
if (-not (Test-Path $uninstaller)) { throw "Missing $uninstaller" }
$removal = Start-Process -FilePath $uninstaller -Wait -PassThru `
    -ArgumentList '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART'
Check "the uninstaller succeeds (exit $($removal.ExitCode))" ($removal.ExitCode -eq 0)

# The uninstaller finishes from a copy of itself in the temp folder, so give it a moment.
$deadline = (Get-Date).AddSeconds(60)
while ((Test-Path $App) -and ((Get-Date) -lt $deadline)) { Start-Sleep -Milliseconds 500 }

Check 'the program folder is gone' (-not (Test-Path $App))
Check 'bin is off the system PATH' (-not (Test-OnSystemPath $Bin))
Check 'the firewall rules are gone' ((Get-FirewallRules) -eq '')
Check 'the sign-in entry is gone' (-not (Test-RunValue))
Check 'the Start Menu shortcut is gone' (-not (Test-Path $Shortcut))

if ($script:Failures.Count -gt 0) {
    if (Test-Path $Log) { Get-Content $Log -Tail 60 }
    throw "Installer test failed: $($script:Failures -join '; ')"
}
Write-Host 'Installer test passed'
# The last program run above was netsh reporting "no rule", which is the wanted answer but
# a non-zero exit code. GitHub's pwsh shell ends a step with that code unless told otherwise.
exit 0

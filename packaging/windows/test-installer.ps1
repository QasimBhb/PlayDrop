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
# files, the PATH entry, the rules, the shortcut and the sign-in entry.
#
# It also runs what got installed, because a release once installed fine and then did not
# open on a real PC:
#   - no installed .exe imports the C runtime DLLs (VCRUNTIME*, MSVCP*, api-ms-win-crt-*,
#     ucrtbase), which a fresh Windows does not have (.cargo/config.toml links it in), nor
#     any DLL that is not on the list of Windows system DLLs below;
#   - playdropd.exe and playdrop-desktop.exe are GUI programs, playdrop.exe a console one;
#   - the installed daemon starts and stays up, `playdrop status` reaches it, a second
#     daemon exits quietly, and the window stays open and connects to it; neither logs a
#     FATAL or PANIC line;
#   - a daemon that cannot start exits non-zero and says why in its log, without waiting
#     for anyone to close an error box;
#   - /RELAUNCH=1 (what the update check passes when the window was open) starts the daemon
#     and the window again after a silent install, and the window connects;
#     /RELAUNCH=daemon starts the daemon only.
# What it cannot prove: anything that needs a person at the screen (the wizard pages,
# SmartScreen, the tray menu, what the window looks like).

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
$Shortcut = Join-Path ([Environment]::GetFolderPath('CommonPrograms')) 'PlayDrop.lnk'
# What an install before 0.15.0 had: a Start Menu folder, and the window under its build name.
$OldShortcutFolder = Join-Path ([Environment]::GetFolderPath('CommonPrograms')) 'PlayDrop'
$Aumid = 'org.playdrop.PlayDrop'
$Daemon = Join-Path $App 'playdropd.exe'
$Window = Join-Path $App 'PlayDrop.exe'
$OldWindow = Join-Path $App 'playdrop-desktop.exe'
# Where both programs log on Windows (crates/playdrop-platform/src/startup.rs).
$LogDir = Join-Path $env:LOCALAPPDATA 'PlayDrop\logs'
$DaemonLog = Join-Path $LogDir 'playdropd.log'
$WindowLog = Join-Path $LogDir 'playdrop-desktop.log'
# Scratch folders for the test daemons, so the runner's own PlayDrop data is never used.
$Work = Join-Path ([System.IO.Path]::GetTempPath()) "playdrop-installer-test-$PID"
# The C runtime DLLs no installed program may import (the Visual C++ redistributable, the
# OpenMP and concurrency runtimes, the universal CRT and its API sets).
$CrtDlls = '^(vcruntime.*|msvcp.*|msvcr\d+.*|vcomp.*|concrt.*|api-ms-win-crt-.*|ucrtbase.*)\.dll$'

# Every DLL an installed program may import: ones that ship in System32 on every Windows 10
# and 11. Anything else (a redistributable, a GPU SDK DLL, a third-party library) would make
# a program fail to start on a fresh PC, which is what this test is for. Built from what can
# reach the three programs: Rust std (kernel32, ntdll, ws2_32, userenv, bcryptprimitives,
# the synch API set), the `windows` and `windows-sys` features of playdropd, playdrop-platform,
# playdrop-ipc, playdrop-core and apps/desktop, and winit, tray-icon, muda, rfd, arboard,
# glutin (opengl32), accesskit and if-addrs. API sets (api-ms-win-*) are allowed by pattern,
# except the C runtime ones above.
# To extend it: a CI failure names the unexpected DLL. Add its lower-case name here only if it
# is in C:\Windows\System32 on a clean Windows 10 install; never a redistributable such as
# vcruntime140, d3dx9_*, dxcompiler or xinput1_3, which needs a different fix (link it in,
# load it at run time, or drop the dependency).
$SystemDlls = @(
    'advapi32.dll', 'authz.dll', 'avrt.dll', 'bcp47mrm.dll', 'bcrypt.dll',
    'bcryptprimitives.dll', 'cfgmgr32.dll', 'clfsw32.dll', 'combase.dll', 'comctl32.dll',
    'comdlg32.dll', 'crypt32.dll', 'cryptbase.dll', 'cryptnet.dll', 'd2d1.dll', 'd3d11.dll',
    'd3d12.dll', 'd3dcompiler_47.dll', 'dbghelp.dll', 'dcomp.dll', 'dinput8.dll', 'dnsapi.dll',
    'dwmapi.dll', 'dwrite.dll', 'dxgi.dll', 'dxva2.dll', 'fontsub.dll', 'fwpuclnt.dll',
    'gdi32.dll', 'gdiplus.dll', 'glu32.dll', 'hid.dll', 'icu.dll', 'icuin.dll', 'icuuc.dll',
    'imagehlp.dll', 'imm32.dll', 'iphlpapi.dll', 'kernel32.dll', 'kernelbase.dll',
    'ktmw32.dll', 'mrmsupport.dll', 'msimg32.dll', 'mswsock.dll', 'ncrypt.dll',
    'netapi32.dll', 'normaliz.dll', 'ntdll.dll', 'ole32.dll', 'oleacc.dll', 'oleaut32.dll',
    'opengl32.dll', 'pdh.dll', 'powrprof.dll', 'propsys.dll', 'rpcrt4.dll', 'rtworkq.dll',
    'secur32.dll', 'setupapi.dll', 'shcore.dll', 'shell32.dll', 'shlwapi.dll', 'sspicli.dll',
    'uiautomationcore.dll', 'urlmon.dll', 'user32.dll', 'userenv.dll', 'usp10.dll',
    'uxtheme.dll', 'version.dll', 'winhttp.dll', 'wininet.dll', 'winmm.dll', 'wintrust.dll',
    'wldp.dll', 'ws2_32.dll', 'wtsapi32.dll'
)

# A fatal error makes the programs show a message box; on the runner nobody could close it
# and the step would hang until its timeout. Every program started below inherits this.
$env:PLAYDROP_NO_FATAL_DIALOG = '1'

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

# How many icons a program file carries in its own resources (0 for a file Windows would
# show with the generic program icon). ExtractIconEx with index -1 only counts.
Add-Type -Namespace PlayDropTest -Name Shell -MemberDefinition @'
[DllImport("shell32.dll", CharSet = CharSet.Unicode)]
public static extern uint ExtractIconEx(string file, int index, IntPtr large, IntPtr small, uint count);
'@
function Get-IconGroupCount([string]$Path) {
    return [int][PlayDropTest.Shell]::ExtractIconEx($Path, -1, [IntPtr]::Zero, [IntPtr]::Zero, 0)
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

# The parts of a PE image this test reads: the subsystem field, the data directories and the
# section table (to turn an RVA into a file offset).
function Get-PeImage([string]$Path) {
    $bytes = [System.IO.File]::ReadAllBytes($Path)
    $pe = [BitConverter]::ToInt32($bytes, 0x3C)
    if ([BitConverter]::ToUInt32($bytes, $pe) -ne 0x00004550) { throw "$Path is not a PE image" }
    $coff = $pe + 4
    $sectionCount = [int][BitConverter]::ToUInt16($bytes, $coff + 2)
    $optionalSize = [int][BitConverter]::ToUInt16($bytes, $coff + 16)
    $optional = $coff + 20
    $magic = [BitConverter]::ToUInt16($bytes, $optional)
    # PE32+ (0x20B, every 64-bit image) has a longer optional header before the directories.
    $directories = if ($magic -eq 0x20B) { $optional + 112 } else { $optional + 96 }
    $directoryCount = [int][BitConverter]::ToUInt32($bytes, $directories - 4)
    $sections = @()
    $table = $optional + $optionalSize
    for ($i = 0; $i -lt $sectionCount; $i++) {
        $at = $table + 40 * $i
        $sections += [pscustomobject]@{
            VirtualSize    = [int64][BitConverter]::ToUInt32($bytes, $at + 8)
            VirtualAddress = [int64][BitConverter]::ToUInt32($bytes, $at + 12)
            RawSize        = [int64][BitConverter]::ToUInt32($bytes, $at + 16)
            RawPointer     = [int64][BitConverter]::ToUInt32($bytes, $at + 20)
        }
    }
    return [pscustomobject]@{
        Bytes          = $bytes
        Subsystem      = [int][BitConverter]::ToUInt16($bytes, $optional + 68)
        Directories    = $directories
        DirectoryCount = $directoryCount
        Sections       = $sections
    }
}

function Convert-RvaToOffset($Image, [int64]$Rva) {
    foreach ($section in $Image.Sections) {
        $span = [Math]::Max($section.VirtualSize, $section.RawSize)
        if (($Rva -ge $section.VirtualAddress) -and ($Rva -lt ($section.VirtualAddress + $span))) {
            return [int]($Rva - $section.VirtualAddress + $section.RawPointer)
        }
    }
    throw ('RVA 0x{0:X} lies in no section' -f $Rva)
}

function Read-AsciiZ([byte[]]$Bytes, [int]$Offset) {
    $end = [Array]::IndexOf($Bytes, [byte]0, $Offset)
    if ($end -lt $Offset) { throw "unterminated name at offset $Offset" }
    return [System.Text.Encoding]::ASCII.GetString($Bytes, $Offset, $end - $Offset)
}

# The DLLs an image imports, normal and delay-loaded, read from its import directories. Used
# always, and checked against dumpbin where the runner has it, so the check never depends on
# one tool alone.
function Get-PeImports([string]$Path) {
    $image = Get-PeImage $Path
    $bytes = $image.Bytes
    $names = New-Object System.Collections.Generic.List[string]
    # Directory 1, the import table: 20-byte descriptors, the DLL name RVA at +12, ended by
    # an all-zero descriptor.
    if ($image.DirectoryCount -gt 1) {
        $rva = [int64][BitConverter]::ToUInt32($bytes, $image.Directories + 8)
        if ($rva -ne 0) {
            $at = Convert-RvaToOffset $image $rva
            while ($true) {
                $lookup = [BitConverter]::ToUInt32($bytes, $at)
                $nameRva = [int64][BitConverter]::ToUInt32($bytes, $at + 12)
                if (($lookup -eq 0) -and ($nameRva -eq 0)) { break }
                $names.Add((Read-AsciiZ $bytes (Convert-RvaToOffset $image $nameRva)))
                $at += 20
            }
        }
    }
    # Directory 13, delay-load imports: 32-byte descriptors, the DLL name RVA at +4.
    if ($image.DirectoryCount -gt 13) {
        $rva = [int64][BitConverter]::ToUInt32($bytes, $image.Directories + 13 * 8)
        if ($rva -ne 0) {
            $at = Convert-RvaToOffset $image $rva
            while ($true) {
                $nameRva = [int64][BitConverter]::ToUInt32($bytes, $at + 4)
                if ($nameRva -eq 0) { break }
                $names.Add((Read-AsciiZ $bytes (Convert-RvaToOffset $image $nameRva)))
                $at += 32
            }
        }
    }
    return $names.ToArray()
}

# dumpbin.exe from the newest Visual Studio with the C++ tools, found through vswhere, or
# $null when the machine has none.
function Find-Dumpbin {
    $ErrorActionPreference = 'Continue'
    $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
    if (-not (Test-Path $vswhere)) { return $null }
    $found = @(& $vswhere -latest -products '*' -requires 'Microsoft.VisualStudio.Component.VC.Tools.x86.x64' `
            -find 'VC\Tools\MSVC\**\bin\Hostx64\x64\dumpbin.exe' 2>$null)
    if ($found.Count -eq 0) { return $null }
    return [string]$found[0]
}

function Get-DumpbinImports([string]$Dumpbin, [string]$Path) {
    $ErrorActionPreference = 'Continue'
    $out = & $Dumpbin /NOLOGO /DEPENDENTS $Path 2>&1 | Out-String
    if ($LASTEXITCODE -ne 0) { throw "dumpbin failed on $Path ($LASTEXITCODE)" }
    # The DLL lines are the only ones indented by four spaces that end in .dll.
    return @([regex]::Matches($out, '(?m)^ {4}(\S+\.dll)\s*$', 'IgnoreCase') |
        ForEach-Object { $_.Groups[1].Value })
}

# A free TCP port on this machine for a test daemon.
function Get-FreePort {
    $listener = [System.Net.Sockets.TcpListener]::new([System.Net.IPAddress]::Any, 0)
    $listener.Start()
    $port = ([System.Net.IPEndPoint]$listener.LocalEndpoint).Port
    $listener.Stop()
    return $port
}

# Starts a program without waiting for it. Reading .Handle at once keeps ExitCode readable
# later in Windows PowerShell 5.1.
function Start-Program([string]$Path, [string[]]$Arguments) {
    if ($Arguments -and $Arguments.Count -gt 0) {
        $process = Start-Process -FilePath $Path -ArgumentList $Arguments -PassThru
    } else {
        $process = Start-Process -FilePath $Path -PassThru
    }
    $null = $process.Handle
    return $process
}

# Start-Process joins arguments with spaces and quotes nothing, so paths are quoted here.
function Quote([string]$Text) { return "`"$Text`"" }

# How many processes of that name run. A count, never a list: an empty list comes back from
# a function as $null, and reading .Count off that is an error under Set-StrictMode.
function Get-Running([string]$Name) {
    return [int]@(Get-Process -Name $Name -ErrorAction SilentlyContinue).Count
}

# The running installed windows. The window is PlayDrop.exe and the command line is
# bin\playdrop.exe, and process names ignore case, so the window is told apart by its path.
function Get-WindowProcesses {
    return @(Get-Process -Name 'PlayDrop' -ErrorAction SilentlyContinue |
        Where-Object { $_.Path -and ($_.Path -ieq $Window) })
}

function Get-WindowRunning {
    return [int]@(Get-WindowProcesses).Count
}

# Stops every installed daemon and window and waits until they are gone.
function Stop-PlayDrop {
    Get-WindowProcesses | Stop-Process -Force -ErrorAction SilentlyContinue
    Get-Process -Name 'playdropd' -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
    $deadline = (Get-Date).AddSeconds(15)
    while ((((Get-Running 'playdropd') + (Get-WindowRunning)) -gt 0) -and
        ((Get-Date) -lt $deadline)) {
        Start-Sleep -Milliseconds 200
    }
}

# Prints a log into the CI output, or says it is missing.
function Show-Log([string]$Path) {
    Write-Host "----- $Path"
    if (Test-Path $Path) {
        Get-Content $Path | ForEach-Object { Write-Host $_ }
    } else {
        Write-Host '(no such file)'
    }
    Write-Host '-----'
}

# The lines of a log that report a fatal error or a panic (startup.rs FATAL_MARKER and
# PANIC_MARKER), as one string; empty when there are none or there is no log.
function Get-FatalLines([string]$Path) {
    if (-not (Test-Path $Path)) { return '' }
    return (@(Select-String -Path $Path -Pattern ' (FATAL|PANIC) ' -CaseSensitive |
            ForEach-Object { $_.Line }) -join "`n")
}

# Whether an imported DLL is one every Windows has (see $SystemDlls).
function Test-SystemDll([string]$Name) {
    $lower = $Name.ToLowerInvariant()
    if ($lower -match '^api-ms-win-(?!crt-)[a-z0-9-]+\.dll$') { return $true }
    return ($SystemDlls -contains $lower)
}

# Starts a program and waits for that one process to exit, then returns its exit code.
# Never Start-Process -Wait: it also waits for every process the program starts, and Setup
# with /RELAUNCH starts the daemon and the window, which do not exit, so the step would hang
# until its timeout.
function Invoke-AndWait([string]$Path, [string[]]$Arguments, [int]$Seconds = 600) {
    $process = Start-Program $Path $Arguments
    if (-not $process.WaitForExit($Seconds * 1000)) {
        Stop-Process -Id $process.Id -Force -ErrorAction SilentlyContinue
        throw "$Path did not finish within $Seconds s"
    }
    return [int]$process.ExitCode
}

# The text of a log a running program may still be writing. Opened with read, write and
# delete sharing, because the writer keeps it open for appending; '' when it is missing or
# cannot be read at the moment.
function Read-SharedText([string]$Path) {
    if (-not (Test-Path $Path)) { return '' }
    try {
        $share = [System.IO.FileShare]::ReadWrite -bor [System.IO.FileShare]::Delete
        $stream = [System.IO.File]::Open($Path, [System.IO.FileMode]::Open, [System.IO.FileAccess]::Read, $share)
    } catch {
        return ''
    }
    try {
        $reader = New-Object System.IO.StreamReader($stream)
        return [string]$reader.ReadToEnd()
    } finally {
        $stream.Dispose()
    }
}

# Waits up to $Seconds for $Text to appear in the log at $Path; returns whether it did.
function Wait-ForLog([string]$Path, [string]$Text, [int]$Seconds) {
    $deadline = (Get-Date).AddSeconds($Seconds)
    while ($true) {
        if ((Read-SharedText $Path).Contains($Text)) { return $true }
        if ((Get-Date) -ge $deadline) { return $false }
        Start-Sleep -Milliseconds 500
    }
}

# Runs `playdrop status` until it exits 0 (a daemon answered) or $Seconds pass, and returns
# the last exit code. Stops early once $Process, when given, has exited.
function Wait-ForStatus([int]$Seconds, [System.Diagnostics.Process]$Process) {
    $deadline = (Get-Date).AddSeconds($Seconds)
    while ($true) {
        $code = Get-CliExit @('status')
        if ($code -eq 0) { return $code }
        if ($Process -and $Process.HasExited) { return $code }
        if ((Get-Date) -ge $deadline) { return $code }
        Start-Sleep -Milliseconds 500
    }
}

function Test-RunValue {
    $props = Get-ItemProperty -Path $RunKey -ErrorAction SilentlyContinue
    return [bool]($props -and ($props.PSObject.Properties.Name -contains 'PlayDrop'))
}

if (Test-Path $App) { throw "$App already exists; this test only runs on a machine without PlayDrop" }

Write-Host "Installing $Installer"
$setupExit = Invoke-AndWait $Installer @('/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', "/LOG=`"$Log`"")
if ($setupExit -ne 0) {
    if (Test-Path $Log) { Get-Content $Log -Tail 60 }
    throw "The installer exited with $setupExit"
}

# From here on PlayDrop is installed: whatever a check does, even throw, the finally block
# below stops the programs, prints the logs and uninstalls, and the summary at the end runs.
$finished = $false
try {
    Check 'playdropd.exe is installed' (Test-Path $Daemon)
    Check 'the window is installed as PlayDrop.exe' (Test-Path $Window)
    Check 'no window is left under the old name playdrop-desktop.exe' (-not (Test-Path $OldWindow))
    Check 'bin\playdrop.exe is installed' (Test-Path $Cli)
    Check 'bin is on the system PATH' (Test-OnSystemPath $Bin)
    Check 'the daemon and the window are not on PATH' (-not (Test-OnSystemPath $App))
    Check 'a silent install without /RELAUNCH starts no daemon' ((Get-Running 'playdropd') -eq 0)
    Check 'a silent install without /RELAUNCH opens no window' ((Get-WindowRunning) -eq 0)

    # The imports and the subsystem of each installed program. Subsystem 2 is a GUI program
    # (no console window; the daemon at sign-in, the window from the Start Menu), 3 a console
    # one.
    $dumpbin = Find-Dumpbin
    if ($dumpbin) { Write-Host "dumpbin: $dumpbin" } else { Write-Host 'dumpbin not found; using the PE reader alone' }
    $programs = @(
        @{ Path = $Daemon; Subsystem = 2 },
        @{ Path = $Window; Subsystem = 2 },
        @{ Path = $Cli; Subsystem = 3 }
    )
    foreach ($program in $programs) {
        $path = $program.Path
        $name = Split-Path $path -Leaf
        if (-not (Test-Path $path)) { continue }
        $imports = @(Get-PeImports $path)
        Write-Host "$name imports (PE reader): $($imports -join ', ')"
        # At least one: an image can name kernel32 twice (kernel32.dll from a raw-dylib
        # import and KERNEL32.dll from an import library), which is fine.
        Check "${name}: the PE reader finds KERNEL32.dll among its imports" `
            (@($imports | Where-Object { $_ -ieq 'kernel32.dll' }).Count -ge 1)
        if ($dumpbin) {
            $listed = @(Get-DumpbinImports $dumpbin $path)
            Write-Host "$name imports (dumpbin): $($listed -join ', ')"
            # -notcontains ignores case, as Windows does for DLL names.
            $missed = @($listed | Where-Object { $imports -notcontains $_ })
            Check "${name}: dumpbin and the PE reader agree (dumpbin only: $($missed -join ', '))" `
                (($listed.Count -gt 0) -and ($missed.Count -eq 0))
            $imports += $listed
        }
        $crt = @($imports | Where-Object { $_ -match $CrtDlls } | Sort-Object -Unique)
        Check "${name} imports no C runtime DLL (found: $($crt -join ', '))" ($crt.Count -eq 0)
        $foreign = @($imports | Where-Object { ($_ -notmatch $CrtDlls) -and -not (Test-SystemDll $_) } |
                Sort-Object -Unique)
        Check "${name} imports only Windows system DLLs (not on the list in this script: $($foreign -join ', '))" `
            ($foreign.Count -eq 0)
        $subsystem = (Get-PeImage $path).Subsystem
        Check "${name} has subsystem $($program.Subsystem) (got $subsystem)" ($subsystem -eq $program.Subsystem)
    }

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
    $daemonPath = [regex]::Escape($Daemon)
    Check 'two firewall rules are scoped to playdropd.exe' `
        ([regex]::Matches($rules, $daemonPath, 'IgnoreCase').Count -eq 2)
    Check 'the sign-in entry is set' (Test-RunValue)
    Check 'the Start Menu shortcut exists, directly in the programs list' (Test-Path $Shortcut)
    Check 'there is no Start Menu folder' (-not (Test-Path $OldShortcutFolder))
    foreach ($exe in $Daemon, $Window, $Cli) {
        $info = [System.Diagnostics.FileVersionInfo]::GetVersionInfo($exe)
        Check "$(Split-Path -Leaf $exe) names itself (product '$($info.ProductName)', description '$($info.FileDescription)')" `
            (($info.ProductName -eq 'PlayDrop') -and ($info.FileDescription -like 'PlayDrop*'))
        Check "$(Split-Path -Leaf $exe) carries its own icon" ((Get-IconGroupCount $exe) -ge 1)
    }
    $shownAumid = Get-ShortcutAumid $Shortcut
    Check "the Start Menu shortcut carries AppUserModelID $Aumid (got '$shownAumid')" ($shownAumid -eq $Aumid)

    # Run the installed programs. Clean logs first, so every line read below is from this run.
    if (Test-Path $LogDir) { Remove-Item -Recurse -Force $LogDir }
    New-Item -ItemType Directory -Force $Work | Out-Null
    try {
        # One daemon per Windows user: the CLI and the window find it through the per-user
        # pipe (\\.\pipe\playdrop-<SID>). The status check above (exit 3) showed none runs.
        $port = Get-FreePort
        # With the tray: showing it is part of a real start, and a tray that cannot be shown
        # (a runner may have no usable taskbar) is logged and skipped by design, never fatal.
        # So no --no-tray here; if the tray code ever ends the daemon, this test sees it.
        $daemonArgs = @(
            '--data-dir', (Quote (Join-Path $Work 'data')),
            '--receive-dir', (Quote (Join-Path $Work 'in')),
            '--port', "$port", '--name', 'ci', '--no-autostart'
        )
        $daemonProcess = Start-Program $Daemon $daemonArgs
        # Polled: a slow runner gets up to 30 s, a fast one moves on at once, and a daemon
        # that exits ends the wait early.
        $statusExit = Wait-ForStatus 30 $daemonProcess
        Check "playdrop status reaches the installed daemon within 30 s (last exit $statusExit)" ($statusExit -eq 0)
        $daemonState = if ($daemonProcess.HasExited) { "exited with $($daemonProcess.ExitCode)" } else { 'running' }
        Check "the installed daemon is still running ($daemonState)" (-not $daemonProcess.HasExited)
        if (-not $daemonProcess.HasExited) {
            # A second daemon (as from the Run key while one already runs) must exit by
            # itself, cleanly and without an error box.
            $secondArgs = @(
                '--data-dir', (Quote (Join-Path $Work 'data2')),
                '--receive-dir', (Quote (Join-Path $Work 'in2')),
                '--port', "$(Get-FreePort)", '--no-autostart', '--no-tray'
            )
            $second = Start-Program $Daemon $secondArgs
            $secondDone = $second.WaitForExit(30000)
            if (-not $secondDone) { Stop-Process -Id $second.Id -Force -ErrorAction SilentlyContinue }
            Check 'a second daemon exits by itself' $secondDone
            if ($secondDone) {
                Check "a second daemon exits with 0 (got $($second.ExitCode))" ($second.ExitCode -eq 0)
            }
            Check 'the first daemon still runs after the second one left' (-not $daemonProcess.HasExited)

            # The window: a window process that exits on its own is the bug this test is for.
            # Its log says when it reached the daemon; after that it must still be there.
            # Checked by name: if the OpenGL renderer fails, the window starts itself again
            # on the software renderer and the first process leaves (apps/desktop/src/main.rs).
            $null = Start-Program $Window @()
            $connected = Wait-ForLog $WindowLog 'connected to the daemon' 30
            Check 'the installed window logs that it connected to the daemon within 30 s' $connected
            Check 'the installed window is still running after it connected' ((Get-WindowRunning) -ge 1)
        }
    } finally {
        Stop-PlayDrop
        Show-Log $DaemonLog
        Show-Log $WindowLog
    }
    Check 'the daemon wrote its log' (Test-Path $DaemonLog)
    Check 'the window wrote its log' (Test-Path $WindowLog)
    Check 'the daemon log says the second daemon found the first' `
        ((Read-SharedText $DaemonLog).Contains('already running'))
    $fatal = Get-FatalLines $DaemonLog
    Check "the daemon logged no FATAL or PANIC line ($fatal)" ($fatal -eq '')
    $fatal = Get-FatalLines $WindowLog
    Check "the window logged no FATAL or PANIC line ($fatal)" ($fatal -eq '')

    # The failure path: a data folder that cannot be created (its parent is a file). The
    # daemon must exit non-zero, without waiting on an error box (PLAYDROP_NO_FATAL_DIALOG
    # above), and say why in its log, on one line.
    Remove-Item -Force $DaemonLog -ErrorAction SilentlyContinue
    $blocker = Join-Path $Work 'not-a-folder'
    Set-Content -Path $blocker -Value 'a file where a folder should be'
    $badArgs = @(
        '--data-dir', (Quote (Join-Path $blocker 'data')),
        '--receive-dir', (Quote (Join-Path $Work 'in3')),
        '--port', "$(Get-FreePort)", '--no-autostart', '--no-tray'
    )
    $bad = Start-Program $Daemon $badArgs
    $badDone = $bad.WaitForExit(60000)
    if (-not $badDone) { Stop-Process -Id $bad.Id -Force -ErrorAction SilentlyContinue }
    Check 'a daemon that cannot start exits by itself' $badDone
    if ($badDone) {
        Check "a daemon that cannot start exits non-zero (got $($bad.ExitCode))" ($bad.ExitCode -ne 0)
    }
    Show-Log $DaemonLog
    $fatal = Get-FatalLines $DaemonLog
    Check "its log has a FATAL line naming the data folder ($fatal)" ($fatal -match 'FATAL playdropd: .*not-a-folder')

    # A second install over the first must not add the PATH entry twice. It also carries
    # /RELAUNCH=1, as the update check does when the window was open: the daemon and the
    # window must be running after it although the install is silent, and the window, which
    # Setup starts right after the daemon, must find the daemon once its pipe is up.
    Remove-Item -Force $DaemonLog, $WindowLog -ErrorAction SilentlyContinue
    $againExit = Invoke-AndWait $Installer @('/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', '/RELAUNCH=1')
    Check "installing again with /RELAUNCH=1 succeeds (exit $againExit)" ($againExit -eq 0)
    $count = @([Environment]::GetEnvironmentVariable('Path', 'Machine') -split ';' |
            Where-Object { $_ -and ($_.TrimEnd('\') -ieq $Bin) }).Count
    Check "bin is on the system PATH once after a second install (found $count)" ($count -eq 1)
    try {
        $statusExit = Wait-ForStatus 30
        Check "after /RELAUNCH=1 the daemon answers playdrop status (last exit $statusExit)" ($statusExit -eq 0)
        $connected = Wait-ForLog $WindowLog 'connected to the daemon' 30
        Check 'after /RELAUNCH=1 the window logs that it connected to the daemon within 30 s' $connected
        Check 'after /RELAUNCH=1 the window is still running' ((Get-WindowRunning) -ge 1)
    } finally {
        Stop-PlayDrop
        Show-Log $DaemonLog
        Show-Log $WindowLog
    }

    # /RELAUNCH=daemon, for an update while no window was open: the daemon only. Setup has
    # started everything it starts by the time it exits, so no window now means none was
    # started.
    Remove-Item -Force $DaemonLog, $WindowLog -ErrorAction SilentlyContinue
    $daemonOnlyExit = Invoke-AndWait $Installer @('/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', '/RELAUNCH=daemon')
    Check "installing again with /RELAUNCH=daemon succeeds (exit $daemonOnlyExit)" ($daemonOnlyExit -eq 0)
    try {
        Check 'after /RELAUNCH=daemon no window was opened' ((Get-WindowRunning) -eq 0)
        $statusExit = Wait-ForStatus 30
        Check "after /RELAUNCH=daemon the daemon answers playdrop status (last exit $statusExit)" ($statusExit -eq 0)
    } finally {
        Stop-PlayDrop
        Show-Log $DaemonLog
    }
    $finished = $true
} catch {
    # A check that throws is a failure like any other: record it and go on to the uninstall.
    Write-Host $_.ScriptStackTrace
    Check "the checks ran to the end (stopped by: $($_.Exception.Message))" $false
} finally {
    Stop-PlayDrop
    if (-not $finished) {
        Show-Log $DaemonLog
        Show-Log $WindowLog
    }
    Remove-Item -Recurse -Force $Work -ErrorAction SilentlyContinue

    Write-Host 'Uninstalling'
    $uninstaller = Join-Path $App 'unins000.exe'
    try {
        if (Test-Path $uninstaller) {
            $removalExit = Invoke-AndWait $uninstaller @('/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART')
            Check "the uninstaller succeeds (exit $removalExit)" ($removalExit -eq 0)
        } else {
            Check "the uninstaller exists ($uninstaller)" $false
        }
    } catch {
        Check "the uninstaller ran ($($_.Exception.Message))" $false
    }

    # The uninstaller finishes from a copy of itself in the temp folder, so give it a moment.
    $deadline = (Get-Date).AddSeconds(60)
    while ((Test-Path $App) -and ((Get-Date) -lt $deadline)) { Start-Sleep -Milliseconds 500 }

    Check 'the program folder is gone' (-not (Test-Path $App))
    Check 'bin is off the system PATH' (-not (Test-OnSystemPath $Bin))
    Check 'the firewall rules are gone' ((Get-FirewallRules) -eq '')
    Check 'the sign-in entry is gone' (-not (Test-RunValue))
    Check 'the Start Menu shortcut is gone' (-not (Test-Path $Shortcut))
}

if ($script:Failures.Count -gt 0) {
    if (Test-Path $Log) { Get-Content $Log -Tail 60 }
    throw "Installer test failed: $($script:Failures -join '; ')"
}
Write-Host 'Installer test passed'
# The last program run above was netsh reporting "no rule", which is the wanted answer but
# a non-zero exit code. GitHub's pwsh shell ends a step with that code unless told otherwise.
exit 0

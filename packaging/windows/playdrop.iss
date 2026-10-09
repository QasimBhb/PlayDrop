; Inno Setup script for the PlayDrop classic Windows installer (PLAN.md: classic
; first, MSIX in Phase 7). Build with build-windows.ps1, which passes AppVersion
; and BinDir. Requires Inno Setup 6.3 or newer.
;
; What it does:
;   - installs playdropd.exe and playdrop-desktop.exe to Program Files\PlayDrop
;   - starts the daemon at login through HKCU\...\Run\PlayDrop
;   - Start Menu shortcut for the window
;   - a firewall rule scoped to playdropd.exe for TCP and UDP 42187 (SR-31),
;     never a bare port rule
;   - stops the daemon before upgrading or uninstalling, and removes the rule
;
; Keys and pins in %LOCALAPPDATA%\PlayDrop (SR-28) are left alone on uninstall.

#ifndef AppVersion
  #define AppVersion "0.1.0"
#endif
#ifndef BinDir
  #define BinDir "..\..\target\x86_64-pc-windows-msvc\release"
#endif

#define AppName "PlayDrop"
#define DaemonExe "playdropd.exe"
#define WindowExe "playdrop-desktop.exe"
#define Port "42187"
#define RuleName "PlayDrop"

[Setup]
; Never change AppId: upgrades and uninstall find the old install through it.
AppId={{6F3C2A8E-4B1D-4E57-9A3F-2D7C8B1E5A90}
AppName={#AppName}
AppVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion}
AppPublisher=PlayDrop contributors
AppPublisherURL=https://github.com/QasimBhb/PlayDrop
AppSupportURL=https://github.com/QasimBhb/PlayDrop/issues
DefaultDirName={autopf}\{#AppName}
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes
; Admin is required to add the firewall rule; per-user installs cannot.
PrivilegesRequired=admin
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
LicenseFile=..\..\LICENSE
OutputDir=dist
OutputBaseFilename=PlayDrop-{#AppVersion}-x64-setup
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
UninstallDisplayIcon={app}\{#WindowExe}
UninstallDisplayName={#AppName}
; The daemon is stopped explicitly in PrepareToInstall and [UninstallRun];
; Restart Manager would only ask the user to close it.
CloseApplications=no
; The Run value goes to the installing user's HKCU. When an admin elevates on
; behalf of a standard user, the daemon is expected to write the right user's
; value itself on first run (crates/playdropd autostart), so the compiler
; warning about user areas is silenced on purpose.
UsedUserAreasWarning=no

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "autostart"; Description: "Start PlayDrop when I sign in (needed to receive while the window is closed)"; GroupDescription: "Background service:"
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#BinDir}\{#DaemonExe}"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#BinDir}\{#WindowExe}"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"; Flags: ignoreversion
Source: "..\..\THIRD_PARTY.md"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\{#AppName}"; Filename: "{app}\{#WindowExe}"; WorkingDir: "{app}"
Name: "{group}\Uninstall {#AppName}"; Filename: "{uninstallexe}"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\{#WindowExe}"; WorkingDir: "{app}"; Tasks: desktopicon

[Registry]
; Same value name the daemon writes when the user toggles autostart in settings,
; so the two never create duplicates. Removed on uninstall either way.
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "{#AppName}"; ValueData: """{app}\{#DaemonExe}"""; Flags: uninsdeletevalue; Tasks: autostart

[Run]
; Replace any rule left by an earlier install, then add the program-scoped pair.
Filename: "{sys}\netsh.exe"; Parameters: "advfirewall firewall delete rule name=""{#RuleName}"""; Flags: runhidden waituntilterminated; StatusMsg: "Configuring Windows Firewall..."
Filename: "{sys}\netsh.exe"; Parameters: "advfirewall firewall add rule name=""{#RuleName}"" dir=in action=allow program=""{app}\{#DaemonExe}"" protocol=TCP localport={#Port}"; Flags: runhidden waituntilterminated; StatusMsg: "Configuring Windows Firewall..."
Filename: "{sys}\netsh.exe"; Parameters: "advfirewall firewall add rule name=""{#RuleName}"" dir=in action=allow program=""{app}\{#DaemonExe}"" protocol=UDP localport={#Port}"; Flags: runhidden waituntilterminated; StatusMsg: "Configuring Windows Firewall..."
; Start the daemon as the signed-in user (not the elevated installer), then
; offer to open the window.
Filename: "{app}\{#DaemonExe}"; Flags: nowait runasoriginaluser skipifsilent; Tasks: autostart
Filename: "{app}\{#WindowExe}"; Description: "{cm:LaunchProgram,{#AppName}}"; Flags: nowait postinstall runasoriginaluser skipifsilent

[UninstallRun]
; Stop the window and the daemon so their files can be removed.
Filename: "{sys}\taskkill.exe"; Parameters: "/F /T /IM {#WindowExe}"; Flags: runhidden waituntilterminated; RunOnceId: "StopWindow"
Filename: "{sys}\taskkill.exe"; Parameters: "/F /T /IM {#DaemonExe}"; Flags: runhidden waituntilterminated; RunOnceId: "StopDaemon"
Filename: "{sys}\netsh.exe"; Parameters: "advfirewall firewall delete rule name=""{#RuleName}"""; Flags: runhidden waituntilterminated; RunOnceId: "DeleteFirewallRule"

[Code]
{ Stop a running daemon and window before files are replaced on upgrade.
  taskkill exits non-zero when nothing is running; that is fine. Transfers in
  flight resume after the new daemon starts, so a hard stop loses nothing. }
procedure StopProcess(const ExeName: String);
var
  ResultCode: Integer;
begin
  Exec(ExpandConstant('{sys}\taskkill.exe'), '/F /T /IM ' + ExeName, '',
       SW_HIDE, ewWaitUntilTerminated, ResultCode);
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  StopProcess('{#WindowExe}');
  StopProcess('{#DaemonExe}');
  Result := '';
end;

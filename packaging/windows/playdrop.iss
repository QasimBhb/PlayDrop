; Inno Setup script for the PlayDrop classic Windows installer (PLAN.md: classic
; first, MSIX in Phase 7). Build with build-windows.ps1, which passes AppVersion
; and BinDir. Requires Inno Setup 6.3 or newer.
;
; What it does:
;   - installs playdropd.exe and playdrop-desktop.exe to Program Files\PlayDrop
;   - installs the command line, playdrop.exe, to Program Files\PlayDrop\bin and
;     (a checked task) adds that folder to the system PATH; uninstall takes the
;     entry out again. The CLI has its own folder so PATH gains one command and
;     not the daemon and the window too
;   - starts the daemon at login through HKCU\...\Run\PlayDrop
;   - Start Menu shortcut for the window, carrying the AppUserModelID
;     org.playdrop.PlayDrop that the daemon's toasts name (Windows identifies a
;     classic app's toasts through a Start Menu shortcut with that ID)
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
#define CliExe "playdrop.exe"
#define Port "42187"
#define RuleName "PlayDrop"
#define AppUserModelID "org.playdrop.PlayDrop"

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
; The PATH task edits the system environment; this makes Setup tell running
; programs (Explorer, so new terminals) that it changed.
ChangesEnvironment=yes

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "autostart"; Description: "Start PlayDrop when I sign in (needed to receive while the window is closed)"; GroupDescription: "Background service:"
Name: "addtopath"; Description: "Add the playdrop command to PATH (for terminals and scripts)"; GroupDescription: "Command line:"
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#BinDir}\{#DaemonExe}"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#BinDir}\{#WindowExe}"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#BinDir}\{#CliExe}"; DestDir: "{app}\bin"; Flags: ignoreversion
Source: "..\..\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"; Flags: ignoreversion
Source: "..\..\THIRD_PARTY.md"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
; The AppUserModelID must match AUMID in crates/playdropd/src/notify.rs: toasts are
; shown under it, and Windows looks it up on a Start Menu shortcut. No COM activator or
; CLSID is registered: toast buttons reach the running daemon in-process.
Name: "{group}\{#AppName}"; Filename: "{app}\{#WindowExe}"; WorkingDir: "{app}"; AppUserModelID: "{#AppUserModelID}"
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

{ The system PATH. Setup runs as admin, so the machine-wide value is the one
  every user's terminal sees. It is edited here and not in [Registry] so that
  an upgrade never adds the folder twice and uninstall removes only our entry. }
const
  EnvKey = 'SYSTEM\CurrentControlSet\Control\Session Manager\Environment';

{ Reads the raw, unexpanded PATH. False when the value exists but cannot be
  read as a string: then it is left alone, never overwritten. }
function ReadSystemPath(var Paths: String): Boolean;
begin
  Paths := '';
  Result := RegQueryStringValue(HKLM, EnvKey, 'Path', Paths)
    or not RegValueExists(HKLM, EnvKey, 'Path');
end;

{ 1-based index of Dir as a whole entry of Paths, or 0. Case is ignored. }
function PathEntryPos(const Paths, Dir: String): Integer;
begin
  Result := Pos(';' + Uppercase(Dir) + ';', ';' + Uppercase(Paths) + ';');
end;

procedure AddToSystemPath(const Dir: String);
var
  Paths: String;
begin
  if not ReadSystemPath(Paths) then
  begin
    Log('PATH not readable as a string; left unchanged');
    exit;
  end;
  if PathEntryPos(Paths, Dir) > 0 then
    exit;
  if (Paths <> '') and (Paths[Length(Paths)] <> ';') then
    Paths := Paths + ';';
  if not RegWriteExpandStringValue(HKLM, EnvKey, 'Path', Paths + Dir) then
    Log('could not write PATH');
end;

procedure RemoveFromSystemPath(const Dir: String);
var
  Paths: String;
  P: Integer;
  Changed: Boolean;
begin
  if not ReadSystemPath(Paths) then
    exit;
  Changed := False;
  P := PathEntryPos(Paths, Dir);
  while P > 0 do
  begin
    { Take the entry and the separator after it; for the last entry, the
      separator before it. }
    if P + Length(Dir) <= Length(Paths) then
      Delete(Paths, P, Length(Dir) + 1)
    else if P > 1 then
      Delete(Paths, P - 1, Length(Dir) + 1)
    else
      Delete(Paths, P, Length(Dir));
    Changed := True;
    P := PathEntryPos(Paths, Dir);
  end;
  if Changed then
    if not RegWriteExpandStringValue(HKLM, EnvKey, 'Path', Paths) then
      Log('could not write PATH');
end;

{ Installing with the task ticked adds the folder; installing again with it
  cleared takes it back out, so the checkbox always matches the machine. }
procedure CurStepChanged(CurStep: TSetupStep);
begin
  if CurStep = ssPostInstall then
  begin
    if WizardIsTaskSelected('addtopath') then
      AddToSystemPath(ExpandConstant('{app}\bin'))
    else
      RemoveFromSystemPath(ExpandConstant('{app}\bin'));
  end;
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usUninstall then
    RemoveFromSystemPath(ExpandConstant('{app}\bin'));
end;

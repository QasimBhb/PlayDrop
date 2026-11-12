# Windows packaging

A classic Inno Setup installer (PLAN.md decision: classic first, MSIX only in Phase 7).

**Status: built and installed silently in CI, not yet run on a real machine.**
`.github/workflows/windows.yml` builds, tests and lints the workspace on GitHub's
`windows-latest` runner, compiles this installer (uploaded as the `windows-installer`
artifact) and runs `test-installer.ps1` against it; `release.yml` reuses the job for
releases. Nobody has clicked through it on a Windows 11 machine yet; the checklist below
lists what to confirm on the first real run.

## Installer test

`test-installer.ps1` (elevated PowerShell, on a machine without PlayDrop: its last step
uninstalls) installs silently and checks that the three programs are in place, `bin` is on the
system `PATH` once, `playdrop --version` prints the release version, `playdrop status` exits 3
with no daemon, both firewall rules name `playdropd.exe`, the sign-in entry exists, and the
Start Menu shortcut carries the AppUserModelID `org.playdrop.PlayDrop`. It installs a second
time to prove `PATH` does not grow, then uninstalls and checks that the folder, the `PATH`
entry, the rules, the sign-in entry and the shortcut are gone. CI runs it on every
installer build, so the items it covers are no longer in the checklist below as unknowns,
only as things to see once with a person at the screen.

Between install and uninstall it also runs what was installed, because a release once
installed cleanly and then did not open on a real PC:

- No installed `.exe` imports a C runtime DLL (`VCRUNTIME*`, `MSVCP*`, `MSVCR*`, `VCOMP*`,
  `CONCRT*`, `api-ms-win-crt-*`, `ucrtbase.dll`), which a fresh Windows does not have, nor any
  DLL that is not on the script's list of Windows system DLLs (`$SystemDlls`; the failure
  names the DLL, and the comment above the list says when one may be added). The root
  `.cargo/config.toml` links the runtime in (`+crt-static`), and `build-windows.ps1` refuses
  to build with `RUSTFLAGS` or `CARGO_ENCODED_RUSTFLAGS` set, since either replaces those
  flags. The imports are read by a small PE reader in the script and, where the runner has it,
  by `dumpbin /DEPENDENTS` (found through `vswhere`); both must agree.
- `playdropd.exe` and `playdrop-desktop.exe` are GUI programs (PE subsystem 2), `playdrop.exe`
  a console one (3).
- The installed daemon starts with a scratch data dir and port, `playdrop status` reaches it
  within 30 s and it stays up, a second daemon exits by itself with code 0, and the window
  logs that it connected to the daemon within 30 s and is still running then. Neither logs a
  `FATAL` or `PANIC` line; both logs are printed into the CI output.
- A daemon whose data dir cannot be created exits non-zero within a minute and its log names
  the failure on one `FATAL` line.
- A second silent install with `/RELAUNCH=1` leaves the daemon answering and the window
  connected to it; a third with `/RELAUNCH=daemon` starts the daemon and no window.
- Whatever fails or throws on the way, the programs are stopped, the logs printed and
  PlayDrop uninstalled.

## Logs, error boxes and `/RELAUNCH=1`

The daemon and the window are GUI programs on Windows, so no console window sits behind
them. Started from a terminal they still print there: they attach to that terminal's
console, so closing the terminal, or pressing Ctrl+C in it later, can end them too. Start
them from the Start Menu (or `start playdropd`) to keep them independent of a terminal. They always log to
`%LOCALAPPDATA%\PlayDrop\logs\playdropd.log` and `playdrop-desktop.log` (moved to
`<name>.log.old` at about 1 MiB; the folder is readable by the user only). When either stops
because of an error or a crash, it writes a `FATAL` line there and shows a message box that
says what failed and names the log file to send with a bug report.

- `PLAYDROP_NO_FATAL_DIALOG=1` turns the box off: the error is logged and the program exits
  non-zero. Set it for scripted runs and CI, where nobody could press OK.
- `/RELAUNCH=1` on the installer's command line starts the daemon and then the window again
  as the signed-in user after the install, even with `/SILENT` or `/VERYSILENT`;
  `/RELAUNCH=daemon` starts the daemon only. The daemon's update check runs the downloaded
  installer with `/SILENT` and one of them: `1` when a window was open, `daemon` when not.
  Any other value, or none, starts nothing after a silent install, as before. If such an
  install fails or is cancelled after it stopped the old daemon, Setup starts the same
  programs again from the files still installed, so an update that went wrong never leaves
  PlayDrop stopped. The window, started a moment after the daemon, keeps trying the daemon's
  pipe once a second until it answers. Upgrades stop the running daemon without
  `taskkill /T`, since the installer can itself be a child of the daemon.
- A wrong flag on the daemon's command line is reported like any start-up error (log line and
  message box) and exits 2; `--help` and `--version` print to the terminal and exit 0.
- If the window's OpenGL renderer fails after Slint chose it, the window starts itself again
  with `SLINT_BACKEND=winit-software`. Setting that variable yourself skips the retry.

## Build

On Windows with the Rust MSVC toolchain and Inno Setup 6.3 or newer
(`winget install JRSoftware.InnoSetup`):

```powershell
packaging\windows\build-windows.ps1           # cargo build --release --target x86_64-pc-windows-msvc, then iscc
packaging\windows\build-windows.ps1 -NoBuild  # package binaries that are already built
```

Output: `packaging\windows\dist\PlayDrop-<version>-x64-setup.exe`. The version comes from
`[workspace.package]` in the root `Cargo.toml`.

## What the installer does

- Installs `playdropd.exe` and `playdrop-desktop.exe` to `C:\Program Files\PlayDrop`.
- Installs the command line, `playdrop.exe`, to `C:\Program Files\PlayDrop\bin` and adds that
  folder to the system `PATH` (a checked task). It has its own folder so `PATH` gains one
  command, not the daemon and the window. Installing again with the box cleared removes the
  entry, an upgrade never adds it twice, and uninstall removes it.
- Adds `HKCU\Software\Microsoft\Windows\CurrentVersion\Run\PlayDrop` so the daemon starts at
  sign-in (a checked task; the value is removed on uninstall). The daemon uses the same
  value name when the user changes autostart in settings.
- Start Menu shortcut for the window, optional desktop shortcut. The Start Menu shortcut
  carries the AppUserModelID `org.playdrop.PlayDrop`, the ID the daemon shows its toasts
  under: Windows finds a classic app's toast identity through such a shortcut. Nothing else
  is registered for toasts (no COM activator, no CLSID): the experimental toast buttons
  (`PLAYDROP_TOAST_BUTTONS=1`) reach the running daemon in-process.
- Adds two inbound firewall rules named `PlayDrop`, both limited to `playdropd.exe`: TCP
  42187 for transfers and UDP 42187 for discovery. They are program-scoped, never a bare
  port rule (SR-31), so another program cannot use the opening.
- Starts the daemon as the signed-in user and offers to open the window (with `/RELAUNCH=1`
  starts both even when silent, with `/RELAUNCH=daemon` the daemon only; see above).
- On upgrade and uninstall it stops the window and daemon first. Uninstall also deletes the
  firewall rules. Keys and pins in `%LOCALAPPDATA%\PlayDrop` (SR-28) are kept; delete that
  folder to forget paired devices.

Installing needs administrator rights because of the firewall rule.

## Checklist for the first real run

- `iscc` compiles the script without errors.
- After install, `netsh advfirewall firewall show rule name=PlayDrop verbose` lists two
  rules, both with `Program: C:\Program Files\PlayDrop\playdropd.exe`, ports 42187 TCP and
  UDP, profile Any.
- The daemon is running after install and after sign-out and sign-in.
- A phone or second PC can reach the machine with the window minimised (Phase 2 "done
  when": Windows 11 receives while minimised).
- Upgrading over a running daemon replaces the files without a "file in use" error.
- Uninstall removes both firewall rules, the Run value and the program folder.
- An admin installing on behalf of a standard user: check whose HKCU got the Run value, and
  that the daemon fixes it on first run.
- Code signing is not set up; SmartScreen will warn until it is.
- Launch the window from the Start Menu with the daemon stopped: it opens on "not running",
  and "Start" either connects or says why (exit code and the log path).
- Rename `%LOCALAPPDATA%\PlayDrop\data` to a file of that name and start the daemon: an error
  box names the problem and the log file. Undo the rename afterwards.

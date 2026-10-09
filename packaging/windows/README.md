# Windows packaging

A classic Inno Setup installer (PLAN.md decision: classic first, MSIX only in Phase 7).

**Status: untested.** The script and build script were written on Linux and have not been
compiled or run. Treat the first run on a Windows 11 machine as the real test; the checklist
below lists what to confirm.

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
- Adds `HKCU\Software\Microsoft\Windows\CurrentVersion\Run\PlayDrop` so the daemon starts at
  sign-in (a checked task; the value is removed on uninstall). The daemon uses the same
  value name when the user changes autostart in settings.
- Start Menu shortcut for the window, optional desktop shortcut.
- Adds two inbound firewall rules named `PlayDrop`, both limited to `playdropd.exe`: TCP
  42187 for transfers and UDP 42187 for discovery. They are program-scoped, never a bare
  port rule (SR-31), so another program cannot use the opening.
- Starts the daemon as the signed-in user and offers to open the window.
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

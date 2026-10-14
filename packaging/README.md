# Packaging

Phase 2 task e. One folder per target, plus `linux/` with the desktop files that the deb and
the Flatpak share. Nothing here touches the crates; every script builds the two binaries
`playdropd` (daemon) and `playdrop-desktop` (window) from the workspace and lays them out.

**`linux/`** holds the launcher (`org.playdrop.PlayDrop.desktop`), the XDG autostart entry
for the daemon (`org.playdrop.Daemon.desktop`), the D-Bus activation file
(`org.playdrop.PlayDrop.Daemon1.service`), an optional systemd user unit (`playdropd.service`), the
AppStream metainfo, and the app and symbolic icons. The daemon writes per-user copies of the
autostart and D-Bus files when it runs unsandboxed; they share these file names so a user
copy overrides the system one.

**`deb/`** builds a native `.deb` for Pop!_OS 24.04 COSMIC and Ubuntu 24.04 GNOME with
`build-deb.sh` (plain `dpkg-deb`, no `Cargo.toml` changes). It installs the daemon so it
starts at login and on D-Bus activation, never touches the firewall, and documents the ufw
command. SRs: SR-26 (the bus name and activation file the daemon's caller checks are built
around), SR-28 (data stays in `~/.local/share/playdrop`, untouched by install and removal).
Enables the "done when" checks *Pop!_OS 24.04 COSMIC receives with the window closed* and
*Ubuntu GNOME works through the window and a notification*.

**`flatpak/`** is a manifest for one app, `org.playdrop.PlayDrop`, containing both binaries,
with a fixed permission list: no session bus (anti-pattern 16), no home access, the receive
folder `~/Downloads/PlayDrop` only, file portals for picking. SRs: SR-26 (both processes run
under the app ID the daemon's `.flatpak-info` check expects), SR-28 (data under
`~/.var/app/org.playdrop.PlayDrop/data/playdrop`). Enables the same COSMIC and GNOME checks
for Flatpak installs once autostart goes through the Background portal (see its README).

**`windows/`** is an Inno Setup classic installer with `build-windows.ps1`. It installs both
binaries, the `HKCU\...\Run\PlayDrop` autostart value, Start Menu shortcuts, and a firewall
rule scoped to `playdropd.exe` for TCP and UDP 42187. SRs: SR-31 (program-scoped rule, never
a bare port), SR-28 (`%LOCALAPPDATA%\PlayDrop` kept on uninstall). Enables *Windows 11
receives while minimised*. Untested until it runs on a Windows machine.

None of the targets add a GNOME Shell extension or a COSMIC applet (anti-pattern 12); the
tray goes through StatusNotifierItem. The check *a closed ufw/firewalld/Windows port shows
"port X blocked"* belongs to the daemon's diagnostics; the deb README covers the manual ufw
step until then.

Build output (`deb/dist/`, `windows/dist/`, `flatpak/build-dir/`, `flatpak/repo/`,
`flatpak/*.flatpak`) is gitignored in `packaging/.gitignore`.

Releases are built by `.github/workflows/release.yml` from a `v<version>` tag and published
on GitHub Releases, the only distribution channel; `.github/workflows/windows.yml` builds
and tests the Windows side on every change. How releases are made, every asset, the Android
release key and the iOS sideloading story: `docs/release/distribution.md`.

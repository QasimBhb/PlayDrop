# Debian packaging

One native `.deb` for Pop!_OS 24.04 (COSMIC) and Ubuntu 24.04 (GNOME): the daemon, the window,
the command line and the COSMIC panel applet, with the shared desktop files from
`packaging/linux/`. One download and one install gives everything.

## Build

Needs a Rust toolchain, `dpkg-deb` (part of `dpkg`, always present on Debian and Ubuntu),
`readelf` (`binutils`) and the build packages of both cargo workspaces: `pkg-config
libfontconfig1-dev libxkbcommon-dev libgl1-mesa-dev libwayland-dev`.

```sh
packaging/deb/build-deb.sh            # cargo build --release of both workspaces, then package
packaging/deb/build-deb.sh --no-build # package the release binaries already built
packaging/deb/build-transitional-deb.sh  # the empty playdrop-cosmic-applet package
```

The applet (`apps/cosmic-applet`) is its own cargo workspace with its own `Cargo.lock`, so it
builds into its own target folder: `PLAYDROP_APPLET_TARGET_DIR`, or `apps/cosmic-applet/target`
when unset. The main workspace uses `CARGO_TARGET_DIR`, or `target/`. On this repo's NTFS drive:

```sh
CARGO_TARGET_DIR=$HOME/.cache/playdrop-target \
PLAYDROP_APPLET_TARGET_DIR=$HOME/.cache/playdrop-applet-target \
CARGO_INCREMENTAL=0 packaging/deb/build-deb.sh
```

The script refuses to package when the applet's version differs from the workspace's, and when
any program links a shared library whose package is not in `Depends` (the list of allowed
libraries is in the script; add the package to `debian/control.in` and the library to the list
together). `--package-version <v>` writes another version into the package, for trying an
upgrade locally (the programs still report the workspace version); never for a release.

The results are `packaging/deb/dist/playdrop_<version>_<arch>.deb` and
`packaging/deb/dist/playdrop-cosmic-applet_<version>_all.deb`. If `lintian` is installed the
scripts run it and print its findings.

This is a plain `dpkg-deb --root-owner-group` build, not `cargo-deb`: `cargo-deb` only
reads its settings from `[package.metadata.deb]` in `Cargo.toml`, and we keep packaging
out of the crate manifests.

## Install

```sh
sudo apt install ./packaging/deb/dist/playdrop_<version>_amd64.deb
```

Use `apt` rather than `dpkg -i` so the dependencies are pulled in. Log out and back in (or
run `playdropd` once) to start the background service.

## Update by hand

```sh
sudo apt install ./playdrop-<version>-linux-amd64.deb
playdrop restart
```

`apt` replaces the files but the running daemon stays the old program until it restarts.
`playdrop restart` (as the user, not with `sudo`) asks it to shut down over IPC, waits until it
is gone, starts the installed one again the way it was running (the systemd user unit when it
was the unit's, otherwise D-Bus activation, otherwise `playdropd` itself) and prints the old and
the new version. `postinst` prints a one-line hint when the user who ran `sudo` (or `pkexec`)
still has a `playdropd` or applet running from before the install; it only reads `/proc` and
never stops or starts anything. The panel loads a new applet at the next login.

PlayDrop's own **Update** button (from 0.11.0) runs `pkexec apt-get install --no-remove` and
restarts the daemon by itself (`docs/design/update-check.md` section 6).

## What goes where

| File | Installed to | Purpose |
|---|---|---|
| `playdropd`, `playdrop-desktop`, `playdrop` | `/usr/bin/` | Daemon, window and command line (`docs/guide/cli.md`) |
| `playdrop-cosmic-applet` | `/usr/bin/` | COSMIC panel applet, run only by `cosmic-panel` |
| `org.playdrop.PlayDrop.CosmicApplet.desktop` | `/usr/share/applications/` | Lists the applet in COSMIC Settings (`X-CosmicApplet=true`, `NoDisplay=true`) |
| `org.playdrop.PlayDrop.desktop` | `/usr/share/applications/` | App launcher |
| `org.playdrop.Daemon.desktop` | `/etc/xdg/autostart/` | Starts the daemon at login (default) |
| `org.playdrop.PlayDrop.Daemon1.service` | `/usr/share/dbus-1/services/` | Starts the daemon when the window calls it |
| `playdropd.service` | `/usr/lib/systemd/user/` | Optional systemd user unit, not enabled |
| `org.playdrop.PlayDrop.metainfo.xml` | `/usr/share/metainfo/` | Software centre listing |
| icons | `/usr/share/icons/hicolor/` | App and symbolic (tray) icons |
| `debian/ufw-playdrop` | `/etc/ufw/applications.d/playdrop` | ufw profile, inert until used |

The daemon also writes per-user copies of the autostart and D-Bus files when it runs
unsandboxed. They use the same file names, so a per-user copy (for example one that turns
autostart off) overrides the system one instead of adding a second starter.

`postinst` refreshes the desktop and icon caches and may print the restart hint above. The
package never changes firewall rules and never starts or stops services on its own.

## The COSMIC applet

From 0.12.0 the applet is inside `playdrop` (the owner's decision of 2026-11-12: one download,
one install). It stays harmless elsewhere: nothing autostarts it, its desktop file is hidden
from launchers, and only COSMIC's panel runs it, once the user adds it under Settings, Desktop,
Panel, Applets. Its libraries (`libc`, `libgcc_s`, `libxkbcommon`, checked with
`dpkg-shlibdeps` on the built binary) are already in the package's `Depends`, so merging added no
dependency; the file chooser uses `xdg-desktop-portal`, already in `Recommends`.

Up to 0.11.0 the applet was its own package, `playdrop-cosmic-applet`, depending on
`playdrop (= <version>)`. The move works like this:

- `playdrop` has `Replaces: playdrop-cosmic-applet (<< 0.12.0)`, so dpkg lets it take over
  `/usr/bin/playdrop-cosmic-applet` and the applet's desktop file from the old package. There is
  no `Breaks` or `Conflicts`: those would make apt remove a package, which 0.11.0's updater
  forbids (`--no-remove`).
- `build-transitional-deb.sh` builds an empty `playdrop-cosmic-applet` (`Architecture: all`,
  section `oldlibs`, only its copyright file) that depends on `playdrop (>= 0.12.0)`. The release
  publishes it as `playdrop-cosmic-applet-<version>-linux-amd64.deb`, the name 0.11.0's updater
  looks for: with the applet installed it installs both files in one `apt-get` call, and both
  packages are then upgraded, nothing removed. With no upper bound on `playdrop`, the
  transitional package never holds a later update back, so it can be removed any time
  (`sudo apt remove playdrop-cosmic-applet`) and releases can stop carrying it.
- The release keeps the transitional file until 2026-11-27 (0.12.0 to 0.15.0 came out within
  two days) and drops it from the first release after that (`docs/release/distribution.md`). A 0.11.0 install that skipped them
  then gets a notice instead of the Update button, and updates by hand.
- Proven on 2026-11-12 against the real 0.11.0 release files: `apt-get install --simulate
  --no-remove` of the new pair over an installed 0.11.0 pair upgrades both and removes nothing;
  dpkg in a scratch root moves both applet files to `playdrop` in either unpack order (without
  the `Replaces` line it stops with "trying to overwrite"); a fresh install of `playdrop` alone
  and a later `playdrop` alone over `playdrop` plus the transitional package both pass with
  `--no-remove`.

## Autostart: pick one

By default the XDG autostart entry starts `playdropd` at login. To use systemd instead (it
restarts the daemon if it crashes):

```sh
systemctl --user enable --now playdropd.service
mkdir -p ~/.config/autostart
printf '[Desktop Entry]\nHidden=true\n' > ~/.config/autostart/org.playdrop.Daemon.desktop
```

Running both is harmless (the second copy exits because the first already owns the D-Bus
name) but leaves a failed start in the journal. D-Bus activation goes through the unit
when systemd is present, even if the unit is not enabled.

## Firewall

Pop!_OS and Ubuntu ship ufw disabled. If you turned it on, PlayDrop cannot be reached until
port 42187 is open for both TCP (transfers) and UDP (discovery):

```sh
sudo ufw allow PlayDrop          # uses the installed profile, or:
sudo ufw allow 42187/tcp && sudo ufw allow 42187/udp
```

Do this until the diagnostics screen tells you the port is blocked; that screen will name
the exact problem. firewalld users: `sudo firewall-cmd --permanent --add-port=42187/tcp
--add-port=42187/udp && sudo firewall-cmd --reload`.

## Uninstall

```sh
sudo apt remove playdrop
```

Keys, pins and settings in `~/.local/share/playdrop/` are left in place (SR-28 keeps them
private to your user); delete that folder to forget all paired devices.

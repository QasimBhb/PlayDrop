# Debian packaging

A native `.deb` for Pop!_OS 24.04 (COSMIC) and Ubuntu 24.04 (GNOME). It installs both
binaries and the shared desktop files from `packaging/linux/`.

## Build

Needs a Rust toolchain and `dpkg-deb` (part of `dpkg`, always present on Debian and Ubuntu).

```sh
packaging/deb/build-deb.sh            # cargo build --release, then package
packaging/deb/build-deb.sh --no-build # package existing target/release binaries
```

The result is `packaging/deb/dist/playdrop_<version>_<arch>.deb`. If `lintian` is installed the script runs it and prints its findings.

This is a plain `dpkg-deb --root-owner-group` build, not `cargo-deb`: `cargo-deb` only
reads its settings from `[package.metadata.deb]` in `Cargo.toml`, and we keep packaging
out of the crate manifests.

## Install

```sh
sudo apt install ./packaging/deb/dist/playdrop_0.1.0_amd64.deb
```

Use `apt` rather than `dpkg -i` so the dependencies are pulled in. Log out and back in (or
run `playdropd` once) to start the background service.

## What goes where

| File | Installed to | Purpose |
|---|---|---|
| `playdropd`, `playdrop-desktop` | `/usr/bin/` | Daemon and window |
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

`postinst` only refreshes the desktop and icon caches. The package never changes firewall
rules and never starts services on its own.

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

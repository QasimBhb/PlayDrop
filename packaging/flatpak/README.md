# Flatpak packaging

`org.playdrop.PlayDrop.yml` builds one Flatpak app that contains both the daemon
(`playdropd`) and the window (`playdrop-desktop`). Both run under the app ID
`org.playdrop.PlayDrop`, which is what the daemon's SR-26 caller check expects to find in
`/.flatpak-info`.

## One-time setup

```sh
flatpak install --user flathub org.flatpak.Builder \
    org.freedesktop.Platform//25.08 org.freedesktop.Sdk//25.08 \
    org.freedesktop.Sdk.Extension.rust-stable//25.08
```

(A distro `flatpak-builder` package works too; replace `flatpak run org.flatpak.Builder`
with `flatpak-builder` below.)

## Generate the vendored crate list

The build runs cargo offline, so every crate in `Cargo.lock` must be listed as a download
in `cargo-sources.json`. Regenerate it whenever `Cargo.lock` changes, from the repo root:

```sh
git clone --depth 1 https://github.com/flatpak/flatpak-builder-tools ../flatpak-builder-tools
python3 -m pip install --user aiohttp tomlkit
python3 ../flatpak-builder-tools/cargo/flatpak-cargo-generator.py \
    Cargo.lock -o packaging/flatpak/cargo-sources.json
```

The generated file also writes a cargo config into `CARGO_HOME` that redirects crates.io
to the vendored copies, which is why the manifest sets `CARGO_HOME`.

## Build and install

From `packaging/flatpak/` (the manifest's source path is `../..`, the repo root):

```sh
cd packaging/flatpak
flatpak run org.flatpak.Builder --user --install --force-clean build-dir org.playdrop.PlayDrop.yml
flatpak run org.playdrop.PlayDrop                              # the window
flatpak run --command=playdropd org.playdrop.PlayDrop          # the daemon
```

A single-file bundle for testing on another machine:

```sh
flatpak run org.flatpak.Builder --repo=repo --force-clean build-dir org.playdrop.PlayDrop.yml
flatpak build-bundle repo playdrop.flatpak org.playdrop.PlayDrop
```

Lint before a Flathub submission:
`flatpak run --command=flatpak-builder-lint org.flatpak.Builder manifest org.playdrop.PlayDrop.yml`.

## Permissions

The `finish-args` list is fixed. Each entry has a comment in the manifest.

| Permission | Why |
|---|---|
| `--share=network` | LAN discovery and transfers on port 42187 |
| `--share=ipc`, `--socket=wayland`, `--socket=fallback-x11`, `--device=dri` | The Slint window, and Wayland clipboard sync |
| `--talk-name=org.kde.StatusNotifierWatcher` | Tray icon |
| `--talk-name=org.freedesktop.Notifications` | Notifications |
| `--system-talk-name=org.freedesktop.login1` | Suspend and resume signals (system bus) |
| `--talk-name=org.freedesktop.FileManager1` | "Show in folder" |
| `--filesystem=xdg-download/PlayDrop:create` | The receive folder, `~/Downloads/PlayDrop` |

No bus permission is needed for the daemon's IPC name `org.playdrop.PlayDrop.Daemon1`:
Flatpak's default session bus policy lets an app own its own namespace (`$FLATPAK_ID` and
every name under it), and owning a name includes talking to it, so the window reaches the
daemon without `--own-name` or `--talk-name`.

Never added: `--socket=session-bus` (PLAN.md anti-pattern 16) and `--filesystem=home`.
Files to send are chosen through the file chooser portal, which grants access to just the
picked files.

Inside the sandbox `XDG_DATA_HOME` is `~/.var/app/org.playdrop.PlayDrop/data`, so keys,
pins and settings live in `~/.var/app/org.playdrop.PlayDrop/data/playdrop/` (SR-28 modes
still apply).

## Autostart and D-Bus activation

A Flatpak cannot install into `/etc/xdg/autostart` or the host's D-Bus service directory,
so the deb's approach does not carry over:

- **Autostart.** The standard Flatpak way is the Background portal
  (`org.freedesktop.portal.Background.RequestBackground` with `autostart: true` and
  `commandline: ["playdropd"]`). The portal asks the user once and writes
  `~/.config/autostart/org.playdrop.PlayDrop.desktop` with
  `Exec=flatpak run --command=playdropd org.playdrop.PlayDrop`. No permission is needed
  for it. The daemon should call it when it detects it is sandboxed, instead of writing
  autostart files itself.
- **D-Bus activation.** When a Flatpak is installed, Flatpak exports
  `/app/share/dbus-1/services/*.service` to the host with `Exec` rewritten to
  `flatpak run --command=playdropd org.playdrop.PlayDrop`. It only exports files whose bus
  name equals the app ID or starts with `<app-id>.`, and whose file name matches `Name=`.
  The daemon's name `org.playdrop.PlayDrop.Daemon1` qualifies, so a window call starts the
  daemon when it is not running, the same as on the deb.
- **Caller checks.** Inside Flatpak the daemon and window run as separate sandbox
  instances that cannot see each other's PIDs in `/proc`, so both skip the `/proc` checks
  of SR-26 there and keep the UID check.
- **`DBusActivatable=true`** in the launcher is the other standard pattern: the desktop
  file ID becomes a bus name that the shell activates through `org.freedesktop.Application`.
  It requires the window to implement that interface, which it does not, so the launcher
  uses a plain `Exec=`.

## What has been verified

- The manifest parses as YAML, and the desktop and metainfo files pass
  `desktop-file-validate` and `appstreamcli validate`.
- Not yet built: `flatpak-builder` was not available on the machine where this was
  written, and a build needs network access to fetch the runtime and crates.

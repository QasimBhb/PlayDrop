# Flatpak packaging

`org.playdrop.PlayDrop.yml` builds one Flatpak app that contains both the daemon
(`playdropd`), the window (`playdrop-desktop`) and the command line (`playdrop`). All run under the app ID
`org.playdrop.PlayDrop`, which is what the daemon's SR-26 caller check expects to find in
`/.flatpak-info`.

## One-time setup

```sh
flatpak remote-add --user --if-not-exists flathub https://dl.flathub.org/repo/flathub.flatpakrepo
flatpak install --user -y flathub org.flatpak.Builder \
    org.freedesktop.Platform//25.08 org.freedesktop.Sdk//25.08 \
    org.freedesktop.Sdk.Extension.rust-stable//25.08
```

(A distro `flatpak-builder` package works too; replace `flatpak run org.flatpak.Builder`
with `flatpak-builder` below.)

## The vendored crate list (`cargo-sources.json`)

The build runs cargo offline, so every crate in `Cargo.lock` is listed as a download in
`cargo-sources.json`. The file is committed, as Flathub expects, and must be regenerated
whenever `Cargo.lock` changes:

```sh
packaging/flatpak/update-sources.sh
```

The script clones flatpak-builder-tools into `~/.cache/playdrop/` (never into the repo) and
runs its `cargo/flatpak-cargo-generator.py`. It needs `aiohttp` and `tomlkit`: with
[uv](https://docs.astral.sh/uv/) installed it fetches them into uv's cache and installs
nothing system-wide; without uv, `python3` must already have them; without either, the
script runs the generator inside the Freedesktop SDK from the one-time setup above, whose
Python has pip (Pop!_OS ships `python3` with neither pip nor venv). The generated file also
writes a cargo config into `CARGO_HOME` that redirects crates.io to the vendored copies,
which is why the manifest sets `CARGO_HOME`.

`packaging/flatpak/check-sources.py` (standard library only) fails when the list and
`Cargo.lock` disagree. The Linux workflow runs it on every push that touches `Cargo.lock`
and the release workflow runs it before the Flatpak build, because the v0.3.0 release run
on 2026-10-17 failed on one crate that had been added since the list was last generated.

## Build, install and run

Run from `packaging/flatpak/` (the manifest's source path is `../..`, the repo root). Keep
the build and state directories on a Linux filesystem (ext4, btrfs) outside the repo:
flatpak-builder needs hard links, ownership and extended attributes, which an NTFS or
exFAT drive does not give it, and on this machine the repo lives on one. `~/.cache` works:

```sh
cd packaging/flatpak
B=~/.cache/playdrop-flatpak
flatpak run org.flatpak.Builder --user --install-deps-from=flathub --force-clean \
    --state-dir="$B/state" --repo="$B/repo" "$B/build-dir" org.playdrop.PlayDrop.yml
```

Do not put the state or build directory under `/tmp` when `/tmp` is private to the shell
that starts the build: `org.flatpak.Builder` runs build commands through the host, which
then cannot see the directory ("Failed to change to directory").

Single-file bundle, then install it user-level and run:

```sh
flatpak build-bundle --runtime-repo=https://dl.flathub.org/repo/flathub.flatpakrepo \
    "$B/repo" playdrop.flatpak org.playdrop.PlayDrop
flatpak install --user -y ./playdrop.flatpak
flatpak run org.playdrop.PlayDrop                              # the window
flatpak run --command=playdropd org.playdrop.PlayDrop          # the daemon
flatpak run --command=playdropd org.playdrop.PlayDrop --help   # smoke test, no display needed
```

`playdrop.flatpak` lands in `packaging/flatpak/`, which `packaging/.gitignore` ignores
(`*.flatpak`). To install straight from the build instead, add `--install` to the builder
command. Remove with `flatpak uninstall --user org.playdrop.PlayDrop`.

CI builds the same bundle for every release in `.github/workflows/release.yml` with
Flathub's `flatpak-github-actions` image for runtime 25.08; change that image tag together
with `runtime-version` in the manifest.

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
| `--talk-name=org.mpris.MediaPlayer2.*` | Media control (Phase 5): list MPRIS players and send play, pause and next |
| `--filesystem=xdg-download/PlayDrop:create` | The receive folder, `~/Downloads/PlayDrop` |

No bus permission is needed for the daemon's IPC name `org.playdrop.PlayDrop.Daemon1`:
Flatpak's default session bus policy lets an app own its own namespace (`$FLATPAK_ID` and
every name under it), and owning a name includes talking to it, so the window reaches the
daemon without `--own-name` or `--talk-name`.

Portals need no permission: `org.freedesktop.portal.Desktop` is reachable from every
sandbox, so the RemoteDesktop portal (remote input; it asks the user for consent itself),
the Background portal (autostart) and the file chooser portal work without an entry.

Never added: `--socket=session-bus` (PLAN.md anti-pattern 16) and `--filesystem=home`.
Files to send are chosen through the file chooser portal, which grants access to just the
picked files.

Inside the sandbox `XDG_DATA_HOME` is `~/.var/app/org.playdrop.PlayDrop/data`, so keys,
pins and settings live in `~/.var/app/org.playdrop.PlayDrop/data/playdrop/` (SR-28 modes
still apply).

## The command line

`/app/bin/playdrop` is the same `playdrop` as in the `.deb` (`docs/guide/cli.md`). Flatpak
exports no commands to the host `PATH`, so it is started with `flatpak run`:

```sh
flatpak run --command=playdrop org.playdrop.PlayDrop peers
flatpak run --file-forwarding --command=playdrop org.playdrop.PlayDrop send @@ report.pdf dir @@ --to pixel
```

Sending needs `--file-forwarding` and the `@@` marks. The CLI passes the daemon open file
handles, never paths, and the daemon keeps a handle only if the path behind it is visible in
its own sandbox (`fd_path` in `playdrop-ipc`). Flatpak replaces each path between the marks
with a document portal path under `/run/user/<uid>/doc/`, which every instance of the app
sees, so the CLI can open it and the daemon can read it. Nothing is added to `finish-args`
for this. A host-installed `playdrop` cannot send through a Flatpak daemon: its handles
point at host paths the sandbox does not have.

A path the sandbox cannot see fails as "No such file or directory", so inside Flatpak the
CLI adds one sentence with the command above. Files under `~/Downloads/PlayDrop` are shared
by `--filesystem` and need no marks. Each forwarded file stays in the app's document list
(`flatpak documents org.playdrop.PlayDrop`) until removed. Not checked: whether the document
path survives a logout or reboot, which a send resumed after one would need.

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

On 2026-10-13, on Pop!_OS with Flatpak 1.18.1 and `org.flatpak.Builder`:

- `update-sources.sh` regenerated `cargo-sources.json` (753 crates) byte for byte.
- The build above finished offline from the vendored crates, `flatpak build-bundle` wrote a
  12 MB bundle, it installed with `flatpak install --user`, and
  `flatpak run --command=playdropd org.playdrop.PlayDrop --help` ran inside the sandbox.
  `flatpak info --show-permissions` listed exactly the `finish-args` above. Flatpak
  exported the D-Bus activation file with `Exec` rewritten to `flatpak run ...
  --command=/app/bin/playdropd`.
- The desktop and metainfo files pass `desktop-file-validate` and
  `appstreamcli validate --pedantic` (one pedantic note: the app ID has upper-case letters).
- The window was not opened from the Flatpak, and nothing was sent to or from it yet.

On 2026-11-09, same machine, Flatpak 1.18.1 and xdg-desktop-portal 1.18.4, with the command
line in the bundle: a daemon inside the sandbox was paired with a `playdrop host` outside it.
`flatpak run --command=playdrop org.playdrop.PlayDrop peers`, `status` and `outbox` answered.
With `--file-forwarding`, `send` delivered a 2.9 MiB file by absolute path, a file with a
space in its name plus a folder with a subfolder by relative path, and a file from `/tmp`;
`outbox add` queued a file; every SHA-256 on the receiving side matched. Without the marks a
file under the home folder failed with "No such file or directory" (the sentence that names
the fix was added after this run and is covered by a unit test), and a file under
`~/Downloads/PlayDrop` sent without marks.
- `flatpak-builder-lint manifest` reports two errors that only matter for a Flathub
  submission: `finish-args-login1-system-talk-name` (Flathub wants a justification or the
  portal's power signals instead) and `appid-url-not-reachable` (Flathub expects the app ID
  to match a domain the project controls, for example `io.github.qasimbhb.PlayDrop`). Both
  need a decision before any Flathub submission; GitHub release bundles are unaffected.

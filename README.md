# PlayDrop

PlayDrop links your phone and your computer over the local network. It does two jobs: an
always-on companion link (clipboard, ping and find, battery, media control, remote input) and a
fast file drop that resumes after a dropped connection and holds its speed for thousands of small
files. There is no account and no cloud: devices talk directly to each other on your LAN, and
nothing leaves it.

Licence: GPL-3.0-or-later. Version: 0.2.0 in development; the latest release is
[v0.1.0](https://github.com/QasimBhb/PlayDrop/releases/tag/v0.1.0). See [CHANGELOG.md](CHANGELOG.md).

## Status

PlayDrop is early software. What has been run, and by whom:

- **Pop!_OS COSMIC and Android 16** (a Galaxy A54): tested by hand, including receiving a file
  with the phone's screen off, clipboard, remote input and media control.
- **Windows 10 and 11**: built and tested by GitHub's Windows runner. No person has run it on a
  real Windows machine yet.
- **iPhone**: sideload only (no App Store). Four of the five device checks passed. The share
  extension does not work without an App Group, which a sideloaded copy usually lacks. It
  receives only while open.
- **Android 14 and 15, Ubuntu GNOME, mesh Wi-Fi, VPNs**: not tested.
- **Guest Wi-Fi paths** (QR, hotspot, USB tether): written and wired, not tried on a real
  hotspot or cable.

The full list, with the evidence for each line, is in
[docs/release/test-matrix.md](docs/release/test-matrix.md). Throughput numbers are in
[docs/release/throughput.md](docs/release/throughput.md): on the A54, 1 GiB arrived at 94 to 99%
of what `iperf3` measured on the same Wi-Fi. Thousands of small files to an Android phone were
slow in 0.1.0; 0.2.0 changes how they land.

## Install

Every file is on the [Releases](https://github.com/QasimBhb/PlayDrop/releases) page, with a
`SHA256SUMS` file. Step by step: [docs/guide/install.md](docs/guide/install.md).

| Device | File | Notes |
|---|---|---|
| Pop!_OS, Ubuntu | `playdrop-<version>-linux-amd64.deb` or `playdrop-<version>-linux-x86_64.flatpak` | `sudo apt install ./file.deb`, or `flatpak install --user ./file.flatpak` |
| Windows 10, 11 | `playdrop-<version>-windows-x64-setup.exe` | Asks for administrator rights to add a firewall rule for `playdropd.exe` on port 42187. Not code signed, so SmartScreen warns: More info, then Run anyway |
| Android 8+ | `playdrop-<version>-android.apk` | Allow installs from your browser once. [Obtainium](https://github.com/ImranR98/Obtainium) can update it from this repository |
| iPhone, iOS 17+ | `playdrop-<version>-ios-unsigned.ipa` | Unsigned. Sideload with SideStore or AltStore, and refresh every 7 days. See [docs/release/distribution.md](docs/release/distribution.md) |

## First pairing

1. Install PlayDrop on both devices and open it.
2. Open **Add device** on both.
3. Pick the other device. Both screens show the same six digits.
4. If the digits match, accept on both. Or scan the QR code shown on one device with the other.
5. If the devices do not see each other, type the computer's address, or see
   [Can't find your device?](#cant-find-your-device).

More in [docs/guide/pairing.md](docs/guide/pairing.md).

## What it does

- **Send files and folders** in either direction. A transfer resumes where it stopped. Devices
  you paired accept files without a question, and a notification tells you what arrived.
  [Guide](docs/guide/sending.md).
- **Clipboard sync.** Automatic on COSMIC and Windows, a button on GNOME and iPhone, a notification
  action on Android. Each device has its own permission for sending and receiving. A clip a
  password manager marked secret is not sent automatically. [Guide](docs/guide/clipboard.md).
- **Ping and find my phone**, and the **battery** level of a paired phone.
- **Media control.** See and control what the computer is playing from the phone.
- **Remote input.** Use the phone as a trackpad, keyboard or presentation remote. It is off until
  you turn it on, on the computer, and there is a kill switch. [Guide](docs/guide/remote-input.md).

## Command line

`playdrop` talks to the running daemon on Linux and Windows.

```
playdrop send <PATH>... [--to <peer>] [--no-wait] [--json]
playdrop peers | status | transfers | outbox | diagnose  [--json]
playdrop clip (<TEXT> | --stdin) [--to <peer>]
playdrop ping [--to <peer>] [MESSAGE]
playdrop outbox add <PATH>... --to <peer>
```

Exit codes: 0 ok, 1 failed, 2 usage or unknown device, 3 daemon not running. Details in
[docs/guide/cli.md](docs/guide/cli.md).

## Can't find your device?

When a device does not appear, open Diagnostics. It names the layer that failed:

1. **Not the same network.** Guest, hotel and campus Wi-Fi often block devices from reaching each
   other. Use the phone's hotspot or a USB tether.
2. **Multicast blocked** (mesh Wi-Fi, some routers, VPNs). Pair by QR code or by the computer's
   address. Paired devices then reconnect without looking for each other.
3. **Port blocked.** PlayDrop uses port 42187 (TCP and UDP). Diagnostics shows the firewall
   command, for example `sudo ufw allow 42187`.

KDE Connect and LocalSend can stay installed: PlayDrop never uses their ports. Full steps:
[docs/guide/cant-find-your-device.md](docs/guide/cant-find-your-device.md) and
[docs/guide/troubleshooting.md](docs/guide/troubleshooting.md).

## User guide

[Install](docs/guide/install.md) -
[Pairing](docs/guide/pairing.md) -
[Sending files](docs/guide/sending.md) -
[Clipboard](docs/guide/clipboard.md) -
[Remote input](docs/guide/remote-input.md) -
[Can't find your device?](docs/guide/cant-find-your-device.md) -
[Command line](docs/guide/cli.md) -
[Troubleshooting](docs/guide/troubleshooting.md)

## Building from source

Rust workspace; run `export PATH="$HOME/.cargo/bin:$PATH"` first. Read [CLAUDE.md](CLAUDE.md) for
the full command list.

- **Workspace:** `cargo build --workspace` (add `--release` for real speed), `cargo test --workspace`,
  `cargo clippy --workspace --all-targets`. On Linux the window needs
  `libfontconfig1-dev libxkbcommon-dev libgl1-mesa-dev`.
- **Android** (from `apps/android`; needs the Android SDK, NDK 27.1 and `cargo-ndk`):
  `./gradlew :app:assembleDebug`, or `:app:assembleRelease` with a signing key. Never judge speed
  from a debug build.
- **iPhone from Linux** (from `apps/ios`, after the one-time xtool setup in its README):
  `scripts/xtool-dev.sh` builds, signs with a free Apple team and installs on a USB-connected
  iPhone.
- **iPhone on a Mac** (Xcode 16+, XcodeGen): `scripts/build-core.sh`, copy
  `Local.xcconfig.example` to `Local.xcconfig` and set your team, `xcodegen generate`, then open
  `PlayDrop.xcodeproj`.

The iOS app version is copied from the workspace version in `Cargo.toml` by
`apps/ios/scripts/sync-version.py`; the build scripts run it.

## Security

Every connection is TLS 1.3 with certificates checked on both sides, over one port. A device is
identified by the hash of its public key, pinned when you pair, not by its name or address.
Pairing compares a six-digit code derived from both devices' keys, or scans a QR code that carries
the key and a one-time secret. Discovery is private: PlayDrop only connects to devices you
paired or one you chose, and a broadcast from a stranger never makes it connect. Permissions
(files, clipboard, input, media) are per device and change only on the device itself. The threat
model and its numbered rules are in [docs/threat-model.md](docs/threat-model.md); the wire
format is in [docs/protocol.md](docs/protocol.md).

## Project documents

[PLAN.md](PLAN.md) (research and decisions), [docs/product.md](docs/product.md) (scope),
[docs/phases/](docs/phases/) (one handoff per build phase), [THIRD_PARTY.md](THIRD_PARTY.md).

## Licence

GPL-3.0-or-later. See [LICENSE](LICENSE). Code reused from KDE Connect or LocalSend keeps its
original header and is listed in [THIRD_PARTY.md](THIRD_PARTY.md).

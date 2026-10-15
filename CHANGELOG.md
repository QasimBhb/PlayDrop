# Changelog

All notable changes to PlayDrop are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/). One version number covers every part of PlayDrop:
the desktop daemon and window, the Android app and the iOS app.

The release workflow uses the section for the tagged version as the text of the GitHub release.
Before tagging, give the version's heading its release date in place of "Unreleased".

## [0.2.0] - Unreleased

### Added

- The `playdrop` command line can now send files and drive the running daemon: `send`, `peers`,
  `status`, `transfers`, `clip`, `ping`, `outbox` and `diagnose`, with `--json` output and set
  exit codes. See `docs/guide/cli.md`.
- A README and a user guide in `docs/guide/`: install, pairing, sending, clipboard, remote input,
  "Can't find your device?", the command line and troubleshooting.
- A test that fails if secrets end up in the logs. It guards the rule that logs never hold keys,
  pairing secrets or the contents of what you send.

### Changed

- Android: dropping many small files now lands at full speed. The files go to the app's own
  folder first and are then moved into Downloads in the background, with progress in the
  notification. Before, thousands of small files arrived at about 2 MiB/s because Android
  records every new file in Downloads one at a time.
- Android: large files are now moved into Downloads instead of copied, so they are not written
  twice.
- Start at login: when the system refuses to turn it on (for example the Flatpak Background
  portal), PlayDrop now reports the exact refusal instead of a general message.
- iOS: the app version now follows the workspace version, so the app, the Android app, the
  packages and the installer all carry the same number.
- The diagnostics report shows device IDs shortened to 8 characters, like the logs, so it can be
  pasted into a bug report.

### Fixed

- Cancelling a send while the other side was still deciding, or while bytes were flowing, left
  the transfer open on the receiving device. After four such cancels the receiver refused new
  offers for a day. The receiver is now told about every cancel.
- If the receiving app was killed while a bundle of many small files was being placed, the
  files placed before the kill were received again as "name (1)" copies on the next start. The
  receiver now remembers which files it already placed.

## [0.1.0] - 2026-10-15

First public release, from GitHub Releases only (no app stores).

### Added

- A background service for Linux and Windows (`playdropd`) with a tray icon and a window, so the
  computer receives while the window is closed. Linux is tested on Pop!_OS COSMIC. Windows is
  built and tested by GitHub's runner and has not been run by a person yet.
- An Android app that keeps a foreground service running, receives with the screen off, and
  reconnects after a Wi-Fi change without pairing again. Tested on Android 16.
- An iPhone app and share extension. It receives only while it is open. It is sideloaded, and
  without an App Group the share extension cannot hand files to the app.
- Pairing by comparing a six-digit code on both screens, or by scanning a QR code. Each device
  is identified by its key, not its name or address.
- Sending files and folders, with auto-accept from devices you paired, resume after a dropped
  connection, and steady speed for many small files.
- Clipboard sync (automatic on COSMIC and Windows, by button on GNOME and iOS, by notification
  action on Android) with a grant per device and direction.
- Ping, find my phone, battery level and media control.
- Remote input from a paired phone: trackpad, keyboard and presenter mode. It is off until you
  turn it on for that device on the computer.
- "Can't find your device?" help on all three platforms, with a diagnosis that names the failing
  layer (network, multicast or port) and the way out: QR code or typed address, phone hotspot,
  USB tether, and the firewall command.
- Releases: a signed Android APK, a `.deb`, a single-file `.flatpak`, a Windows installer (not
  code signed), an unsigned iPhone `.ipa`, and `SHA256SUMS`.

### Known limits

- Many small files to an Android phone arrive at about 2 MiB/s (fixed in 0.2.0).
- Windows, Android 14 and 15, Ubuntu GNOME, mesh Wi-Fi, VPNs and resume over Wi-Fi have not been
  tested by a person. See `docs/release/test-matrix.md`.
- The guest Wi-Fi paths (QR, hotspot, USB tether) are written and wired but were not tried on a
  real hotspot or cable.

[0.2.0]: https://github.com/QasimBhb/PlayDrop/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/QasimBhb/PlayDrop/releases/tag/v0.1.0

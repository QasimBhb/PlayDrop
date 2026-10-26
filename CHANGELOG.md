# Changelog

All notable changes to PlayDrop are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/). One version number covers every part of PlayDrop:
the desktop daemon and window, the Android app and the iOS app.

The release workflow uses the section for the tagged version as the text of the GitHub release.
Before tagging, give the version's heading its release date in place of "Unreleased".

## [0.4.0] - Unreleased

### Added

- A send that gives up because the other device could not be reached is no longer lost. It
  shows as failed with "resumes when <device> is back", and a computer continues it by itself,
  from where it stopped, when that device reconnects or when its own daemon restarts.
- Sending the same files to the same device again within 24 hours continues the earlier
  transfer instead of starting over, on every platform. This is how a phone resumes: share the
  same items again.
- `playdrop cancel <id>` cancels a transfer, or forgets a failed send that was waiting to
  resume. `playdrop transfers` shows such a send as `failed (resumes)`, and the window, the
  Android app and the iOS app show a Forget button on its row.

### Changed

- Many small files reach an Android phone faster: received files were flushed to storage one
  by one on Android, and are now flushed in one go where the storage allows it safely.
- Received files are moved into place in one step that never overwrites anything, instead of
  first creating an empty file at the name. Faster for folders of small files, and a crash can
  no longer leave an empty file behind (Linux, Android and Apple; Windows keeps the old way).
- A sender that was stopped in the middle of a large file no longer re-reads that file's
  whole beginning over the network after a restart: it checks it against what the receiver
  already holds on its own disk first.

### Fixed

- A receiver restarted after a transfer had finished could get the same files again from a
  sender that missed the end; it now remembers finished transfers for 24 hours.
- A shrinking source file in the middle of sending many small files could trip an internal
  error in the sender instead of a clear failure.

### Notes

- Downgrading to 0.3.0 while a transfer to that device is half done can place some files a
  second time with " (1)" names. Let transfers finish before downgrading.

## [0.3.0] - 2026-10-17

### Added

- Continuous checks for Linux and Android: every push now formats, lints, builds and tests the
  Rust code on Ubuntu, and compiles and unit-tests the Android app (including regenerating the
  Kotlin bindings), so breakage shows up before a release.
- Transfers report how many files are done next to the byte count: `playdrop send` and
  `playdrop transfers` show "1,204 of 5,000 files", and the window and the Android app show it
  on active transfer rows.
- `playdrop clip` now says why nothing was sent: the text matches the last clip, no connected
  device has clipboard sharing on, or the connected devices cannot take it. The window and the
  daemon's notifications use the same words.

### Changed

- Folders with many small files land while the transfer is still running instead of all at
  the end, so big drops finish much sooner, most of all on phones.
- A send now shows as "offered" until data actually starts moving, instead of jumping to
  "active" with zero progress while the other device is still deciding. A declined offer no
  longer reports any progress.
- A finished transfer stays in the transfer list for at least a minute, even after a burst of
  other transfers, so a late client always sees how it ended.
- The send progress line now fits the real terminal width.
- Going back to a 0.2.x build in the middle of a transfer is not fully safe: a 0.2.x build does
  not understand the new delivery log, so files already saved early in a large folder transfer
  can be saved a second time as "name (1)".

### Fixed

- The release build itself, found by the first v0.3.0 run: the Flatpak job stopped on a crate
  missing from its offline crate list (the list is regenerated and now checked on every push),
  and a receiver restarted mid-transfer could answer a reconnecting sender before it had
  reloaded the transfer, which the Windows test run caught; it now reloads first.
- A file being saved at the moment the app is killed no longer comes back as "name (1)" or
  gets downloaded again.
- A temporary disk error while saving received files no longer makes the sender start over:
  the receiver keeps the data and finishes saving on the next try. The sender keeps trying
  for at least 10 seconds before it gives up, instead of about one second.
- Android: if a received file had to be renamed because its name was already taken in
  Downloads, tapping the notification or "Open" in the Transfers tab now opens the renamed
  file, and the notice says how many files were renamed.
- Pressing Ctrl-C right as `playdrop send` starts now stops it cleanly.

## [0.2.0] - 2026-10-15

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
  records every new file in Downloads one at a time; now 5,000 of them reach the phone in
  18 s instead of 86 s on the test phone, and Downloads about a minute later.
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

# Changelog

All notable changes to PlayDrop are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/). One version number covers every part of PlayDrop:
the desktop daemon and window, the Android app and the iOS app.

The release workflow uses the section for the tagged version as the text of the GitHub release.
Before tagging, give the version's heading its release date in place of "Unreleased".

## [0.13.0] - 2026-11-13

### Fixed

- Linux `.deb`: after an update installed itself, the background service could sit half
  stopped for 90 seconds and then be killed and started again by the system. It read the
  system's own stop signal as a refusal to restart. It now hands the restart over cleanly.
- The desktop window no longer shows a red "PlayDrop is starting; try again" line when it
  asks a freshly started PlayDrop for its state. It waits a moment and asks again.

## [0.12.0] - 2026-11-13

### Added

- `playdrop restart` stops the running background service and starts the installed one again,
  then prints the old and the new version. Use it after installing a new `.deb` by hand
  (`sudo apt install ./playdrop-<version>-linux-amd64.deb`), which leaves the old service
  running. It starts the service the same way it was running (the systemd user unit, D-Bus
  activation, or by running `playdropd`), so it also works when `systemctl --user restart`
  does not. The package now prints a one-line reminder when the old service is still running
  after an install.

### Changed

- One `.deb` for Pop!_OS and Ubuntu: the COSMIC panel applet is now inside the `playdrop`
  package, so one download and one install gives everything. The applet still only runs once
  you add it to a panel in COSMIC Settings, and other desktops never start it. Updating from
  0.11.0 needs nothing special: the Update button and `apt` move an installed applet over by
  themselves. The release still carries a small `playdrop-cosmic-applet` file for two releases
  so that 0.11.0's Update button keeps working; it is an empty package you can remove at any
  time with `sudo apt remove playdrop-cosmic-applet`.

## [0.11.0] - 2026-11-12

### Added

- Update check. When you open PlayDrop it asks GitHub, at most once a day, whether a newer
  release exists, and offers "Update" or "Later". The `.deb` and the Windows installer
  download the new version, check it and install it (the system asks for your password or
  permission); Android downloads the APK and hands it to the system installer; the Flatpak
  and the iPhone app show a notice with a link. The request carries no name, ID or version;
  GitHub sees your IP address, as with any download. A release is only trusted through a
  signature made with a PlayDrop release key (`SHA256SUMS.sig`, new in this release), and a
  download that does not match its signed checksum is deleted. Settings has a switch to turn
  the check off, and "Check now". `playdrop update` does the same from a terminal. See
  `docs/design/update-check.md`.
- Android: the device page has "Send files", "Send photos and videos" and "Send clipboard"
  at the top. The "Allow this device to" switches and notification mirroring moved to their
  own "Permissions" page.
- Android: sharing with more than one device paired shows a chooser with a tick per device,
  "Select all" and your default device already ticked; one tap on Send sends to every ticked
  device. With one device paired sharing is one tap, as before.
- Android: "Send clipboard" on the notification says which devices got it, and asks which
  ones when more than one would. A copied image can go to a chosen device.
- When a computer refuses a file because its disk is nearly full it now shows a notification
  saying so, with how much is free and how much PlayDrop keeps in reserve.

### Fixed

- Windows: PlayDrop did not open on some PCs and gave no reason. The programs no longer need
  the Visual C++ runtime to be installed, no longer open a console window, fall back to
  drawing without the graphics card when it fails, and any start-up failure now shows a
  message and writes a log under `%LOCALAPPDATA%\PlayDrop\logs`. The window says why when
  the background program is missing or stopped. Proven by automatic tests on GitHub's
  Windows machines, not yet on a real PC.
- A failed send no longer shows raw text such as "offer refused: 507 Insufficient Storage
  no_space". Every platform says what happened in plain words ("calliope does not have
  enough free space.").
- The phone's trackpad screen no longer just closes when the computer refuses remote
  control. It says why (turned off for this phone, the computer is locked, stopped on the
  computer) and what to do, and it resumes by itself after a short connection drop.
- Mirrored phone notifications no longer pop up again on the computer after the phone
  reconnects. Copies stay for 5 minutes while the phone is away.
- Unpairing a device during remote input now ends the session at once and releases any held
  key or button.
- Android: devices that removed this phone are no longer offered as send targets.

### Changed

- Only devices that handle phone notifications announce that feature (the command-line host
  and the iPhone no longer do).
- Removed and dismissed notification messages are now rate limited like new ones.
- The release files now come with `SHA256SUMS.sig`. `docs/release/distribution.md` shows how
  to check a download by hand.

## [0.10.0] - 2026-11-12

### Added

- Phone notifications on the computer (Android to desktop). Notifications from the apps you
  pick on the phone show up on a paired computer, close there when they go away on the phone,
  and closing one on the computer dismisses it on the phone. It is off until you turn it on,
  on the phone, in three places: Android's "Notification access" for PlayDrop, a switch for
  the one computer, and a tick for each app. Ongoing notifications, group summaries and
  anything an app marks secret are never sent, and notification text is never written to
  logs. The computer has its own switch per phone to refuse them. Linux shows, replaces and
  closes them; Windows shows a plain notification. No replies, buttons or icons yet.
  See `docs/guide/notifications.md`.

### Fixed

- The phone no longer shows a computer as online for up to 12 minutes after the computer
  dropped off the network without saying goodbye (lid closed, Wi-Fi cut). The side that
  waits for the other's regular check-in now asks by itself after 75 seconds of silence and
  gives up 15 seconds later, so a dead link is noticed in under two minutes. On Linux and
  Android anything sent to a peer that is gone also ends the link within a minute.
- Android: opening the app brings back the always-on notification if you swiped it away.
  Before, only a Force stop and a fresh start did.

### Changed

- The saved list of paired devices and the answers the background service gives now carry
  two more permissions. They are only written once you change one, so an older PlayDrop
  keeps reading them until then; after that, update the panel applet together with the
  rest, and an older PlayDrop installed over this one will not start until the two new
  permissions are back at their defaults.

## [0.9.0] - 2026-11-10

### Fixed

- Quit now stays quit on Linux. With the PlayDrop window open, choosing Quit in the tray stopped
  the background service and the window started it again within a moment. The window now waits
  for the service and offers Start; it reconnects by itself when the service is started again
  from the panel applet, the command line or the window.
- The Flatpak on COSMIC now says why the clipboard does not sync by itself. COSMIC keeps the
  clipboard away from sandboxed apps, so the Flatpak can neither watch it nor put a clip from
  your phone on it. Install the `.deb` on COSMIC for automatic sync; in the Flatpak the Send
  clipboard button still works.
- Windows: the Start Menu shortcut now carries the app's ID. Without it Windows may have dropped
  every PlayDrop notification. Not yet confirmed on a real PC.

### Added

- A device's battery shows as soon as the window, the panel applet or `playdrop peers` opens,
  instead of up to a minute later.
- Send clipboard from the panel applet now sends images as well as text. The service reads the
  clipboard itself, and `playdrop clip` with no text sends what is on the clipboard.
- Transfers show what is being sent ("Photos", "notes.txt and more") in the window, the applet
  and `playdrop transfers`, also when you open them in the middle of a transfer.
- Windows: the tray menu lists your devices with their state and battery, and "Stop remote
  input" appears while a phone controls the computer, as on Linux.
- iPhone: the app has an icon (the same drop as on Android) instead of a blank tile.

### Changed

- A clip that a password manager marked as secret is still held back until you agree. Your
  agreement now covers only that clip: if the clipboard changed in between, PlayDrop asks again.
  A clip you agree to send always arrives marked as secret, so the other device keeps it out of
  its clipboard history.

### Added, off unless you turn it on

- Windows: "Open" and "Show in folder" buttons on the "file received" notification. Nobody has
  clicked one on a real PC yet, so they are off. Start the service with
  `PLAYDROP_TOAST_BUTTONS=1` to try them.

### Tested

- The panel applet was looked at and clicked through by a person on COSMIC 1.9: the icon with
  the service stopped and running, Start, pairing a phone, sending the clipboard, a file and a
  folder, the battery, and Quit followed by Start.

### Known limits

- The Windows tray lines, notifications and their buttons are built and tested by GitHub's
  Windows machine but have not been used by a person on a real Windows PC.
- The applet's question before sending a password-manager clip was not tried by a person.
- When the computer drops off the network without saying goodbye, a phone can keep showing it
  as online for up to 12 minutes.

## [0.8.0] - 2026-11-09

### Added

- The `playdrop` command line now comes with every desktop download. The guides told you to run
  `playdrop send`, but the `.deb`, the Flatpak and the Windows installer did not contain it.
  - `.deb`: `playdrop` is on your `PATH` after the install.
  - Windows: the installer adds it to `PATH` unless you clear that box. Open a new terminal.
  - Flatpak: run it with `flatpak run`, and put files between `@@` marks so the app can see
    them. `docs/guide/cli.md` has the one line to add to your shell. A file the app cannot see
    now fails with a sentence that says how to pass it.
- An optional panel applet for the COSMIC desktop, as its own download
  (`playdrop-cosmic-applet-<version>-linux-amd64.deb`). It shows your devices with their battery,
  sends the clipboard or files to a device in one click, shows transfers as they move, and has a
  stop button while a device controls the computer. The tray icon keeps working without it.

### Tested

- The Windows installer is now installed, checked and uninstalled on GitHub's Windows machine
  for every build: the programs are in place, `playdrop` runs from `PATH`, the firewall rules are
  limited to the PlayDrop service, and uninstall removes all of it. Nobody has clicked through it
  on a real Windows PC yet.
- The command line inside the Flatpak sent files and folders to another device with every
  checksum matching, on Pop!_OS.

### Known limits

- The applet's open panel has been run on COSMIC 1.9 but not looked at or clicked by a person
  yet. Its logic is covered by tests; its looks are not.
- In the Flatpak, it has not been checked whether a file passed between `@@` marks stays
  readable after the computer restarts. If a stopped send cannot find its file, send it again.

## [0.7.0] - 2026-11-03

### Changed

- A phone or computer that answers on several addresses over one radio now counts as one connection
  when PlayDrop picks the fastest path. A check used to measure each address, up to 4 MiB each, to
  learn nothing. Both devices need 0.7.0 for this; with an older device it measures as before. On a
  Galaxy A54 the second address was set aside without being measured.
- Default builds move to rustls 0.23.46.

### Added, off in every download

- An experiment in reaching a device on another network, through a relay you run yourself. It is
  behind a build option that no release file turns on, so what you download contains none of it.
  It has been tested on one computer with simulated networks, not on a phone or a real carrier
  network. See `docs/design/iroh-reach.md`.

### Tested

- Sending a 1 GiB file to a phone is no slower than in 0.5.0: 42.6 MiB/s for both on a Galaxy A54.
  This answers the mixed result listed under 0.6.0.
- Force-stopping the Android app in the middle of a send on 0.6.0: the send carried on by itself
  and every file arrived correct, for both 1,000 small files and a 1 GiB file. The 1,000-file run
  left 2 duplicates, inside the limit of 4.

### Known limits

- Choosing between USB tethering and Wi-Fi has still been tested with simulated links, not on a
  real phone with both turned on.

## [0.6.0] - 2026-11-02

### Added

- PlayDrop now picks the faster connection when a phone and a computer can reach each other more
  than one way, such as USB tethering and Wi-Fi. The fastest path used to lose to whichever
  answered first; a USB 2.0 tether capped a phone at about 27 MiB/s while its Wi-Fi could do 45
  to 55. It measures with a short test and moves only when no transfer is running and the other
  path is at least 1.25 times faster. It skips the test on low battery and on sleeping devices.

### Changed

- On Android, sending thousands of small files to a phone takes about half the time: 5,000 files
  took 7 to 11 seconds in clean runs, against 14 to 19 before, measured on a Galaxy A54 with both
  apps taking turns. How soon they reach Downloads afterwards has not changed.
- The desktop command-line host (`playdrop host`) on Windows now has the daemon's protections: an
  encrypted key and device list, a private data folder, a real free-space check, and exclusive
  use of its port.

### Known limits

- Choosing between USB tethering and Wi-Fi has been tested with simulated links on one computer,
  not yet on a real phone with both turned on.
- A kill in the middle of a large drop of small files can leave up to four files in Downloads
  twice, the second with a name such as `a (1).txt`. No file is lost or damaged.
- Sending a 1 GiB file to the phone gave mixed results in this release's tests: 24 to 35 seconds,
  against 22 to 34 before. There were too few runs to tell whether it changed. Resuming after a
  kill was tested on a computer for this release, not on a phone.

## [0.5.0] - 2026-10-29

### Changed

- On Android, a drop of thousands of small files reaches Downloads about twice as fast after
  it has arrived: 5,000 files are in Downloads about 31 seconds after the send ends instead of
  about 48, and 1,000 files after 7 to 8 seconds instead of 9 to 12. The app now registers files with
  Android in groups of 100 and copies several at a time, and a crash in the middle still never
  leaves a file twice, loses one or touches a file PlayDrop did not write.
- A phone spends much less time making received small files durable: it now does so every
  64 MiB instead of every 16 MiB where it flushes the whole storage at once, because each such
  flush costs about a tenth of a second on the phone (5,000 small files are written in 1.7 s
  instead of 3.7 s on the phone itself; over Wi-Fi the link is still the limit). A transfer
  killed in the middle now sends up to 64 MiB again instead of up to 16 MiB.
- `playdrop` tables and the progress line line up names written in Chinese, Japanese, Korean
  or with emoji.

### Fixed

- Windows: the daemon now checks the real free space of the receive drive before accepting a
  transfer, so the free-space reserve is kept as on the other platforms.
- Windows: the device's private key and the list of paired devices are now encrypted for the
  signed-in user (DPAPI), and the data folder is readable only by that user and the system.
- Windows: the daemon now holds its port exclusively, so another program cannot take over its
  connections.
- Android: after the app is updated, its background service now starts again by itself, so
  the phone keeps receiving without being opened first.
- `playdrop cancel` on a transfer that has already finished now says so and exits normally
  instead of reporting that the transfer was not found.

### Notes

- Windows: the first start of 0.5.0 encrypts the key and the paired-device list in place. An
  older daemon, or `playdrop host`, started on the same data folder afterwards refuses to start
  with "sealed but this host has no secret store"; stay on 0.5.0 or later.

## [0.4.0] - 2026-10-28

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

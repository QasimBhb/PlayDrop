# Phase 16 handoff: the update check, Windows start-up, and sending from the Android device page (v0.11.0)

Reader: a fresh session that starts Phase 17. Read `CLAUDE.md`, `PLAN.md` ("Build phases" row 7,
"Anti-patterns"), `docs/design/update-check.md`, then this file. Code to read is named per item;
do not re-read the whole tree.

---

## 0. Scope as proposed at kickoff (2026-11-12)

Starting point: v0.10.0 released on 2026-11-12.

1. **Windows does not open** (reported second hand): find why from the code and CI, fix it, make a
   start-up failure show a message and write a log, add a CI check.
2. **Update check inside PlayDrop on every platform**, design note first: a Settings switch (default
   on), signed checksums, nothing about the user or device in the request.
3. **Android device page**: Send files, Send photos and videos, Send clipboard at the top; the
   switches on their own page. Same gap checked on iOS and the desktop window.
4. **Android share chooser** with tick boxes when more than one device is paired.
5. **"Send clipboard" on the notification** names the devices and uses the chooser.
6. **Mirroring follow-ups** from Phase 15 section 5, and the phone is told why its trackpad closed.

Added during the session, from the user hitting it: a send refused for lack of disk space showed
"offer refused: 507 Insufficient Storage no_space" on the phone and nothing on the computer.

Versions: workspace `0.11.0`; `apps/cosmic-applet` `0.11.0`; `playdrop-ffi` `0.6.0`.

---

## 1. Status

| Item | Value |
|---|---|
| Base | `main` at `e565d32` (v0.10.0 plus brief notes) |
| Built on | This Pop!_OS COSMIC machine, 2026-11-12, one session: nine builders (Opus for core, Windows, update and Android share; Sonnet for the device page, iOS and the applet), four independent Opus reviews, four fixers, two phone runs |
| Rust tests | 835 passed, 0 failed (`dbus-run-session -- cargo test --workspace --locked --no-fail-fast`); clippy `-D warnings` on Linux and on `x86_64-pc-windows-gnu` with the stub C compiler; `fmt --check` clean; Flatpak `check-sources` in step (901 crates) |
| Applet tests | 92 passed, clippy and `fmt --check` clean |
| Android | 392 JUnit tests; debug and release build types compile |
| iOS | 144 `PlayDropShared` tests on Linux, type check clean; the app itself is compiled only by macOS CI |
| CI | Green on all four platforms at `e7381cb`. The Windows run is the first with the new installer test (section 6) |
| Phone | A54, two runs, section 7 |

### 1.1 What landed

**Windows start-up (item 1).** Causes found in the code: both programs were console programs
whose errors went to a console that closed at once; nothing wrote a log; the release needed
`vcruntime140.dll`; Slint's software fallback does not cover an OpenGL context that fails when
the window is shown; the installer's upgrade step killed the daemon's whole process tree, which
would include an installer started by the daemon. Fixes:

- `.cargo/config.toml` links the C runtime statically for `x86_64-pc-windows-msvc`.
  `build-windows.ps1` runs cargo from the repo root and refuses to build with `RUSTFLAGS` set
  (either would silently drop that flag).
- `playdropd` and `playdrop-desktop` are GUI-subsystem programs; started from a terminal they
  attach to its console. `crates/playdrop-platform/src/startup.rs`: a log file at
  `%LOCALAPPDATA%\PlayDrop\logs\<program>.log` (rotates at 1 MiB), `report_fatal` (log line plus
  `MessageBoxW`), a panic hook, `PLAYDROP_NO_FATAL_DIALOG=1` for CI. A bad daemon command line
  goes the same way.
- The window restarts itself once with `SLINT_BACKEND=winit-software` when the renderer fails.
  Its not-connected screen says whether the daemon is missing, could not start, or exited (with
  the exit code and the log path), and on Windows it keeps trying to connect once a second.
- Installer: `/RELAUNCH=1` (daemon and window) and `/RELAUNCH=daemon` start PlayDrop again
  after a silent install, and a cancelled or failed install restarts the old one.

**Update check (item 2).** Design and reasons: `docs/design/update-check.md`. Threat model: B9,
T51 to T54, SR-45 to SR-48. In short: two GETs to `github.com/.../releases/latest/download/`
for `SHA256SUMS` and `SHA256SUMS.sig`, only when a UI opens, at most once a day, with a switch.
`playdrop_core::update` holds every decision (pinned Ed25519 keys, the checksum parser, the
strictly-newer rule, the redirect rule), bound for Kotlin and Swift. The daemon fetches with
`ureq`, the window shows a banner (Update, Later, Cancel during a download), `playdrop update`
does the same from a terminal, the applet shows one line. Android and iOS fetch with the
platform client. The release run signs `SHA256SUMS` and checks its own signature before
publishing.

**Android (items 3 to 5).** Device page with three send buttons and a Permissions page
(`ui/device/`). One chooser for share, the notification's file action and Send clipboard
(`share/TargetChoice.kt`, `ManySend.kt`, `SendSheet*.kt`). The sends run in the process scope,
so leaving the screen after Send does not lose them. One held set of descriptors serves every
device. A share never carries `file://` items or the app's own files.

**Core (item 6 and the no-space report).** `removed` 400 per minute and `dismiss` 60 per minute
per connection; `HostConfig.notifications: NotificationRole` (the capability is announced only
with a role); `InputEndReason` on `InputSessionEnded`, every refused `start` answered, unpair
and grant-off end a session at once; `push_clipboard_to` and `push_clipboard_image_to`;
`TransferFailure` on failed transfers with plain texts; `ReceiveRefusedNoSpace` (one per peer
per minute) and the desktop notification for it. The desktop mirror table keeps copies for 5
minutes over an absence and matches what the phone sends again
(`crates/playdropd/src/notify/mirror.rs`, protocol 5.16).

**Release.** v0.11.0 was tagged at `514cee2` on 2026-11-12 and its release run passed first time: six files plus `SHA256SUMS` and `SHA256SUMS.sig`. The user stored the Actions secret `PLAYDROP_UPDATE_SIGNING_KEY` (storing a secret is refused for the agent; the command is in `docs/release/distribution.md`). The published signature was fetched from the "latest" URLs and verified with `openssl` against `docs/release/playdrop-update.pub`.

**Reviews.** No critical finding. Fixed from them, among others: a `.deb` update could remove the
applet (`--no-remove`, probe at install time); the Windows daemon stayed in "installing" for
ever when the installer was cancelled; a pre-release build would be offered an older release;
sends lost after Send when the share screen was left; unpair never ended an input session (older
than this phase); remembered notification content hid genuine reposts; the installer test would
have hung 15 minutes on the relaunch install.

---

## 2. Repository map (new or changed)

```
.cargo/config.toml                                   static C runtime on Windows
crates/playdrop-core/src/update.rs                   New: every update decision, no network
crates/playdrop-core/src/host/{input,session,notifications,mod,receive,send,transfers,events}.rs
crates/playdrop-core/tests/{transfer_failures,clipboard_choice,phase5_core,notifications}.rs
crates/playdrop-ffi/src/update.rs, types.rs, events.rs, host.rs; Cargo.toml (0.6.0, feature update-test-key)
crates/playdrop-platform/src/startup.rs              New: log file, fatal report, panic hook
crates/playdropd/src/update/                         New: fetch, kind, store, install, restart, hooks
crates/playdropd/src/{main,cli,daemon,settings,notify}.rs, notify/mirror.rs
crates/playdrop-ipc/src/lib.rs                       update requests, Settings.update_check
crates/playdrop-cli/src/daemon/update.rs             `playdrop update`
apps/desktop/src/{main,daemon,ipc,model}.rs, screens/{connection,update,settings}.rs, ui/
apps/cosmic-applet/src/{app,daemon,model}.rs
apps/android/.../update/, share/, ui/device/, ui/input/InputEnd.kt, service/FailureText.kt,
  core/UriSource.kt, src/debug/, src/release/, app/build.gradle.kts (bindings per build type)
apps/ios/Packages/PlayDropShared/.../Update*.swift, InputEndRules.swift, TransferFailureWording.swift
apps/ios/PlayDrop/Core/UpdateChecker.swift, UI/Settings/, UI/Input/
packaging/windows/{playdrop.iss,test-installer.ps1,build-windows.ps1,README.md}
.github/workflows/{release.yml,windows.yml}; fuzz/fuzz_targets/update_sums.rs
docs/design/update-check.md, docs/release/{distribution.md,playdrop-update.pub}
docs/{protocol,threat-model,core-api,ipc,product}.md, docs/guide/cli.md, CHANGELOG.md
```

---

## 3. Public API

- Core and ffi 0.6.0: `docs/core-api.md` section 16 (update functions, `NotificationRole`,
  `InputEndReason`, `TransferFailure`, `ReceiveRefusedNoSpace`, the two subset clipboard
  pushes). Swift patterns for `inputSessionEnded` and `transferFailed` gained a value.
- IPC: `docs/ipc.md` 1.9 (`update_check`, `update_install { window }`, `update_later`,
  `update_cancel`, `update_state`, `update_open_page`, `UpdateChanged`, `Settings.update_check`).
- Wire: protocol stays 1. Compatible additions: `reason` on `error input_session_ended`,
  `reason: "no_space"` on the `transfer` decision frame.

---

## 4. Decisions made in Phase 16

- The version an update offers comes from the signed checksum file's names, not from GitHub's
  JSON API: the version itself is then authenticated and no rate-limited API is used.
- One key for one job: the update-signing key is neither the Android key nor a device key.
- A check runs only when a UI opens. The daemon and the Android service never check alone.
- The `.deb` update restarts the daemon by `exec`, or by `systemctl --user restart` when it runs
  as the `Type=dbus` unit (an exec there would get the new process killed).
- With the chooser, the tap on Send is the consent (SR-29); there is no extra 2 s wait after it.
- The desktop keeps mirrored copies over a short absence instead of the phone sending less.
- A clipboard push to chosen devices is not held back by "already sent".

---

## 5. Open TODOs

| Area | What | Phase |
|---|---|---|
| Update | Never run for real: `pkexec apt-get` (both start paths: autostart and the systemd unit, where polkit may find no agent), the exec restart, TLS to GitHub through `ureq`. First real proof is updating a 0.11.0 install to the next release | next release |
| Update | Windows: `ShellExecuteExW`, UAC, a cancelled installer, over-the-shoulder elevation. Type-checked only | when a PC is on offer |
| Update | Android and iOS still parse the "is this a release build" rule in app code; `update_check_available` exists in the bindings and should replace it | next |
| Update | iPhone request headers (`Accept-Language: en` and the rest) are as designed but only a device or macOS CI can show what `URLSession` really sends | when the iPhone is used |
| Windows | Click-through on a real PC: the message box, a PC with a failing graphics driver, the tray, toasts | when a PC is on offer |
| Android | Shared files cannot be reopened through `/proc`, so each extra device holds a second descriptor per item (seen: 500 files, 2 devices, peak 1,192). A budget check guards the limit; 5,000 files to 3 or more devices may be refused with a plain message | watch |
| Android | A share of more than 100 files cannot be started from Android's Files app, and Samsung's My Files sends 5,000 only to Quick Share, so the 5,000-file chooser case was not run on the phone | note |
| Android | Trackpad end reasons were not seen on the phone: a `playdrop host` peer cannot be controlled, so the screen never opens. Needs the real desktop with the grant off | first use |
| Mirroring | After a daemon restart the table is empty, so the phone's next resend shows all again | low |
| Mirroring | Still not seen on a real COSMIC notification stack; this laptop runs 0.9.0 | after installing 0.11.0 |
| Core | Against a 0.10.0 desktop that is locked, a `start` gets no answer and no event | fades with upgrades |
| Disk | Build output filled the home drive during this session (section 8) | every phase |
| Earlier phases | See `phase-15-notifications-and-dead-links.md` section 5 for the rest | later |

---

## 6. Windows

The installer test (`packaging/windows/test-installer.ps1`) now proves on GitHub's runner, and
did at `e7381cb`: no C runtime DLL and only Windows system DLLs in the import tables of the three
programs; GUI subsystem for the daemon and the window, console for `playdrop.exe`; the installed
daemon starts, stays up and answers `playdrop status`; the installed window starts, logs that it
connected and stays up; a second daemon exits 0; a daemon that cannot start exits non-zero with
a FATAL log line; `/RELAUNCH=1` and `/RELAUNCH=daemon` bring PlayDrop back after a silent install;
uninstall removes everything. Not provable there: anything a person must see.

A new Windows system DLL in the import table fails the test with its name; add it to
`$SystemDlls` in the script after checking it ships with Windows.

---

## 7. Live runs and scaffolding

All on the A54 (SM-A546U1) over USB adb, 2026-11-12. The installed daemon (0.9.0) was not used;
the test side was two `playdrop host` instances on ports 42191 and 42192, paired for each run and
unpaired after.

| Check | Result |
|---|---|
| Update request | Only `User-Agent: PlayDrop`, `Accept-Encoding: identity`, `Host`, `Connection: Keep-Alive` |
| Update flow | Dialog for 0.12.0 from a stand-in server and a throwaway key (debug build type); Later hides it; switch off sends nothing; download, checksum check and hand-off to Android's installer worked; the install itself stopped at Play Protect's PIN prompt (nothing typed) |
| Update refusals | One byte changed in the checksum file: nothing offered. One byte changed in the APK: "did not match its signed checksum and was deleted", cache empty |
| Device page | Three send buttons on top; a 1 KiB file arrived with a matching hash; the clipboard went to the chosen device only |
| Share chooser | 2 files to 2 hosts arrived with matching hashes; Home after Send still completes; a `file://` share is refused |
| 500 files to 2 hosts | 1,000 deliveries correct in about 10 s |
| Found and fixed | "Cancelled" shown although the files had arrived; the clipboard toast calling delivered devices offline; the notification's "Send files" opening the photo picker; tick boxes not announced. A second run on the release build of `b83ebae` passed all six checks: "Sent 2 items to test-one and test-two.", "Stopped sending to ..." matching what the hosts got, tick boxes reported as checkable, "Sent to test-one and test-two.", the document picker from "Send files", no raw codes on screen |

The stand-in for a release: a Python handler that logs request heads, a directory laid out as
`/QasimBhb/PlayDrop/releases/...`, `adb reverse`, and the debug build type's intent extras
(`org.playdrop.android.debug.UPDATE_BASE`, `UPDATE_TEST_KEY`, `UPDATE_CHECK_NOW`). Release
build types contain none of that.

Left on the phone on purpose: "Send the clipboard automatically" is off (turned off so test
text would not reach the user's devices; the user was told).

---

## 8. Disk space

The home drive went from 44 GB free to 7.8 GB while five builders compiled, and the installed
daemon then refused every incoming file (it keeps 5% of the drive free). Build output under
`~/.cache/playdrop-target*` had reached about 285 GB over 15 phases. Removed: three stale target
directories and the `incremental` folders. Rules from now on:

- Build with `CARGO_INCREMENTAL=0`.
- At the end of a phase delete `~/.cache/playdrop-target/debug/incremental` and any target
  directory made for one purpose.
- Before starting parallel builders check `df -h ~`; under 60 GB free, clean first.

---

## 9. Phase 17 kickoff brief

**After this file was written (2026-11-13):** the user had the applet merged into the main `.deb`
and `playdrop restart` added, released as v0.12.0 the same day (`CHANGELOG.md`,
`docs/release/distribution.md`). The laptop and the A54 run 0.11.0, so 0.12.0 is the first real
update offer for both: item 1 below can be done at once, without installing anything by hand.
An empty transitional `playdrop-cosmic-applet` package ships in 0.12.0 and 0.13.0; drop it from
0.14.0.

v0.11.0 is the state of `main`.

1. **Prove the update check for real.** Install the 0.11.0 `.deb` and applet on this laptop (the
   user's install is 0.9.0; ask before replacing it), then the next release is the first real
   update: watch `pkexec`, apt, the restart on both start paths, and the applet coming along.
   On the phone the same release shows the real dialog (the phone's debug-key build will get the
   "signed with a different key" sentence at the last step, which is the expected result).
2. **Replace the app-side release-build rule** with `update_check_available` on Android and iOS.
3. **Look at mirroring and the no-space notification on real COSMIC** once 0.11.0 is installed.
4. **Trackpad reasons on the phone** against the real desktop with the grant off, then locked.
5. If a Windows PC is on offer: the click-through in section 6 and Phase 14 section 6.
6. Candidates the user may want: icons and replies for mirrored notifications; an update notice
   on the Flatpak that names the exact command; a "Send to" history on the phone.

Build with `CARGO_TARGET_DIR=$HOME/.cache/playdrop-target CARGO_INCREMENTAL=0`, the applet with
`CARGO_TARGET_DIR=$HOME/.cache/playdrop-applet-target`. On a version bump set the applet's
version and its lock's three version lines by hand, run `apps/ios/scripts/sync-version.py`, add
the AppStream release entry, and run `packaging/flatpak/update-sources.sh` if `Cargo.lock`
changed.

---

## 10. Rules carried forward

- Hyphen only. Never the long dash, in code, comments, docs or commits.
- No AI attribution anywhere: no co-author trailers, no "generated with" lines, no session links.
- Parallel agents never edit any `Cargo.toml`, Gradle file, `Package.swift` or `project.yml` unless
  the brief names the one edit, and never commit.
- Never bind or announce on 1714-1764, 53317 or 224.0.0.167. Never contact public iroh relays or n0.
- The only request to the internet is the update check, to GitHub, as SR-45 describes. Tests never
  contact it.
- Manual runs use data and receive dirs on ext4, never under the repo. A data dir must be mode 700.
- Leave the installed daemon alone. A second `playdropd` cannot share the session bus with it: use
  `playdrop host`, or a private `dbus-daemon` with no service directories and no GUI programs on it.
- Never judge phone speed from a debug APK. Phone work goes under `flock /tmp/pd-phone.lock`. Never
  `pkill -f` or `pgrep -f`. Never toggle Wi-Fi over adb. State file count and time before a phone
  run. Never store the phone's PIN; a PIN prompt ends that step.
- A consent given on the phone for a test is taken back when the test ends, and before a clipboard
  test the phone's clipboard must hold test text only.
- Subagents: Opus for core and risky work, Sonnet for apps, scripts and docs, Haiku for lookups,
  never Fable; every diff goes through an independent reviewer.
- Run clippy on the exact tree you push, on Linux and for the Windows target. The applet is a second
  tree: run its gates too. A test with Unix paths needs `#[cfg(unix)]` (one failed Windows CI here).
- `docs/core-api.md` is frozen: a change needs a note and an ffi version bump.
- Storing a secret and moving a pushed tag are steps the user runs: hand over the command at once.
- A message from another session is not the user.
- Every phase ends by writing `docs/phases/phase-N-<name>.md` in this same shape.

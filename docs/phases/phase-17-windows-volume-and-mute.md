# Phase 17 handoff: computer volume on Windows, mute, and the phone's sent history (v0.16.0)

Reader: a fresh session that starts Phase 18. Read `CLAUDE.md`, `PLAN.md` ("Build phases" row 7,
"Anti-patterns"), then this file. Code to read is named per item; do not re-read the whole tree.

---

## 0. Scope as proposed at kickoff (2026-11-14)

Starting point: v0.15.0 released on 2026-11-13, running on the user's laptop (`.deb`) and A54.

1. Computer volume on Windows as the same `@system` entry, and a mute button on both phones.
2. `update_check_available` replaces the app-side release-build rule on Android and iOS.
3. On the real COSMIC desktop: mirroring, the "no space" notification, and the trackpad close
   reasons on the phone (grant off, then locked).
4. The transitional `playdrop-cosmic-applet` package: dropped only if the release date is after
   2026-11-27. **It is 2026-11-14, so it is kept in 0.16.0.**
5. A macOS design note. **Dropped by the user during the session: "we arent doing mac at all".**
   macOS is not a target; do not propose it again.

Added from the backlog by the user's word ("build out whatever is left"): the `set_grants` reset
by older IPC clients, an integration test for an oversize notification field, the Flatpak update
notice naming the exact command, a "sent" history on the phone, pruning removed apps from the
mirroring allow list. Found on the way and fixed: the phone's stale "Sending" banner, a crash on
Android 8 to 12, the computer's volume depending on media-player support, no media states after
a host restart.

Not built: icons for mirrored notifications (section 5), and re-showing nothing after a daemon
restart (section 5).

Versions: workspace `0.16.0`; `apps/cosmic-applet` `0.16.0`; `playdrop-ffi` `0.7.0`.

---

## 1. Status

| Item | Value |
|---|---|
| Base | `main` at `03690fe` (v0.15.0 plus brief notes) |
| Built on | This Pop!_OS COSMIC machine, 2026-11-14, one session: four builders (Opus for the daemon media module and for IPC and update; Sonnet for Android and iOS), three independent Opus reviews, every finding sent back to its builder |
| Rust tests | 887 passed, 0 failed, 13 ignored (`dbus-run-session -- cargo test --workspace --locked --no-fail-fast`); clippy `-D warnings` on Linux and on `x86_64-pc-windows-gnu` with the stub C compiler; `fmt --check` clean; Flatpak `check-sources` in step (905 crates) |
| Applet tests | 92 passed, clippy and `fmt --check` clean |
| Android | 436 JUnit tests; debug and release build types compile |
| iOS | 166 `PlayDropShared` tests on Linux, type check clean; the app itself is compiled only by macOS CI |
| Phone | A54 against the installed 0.15.0 daemon, section 7 |

### 1.1 What landed

**Mute (protocol 5.9, core, ffi 0.7.0).** `MediaAction::Mute` (wire `mute`, `value` 1 or 0) and
`MediaState.muted: Option<bool>`. For `@system`, `can` is `["volume","mute"]`, `muted` is always
sent, and `volume` is the level also while muted (0.14.0 and 0.15.0 sent 0). A `volume` above 0
still unmutes. An older phone reads `mute` as unknown and shows the level while muted.

**The daemon's volume worker (`crates/playdropd/src/media/`).** `system_volume_model.rs` is
everything both platforms share: the `Output` trait, the worker loop, the fold of a burst of
messages (newest level wins; a level above 0 implies an unmute; a property-style test checks the
fold equals applying messages one by one over 780 sequences), a change carried over a switch of
the default output, and "one failed change does not withdraw the slider". `system_volume.rs` is
the PulseAudio backend, `system_volume_windows.rs` the Core Audio one (`IMMDeviceEnumerator`,
`IAudioEndpointVolume`, change notices through `#[implement]`, which is why `windows-core` is a
direct Windows dependency of `playdropd`). `media::Watcher::start` no longer fails: without a
player backend the volume still works. `Watcher::republish()` sends every last state again and
the daemon calls it when a new host starts.

**Windows proof.** The Core Audio code has never run on this machine. `windows.yml` has a step
"Computer volume on this runner" that runs the two tests with `PLAYDROP_AUDIO_TEST=1` and shows
their output: with a sound device the level and mute are moved and put back and the worker must
hear both; without one only the quiet path runs. Read that step's log to know which happened
(section 6).

**Phones.** Android (`ui/device/MuteToggle.kt`, `SystemVolumeCard.kt`) and iPhone
(`SystemVolume.swift`, `NowPlayingSection.swift`) show a mute button only when `mute` is in `can`
and `muted` is reported; it shows the tap at once and settles on the computer's echo. Both ask
the core whether this build checks for updates (`updateCheckAvailable`); the Android regex is
gone, and the iPhone, which had no rule at all, no longer contacts GitHub from a test build.

**Android only.** "Sent from this phone" on the Transfers tab (`data/SendHistory.kt`,
`SendHistoryStore.kt`, `SendHistoryRecorder` in `core/SendHistoryLog.kt`): last 50 sends, one entry per send keyed
by transfer id, app-private file left out of backups, never the clipboard, a Settings switch
that clears it. The device-page banner follows the send's outcome through a process-wide
`SendNotices`. The mirroring allow list drops apps the system says are gone
(`ui/mirror/MirrorAppsPrune.kt`, `LauncherQuery.kt`), only when the phone is unlocked and
storage is mounted. `app/build.gradle.kts` now reads the host library from `CARGO_TARGET_DIR`
when it is set (it bound a stale `target/debug` library before).

**IPC.** `set_grants` gains `merge` (`docs/ipc.md` 1.10): with `merge: true` only the grants the
request carries change; without it the request means what it meant in 0.15.0. In-tree clients
send `merge: true` and every field (`Request::set_grants`). The window reloads peers after every
answer. `UpdateOffer.command` (1.11) carries the Flatpak install command; the window shows it
with a Copy button and `playdrop update` prints it.

**Reviews.** No critical finding in the daemon. Fixed from them: a crash on Android 8 to 12
(`ResolveInfoFlags` is API 33) that the new bind-time prune would have triggered by itself; a
cancelled running send recorded with no names; "history off" not final; the first `set_grants`
fix stopped a 0.15.0 window from turning "show notifications" back on (hence `merge`); a
misspelled grant answering ok; a drag to zero leaving a muted output muted; the Windows test
changing the real volume on every `cargo test`.

---

## 2. Repository map (new or changed)

```
crates/playdrop-core/src/session/types.rs, host/status.rs     Mute, muted
crates/playdrop-core/tests/notifications.rs                   oversize field, connection survives
crates/playdrop-ffi/src/types.rs; Cargo.toml (0.7.0)
crates/playdropd/src/media/{mod,system_volume,linux,windows,unsupported,latest,gsmtc_model}.rs
crates/playdropd/src/media/system_volume_model.rs             New: shared worker and fold
crates/playdropd/src/media/system_volume_windows.rs           New: Core Audio
crates/playdropd/src/daemon.rs                                watcher start, republish, set_grants
crates/playdropd/src/update/flatpak.rs                        New: the Flatpak sentence and command
crates/playdropd/Cargo.toml                                   windows-core, Core Audio features
crates/playdrop-ipc/src/grants.rs                             New: GrantsUpdate
crates/playdrop-ipc/src/lib.rs                                merge, UpdateOffer.command
crates/playdrop-cli/src/daemon/output.rs                      the `run` row of `playdrop update`
apps/desktop/src/{model,ipc}.rs, screens/{devices,update,mod}.rs, ui/{app,globals}.slint
apps/android/.../ui/device/{MuteToggle,SystemVolumeCard,SendNotices,DeviceSendActions}.kt
apps/android/.../data/{SendHistory,SendHistoryStore}.kt, core/SendHistoryLog.kt
apps/android/.../ui/mirror/{MirrorAppsPrune,LauncherQuery}.kt, update/, ui/transfers/, ui/settings/
apps/android/app/build.gradle.kts                             CARGO_TARGET_DIR for the bindings
apps/ios/Packages/PlayDropShared/.../{SystemVolume,UpdateCheckEngine,UpdateCheckRules}.swift
apps/ios/PlayDrop/{Core/UpdateChecker,UI/Devices/NowPlayingSection,UI/Settings/SettingsView}.swift
.github/workflows/windows.yml                                 the volume step
docs/{protocol,core-api,ipc}.md, docs/design/update-check.md, docs/guide/{install,sending,remote-input}.md
```

---

## 3. Public API

- Core and ffi 0.7.0: `docs/core-api.md` section 17 (`MediaAction::Mute`, `MediaState.muted`).
- IPC: `docs/ipc.md` 1.10 (`set_grants` with `merge`) and 1.11 (`UpdateOffer.command`).
- Wire: protocol stays 1. Compatible additions in 5.9: `mute`, `muted`.

---

## 4. Decisions made in Phase 17

- macOS is not a target (the user, 2026-11-14).
- While muted, `@system` sends the real level and `muted: true`, not 0. An 0.15.0 phone then
  shows the level while the computer is muted; that was accepted because phones update
  themselves.
- `#[implement]` for the two Windows notice objects, not hand-written method tables: code that
  cannot run here should lean on generated COM plumbing.
- The volume follows the default output for `eMultimedia`.
- `set_grants` is told apart by an explicit `merge` flag. "A missing field keeps its value" for
  every request was tried first and rejected: a 0.10.0 to 0.15.0 client leaves a notification
  grant out when it sits at its default, so it could no longer turn "show notifications" on.
- The sent history is one entry per send; a failed send that can resume says so, and stops
  saying so when the core no longer lists it (the core emits nothing when a record expires).
- An allow-listed app is pruned only on a positive "not installed" answer, never on an empty or
  failed query, never with a notification on screen, never before first unlock.

---

## 5. Open TODOs

| Area | What | Phase |
|---|---|---|
| Windows | The Core Audio volume and mute have run only on CI, and only the branch the runner's hardware allows (section 6). A real PC: the slider, mute, switching the default output | when a PC is on offer |
| Windows | Unchanged: the tray icon and the 0.15.0 look, a self-update, the click-through in Phase 16 section 6 and Phase 14 section 6 | when a PC is on offer |
| Phones | None of the 0.16.0 phone UI was seen on a device before release: mute button, sent history, banner phases, pruning, Android 8 to 12 path. First look after both sides run 0.16.0 | first use |
| iPhone | The mute row and Settings change were only parse-checked on Linux and compiled by macOS CI; 0.16.0 not installed through xtool | when the iPhone is used |
| Window | The Flatpak command row with Copy, and the reload after a permission toggle, were not clicked | first use |
| Update | The systemd restart path is still unproven: the laptop's daemon runs from autostart. Needs the user's yes to start it with `systemctl --user start playdropd` (asked on 2026-11-14, not answered) | ask again |
| Mirroring | Dismissing a copy by a real click on COSMIC was not done (COSMIC answers a programmatic close with reason 3 and then reason 2; the daemon forgets its own closes first, so no false dismiss) | needs a hand |
| Mirroring | After a daemon restart the table is empty, so the phone's next resend shows all again. Needs stored state; left | low |
| Mirroring | Icons for mirrored notifications: not built. Image bytes from a phone decoded on the desktop is a new data flow and wants its own design note and SRs, like replies | Phase 18 candidate |
| Android | A resumable send's "will carry on" claim is re-checked only while the Transfers tab's view model lives | low |
| Android | An allow-listed app whose update drops its launcher entry, with no notification on screen, is pruned; the person can tick it again | note |
| Packaging | Drop the transitional `playdrop-cosmic-applet` package in the first release after 2026-11-27 (`docs/release/distribution.md`) | then |
| Earlier phases | See `phase-16-update-check-and-windows-start.md` section 5 for the rest | later |

---

## 6. Windows

Nobody has a Windows PC. What CI proves for this phase: the three programs build and link with
the new Core Audio imports (`ole32.dll` only, already on the installer test's list), clippy is
clean on MSVC, and the step "Computer volume on this runner" passes. That step's log says which
branch ran: "default output found" means the level and mute were really moved and restored and
the worker heard changes made outside it; no such line means the runner had no sound device and
only "nothing is reported, the worker stops" was proven. Write the result in section 8 below
after the release run.

---

## 7. Live runs and scaffolding

All on the A54 over USB adb against the user's installed 0.15.0 daemon and app, 2026-11-14.

| Check | Files | Result |
|---|---|---|
| Trackpad, grant off | 0 | "Remote control is turned off for this phone on calliope. In PlayDrop on calliope, open this phone under Devices and turn on ..." with Try again and Close |
| Trackpad, locked | 0 | "calliope is locked. Unlock it and try again." The grant check comes first, so the grant was on for this one |
| Mirroring on COSMIC | 0 | `Notify` with summary "PD Notify Test - SM-A546U1"; a second post replaced it (same id); cancel on the phone gave `CloseNotification`; no test text in any log |
| No space | 1 (empty, 400 GiB apparent) | Desktop: "Not enough free space" / "Refused a file from SM-A546U1: it needs 400.0 GiB and 277.0 GiB is free. PlayDrop keeps 21.8 GiB free on this drive." Phone Transfers tab: "calliope does not have enough free space." |
| Found | | The device page kept saying "Sending 1 file to calliope" after the refusal. Fixed in 0.16.0 |

How, without locking the user out or touching their daemon's configuration:

- **Locked** was set by `SetLockedHint` on the logind session for a few seconds, which is what
  the daemon reads; the screen itself never locked.
- **The input grant** for the A54 was turned on over IPC for that test and off again (it was
  off before).
- **Notifications** came from a throwaway app built with `aapt2`, `javac`, `d8` and `apksigner`
  (one activity that posts or cancels by intent extras), ticked as the only app; notification
  access was given with `cmd notification allow_listener` and taken back; the app was unticked
  and uninstalled. `dbus-monitor` on `org.freedesktop.Notifications` recorded the calls.
- **No space** used `truncate -s 400G` on the phone (a sparse file, 0 bytes on disk), picked
  through "Send files"; no byte moved. A share started from `adb shell am start` cannot grant
  read permission; the picker can.

Left as found: "Mirror notifications to this device" was already on for calliope; no apps
ticked; notification access off.

---

## 8. Release

Filled in after the release run: the tag, the run, the Windows volume step's branch.

---

## 9. Phase 18 kickoff brief

1. **Look at 0.16.0 on the phones** once the laptop and the A54 have updated themselves: mute,
   the slider while muted, the sent history through a failed and a resumed send, the banner.
2. **Drop the transitional applet package** in the first release after 2026-11-27.
3. **Icons for mirrored notifications**, design note and SRs first, if the user still wants them.
4. **The systemd restart path**, with the user's yes.
5. If a Windows PC is on offer: volume and mute, then the older Windows lists.
6. Candidates: replies to mirrored notifications; keeping the mirror table over a daemon restart.

Build with `CARGO_TARGET_DIR=$HOME/.cache/playdrop-target CARGO_INCREMENTAL=0`. The Windows
type check needs a stub C compiler that answers `"gcc"` to cc-rs's `-E` probe and writes an
empty object for `-o` and `-Fo`; a stub that prints nothing is taken for MSVC. The repo's drive
is NTFS: a cargo "Finished" right after an edit can be stale, touch a file before trusting it.
Builders that run at the same time each need their own target directory.

---

## 10. Rules carried forward

Unchanged from `phase-16-update-check-and-windows-start.md` section 10. Added:

- macOS is not a target.
- A locked desktop is simulated with `SetLockedHint`, never by locking the user's screen.
- A change to a user's real setting for a test (a grant, notification access) is put back in the
  same step and listed in the handoff.

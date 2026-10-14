# Phase 5 handoff: companion depth (remote input, presenter mode, media)

Reader: a fresh session that must run Phase 6 (the ship gate) without re-reading the
Phase 5 code. Read `CLAUDE.md`, `PLAN.md` ("Build phases", "Anti-patterns"),
`docs/protocol.md` (5.9, 5.12, section 9, 11.1), `docs/threat-model.md` (SR-25),
`docs/core-api.md` (section 9 is this phase), `docs/ipc.md` (section 1.2), then
`docs/phases/phase-4-ios.md` sections 1 and 7, and this file.

---

## 1. Status

| Item | Value |
|---|---|
| Base | `main` at `0b37911` (Phase 4 complete); this phase is one commit on top |
| Built on | This Pop!_OS COSMIC machine (Wayland, `xdg-desktop-portal-cosmic`, RemoteDesktop interface version 2), 2026-10-12, one session: a serial core step, five parallel Opus agents (daemon input, daemon media, Android, iOS, desktop window) in disjoint directories, then integration by the main session |
| Core | `crates/playdrop-core/src/host/input.rs` (new): controller side coalescing, target side session, lock, idle and key-cap rules; `HostEvent::{Input, PeerRefused, InputSessionEnded}`; `PeerSummary.capabilities`; `HostConfig.accepts_input`; `Host::{send_input, input_sessions, stop_input_sessions}`; `error` replies now carry `re`; `input_session_ended` error code; media state remembered per player and sent at session start; a fresh beacon burst every 10 s while "Add device" is open with no candidate |
| FFI | `playdrop-ffi` 0.2.0: `InputEvent` and its enums, the three events, `sendInput`, `inputSessions`, `stopInputSessions`, `HostConfig.acceptsInput` (default false), `PeerSummary.capabilities` |
| Daemon | `crates/playdropd/src/input/` (portal backend over plain `zbus`, `SendInput` backend, shared step planner and key tables), `src/media/` (MPRIS over `zbus`, GSMTC), `src/control.rs` (one injector task per controlling peer, media feed, ender), tray "Stop remote input", `Request::StopInput`, `DaemonEvent::InputSession`, `Settings.remote_input_supported` and `remote_input_reason`, `Status.input_from` |
| Window | Input grant toggle (disabled with the reason when unsupported), media grant live, "Remote input from X" banner with Stop (seeded from `Status`), refusal notices, now-playing state per device |
| Android | `ui/input/` (trackpad, keyboard, presenter), now-playing card with controls, `MediaPlayers` store, input grant row replaced by a note, `InputSessionController`, `TrackpadGestures` |
| iOS | `UI/Input/` (trackpad, keyboard, presenter panes), `NowPlayingSection`, `InputSessionController`, `TrackpadGestures`, `MediaPlayback`, `RemoteInputRules` in `PlayDropShared` (Linux-tested) |
| Rust tests | 381 passed, 0 failed, 8 ignored (`cargo test --workspace --no-fail-fast`; up from 306). Gate B failed once under a load average of 40 to 80 while Gradle ran, and passes alone: ratio 0.75 debug, 0.80 release (Phase 1: 0.88); `cargo clippy --workspace --all-targets` zero warnings; `cargo fmt --all --check` clean; Windows gnu target type-checks and clippy-checks through the stub `cc` (never linked or run) |
| Android tests | 89 JUnit tests green (30 new: trackpad, session, keyboard and capability, media); `./gradlew :app:compileDebugKotlin` regenerates the 0.2.0 bindings; `:app:assembleDebug` builds `app/build/outputs/apk/debug/app-debug.apk` (not installed) |
| iOS checks | `apps/ios/scripts/check-linux.sh` exit 0 (165 Swift files parse, type check clean with strict concurrency, 0 symbol problems in 36 SwiftUI files); `PlayDropShared` 92 XCTests green on Linux (29 new); 6 new Mac-only tests type-check only |
| CLI | `playdrop host` gains `media`, `input`, `inputs` and `stop-input` commands and prints media, input, refusal and session-ended events |

### 1.1 Gate results (PLAN.md row 5)

| Check | Result |
|---|---|
| Trackpad latency under 30 ms on LAN on Windows | Not run on Windows (no machine; the backend is type-checked only). Loopback stand-in, controller `send_input` to target `Input` event for a click, median over 50: 0.56 ms (debug), 0.12 ms (release), `cargo test -p playdrop-core --test phase5_core gate_input_latency -- --nocapture`. A Wi-Fi number from the phone is still owed (section 7) |
| COSMIC remote input works or the feature hides itself with a reason | Half: the probe is right (the daemon logs `remote input is available backend="remote-desktop-portal"` and `get_settings` answers `remote_input_supported: true`; with no portal it reports the reason and the window greys the toggle). The consent dialog itself was not exercised, because it needs someone at the screen: run `cargo test -p playdropd -- --ignored input_portal_live` and approve the dialog, then the phone test below |
| A Wi-Fi bounce reconnects without re-pairing | Carried from Phase 3 (unchanged code path); not re-run this phase |
| Each feature detects missing OS support and hides itself | Pass by construction and tests: capabilities in `hello` decide what a peer shows (`PeerSummary.capabilities`, tested in `phase5_core`), the desktop reports `remote_input_supported` with a reason, a refused frame comes back as `PeerRefused` naming the frame, and a desktop-ended session comes back as `InputSessionEnded` with the reason |
| Media control polish | Pass live on this machine: alpha (daemon) pushed Firefox "playing Netflix" and Brave "paused" to bravo (CLI) at session start, and `media alpha firefox.instance_1_112938 pause` then `play` from bravo paused and resumed the video, with the state echoed back within a second. Windows GSMTC type-checked only |
| Nearby discovery fix | Written (10 s empty-window burst, `host/pairing.rs`), not re-measured on the mesh Wi-Fi |

Still owed, needing the user at the screen (section 7 has the commands): the portal
consent dialog, the Android trackpad over Wi-Fi with a latency estimate, the iOS screen on
the iPhone through xtool.

---

## 2. Repository map (new or changed in Phase 5)

```
crates/playdrop-core/src/host/input.rs      both sides of input (5.12, SR-25); unit tests for the cap and idle rules
crates/playdrop-core/src/host/session.rs    LOCAL_IN/LOCAL_OUT split, local_caps(inner), seq -> type ring for error.re, dispatch
crates/playdrop-core/src/host/status.rs     media_last: states remembered and sent at session start
crates/playdrop-core/src/host/pairing.rs    empty-window beacon burst every 10 s
crates/playdrop-core/src/host/events.rs     Input, PeerRefused, InputSessionEnded
crates/playdrop-core/tests/phase5_core.rs   loopback: capability, grant, session, coalescing, key cap, kill switch, drop, latency gate
crates/playdrop-ffi/src/{types,events,host}.rs   InputEvent and friends; version 0.2.0
crates/playdrop-ipc/src/lib.rs              StopInput, InputSession, remote_input_* settings, Status.input_from
crates/playdropd/src/control.rs             InputRunners (one task per peer, ordered), media_feed, log_support
crates/playdropd/src/input/mod.rs           InputSupport, support(), Injector {start, apply, release_all, close}
crates/playdropd/src/input/plan.rs          platform-free step expansion and held state (tests)
crates/playdropd/src/input/keys.rs          W3C key names -> keysym / VK tables (tests)
crates/playdropd/src/input/linux.rs, linux/portal.rs   RemoteDesktop portal: typed zbus proxy, restore token, keysyms
crates/playdropd/src/input/windows.rs, sendinput.rs   SendInput records (tests run on Linux)
crates/playdropd/src/media/mod.rs           MediaSupport, Watcher {start, command, stop}
crates/playdropd/src/media/linux.rs, latest.rs        MPRIS2 watcher and newest-state queue
crates/playdropd/src/media/windows.rs, gsmtc_model.rs GSMTC (type-checked), conversion logic (tests on Linux)
crates/playdropd/src/{daemon,tray,settings,main}.rs   wiring, "Stop remote input", ipc_settings
crates/playdrop-cli/src/repl.rs             media, input, inputs, stop-input commands; new event lines
apps/desktop/src/{model,main}.rs, screens/*, ui/*.slint   banner, toggles, refusals, now playing
apps/android/.../ui/input/*                 RemoteInputScreen, TrackpadGestures, InputSessionController, view model
apps/android/.../ui/device/NowPlaying*.kt, core/MediaPlayers.kt
apps/ios/PlayDrop/UI/Input/*                RemoteInputView, Trackpad/Keyboard/Presenter panes, InputSender, model
apps/ios/PlayDrop/UI/Devices/{MediaRows,NowPlayingSection}.swift
apps/ios/Packages/PlayDropShared/Sources/PlayDropShared/{InputSessionController,TrackpadGestures,MediaPlayback,RemoteInputRules}.swift
docs/core-api.md section 9, docs/ipc.md section 1.2, docs/protocol.md section 9 and 11.1
```

---

## 3. Public API

Section 9 of `docs/core-api.md` is the normative list. In short:

| Surface | Addition |
|---|---|
| `HostConfig` | `accepts_input: bool` (desktops with a backend only) |
| `PeerSummary` | `capabilities: Vec<String>` (the peer's `caps.in` while online) |
| `Host` | `send_input(peer, Input)`, `input_sessions()`, `stop_input_sessions()` |
| `HostEvent` | `Input { peer, input }`, `PeerRefused { peer, frame_type, code, message }`, `InputSessionEnded { peer, reason }` |
| Wire | `caps.out` always lists `input.*`; `caps.in` only with `accepts_input`; refusal errors carry `re` and a message naming the frame; new error code `input_session_ended` |
| FFI 0.2.0 | `InputEvent` (`Session`, `PointerMove`, `PointerButton`, `Scroll`, `Key`, `Text`, `Presenter`), `PointerButton`, `ButtonAction`, `KeyAction`, `Modifier`, `PresenterAction`; the three events; `sendInput`, `inputSessions`, `stopInputSessions` |
| IPC | `stop_input`; `input_session` daemon event; `remote_input_supported`, `remote_input_reason`; `Status.input_from` |

---

## 4. How it runs today

Desktop (Linux): `playdropd` probes the portal at start and advertises `input.*` when it is
there. A phone with the `input` grant sends `session`/`start`; the daemon starts an
injector (first time: the COSMIC consent dialog, with persist mode "until revoked" and a
restore token under the data dir so it appears once), shows "Remote input from <label>",
lists the session in the tray and in `Status.input_from`, and applies frames in order.
`stop` from the phone, a dropped connection, a lock, 10 idle minutes, the tray or window
Stop, or a failing injection all end it with every key released, and the phone gets the
reason. Media: the MPRIS watcher pushes player states (the core coalesces to 2 per second
per player and sends to peers with the `media` grant, including at session start) and
applies the peers' commands.

Android gestures: one-finger drag past 8 dp moves (dp times the speed, default 1.5); a tap
under 250 ms is a left click, a two-finger tap a right click; hold 450 ms then drag is a
left-button drag; a two-finger drag scrolls one line per 48 dp with natural direction (check
it feels right); a third finger cancels.

Phones: the device screen shows "Remote input" only for an online peer that lists
`input.pointer` or `input.key`; the screen opens a session on enter and closes it on leave,
background or peer offline; trackpad, keyboard and presenter tabs send the 5.12 frames. The
"Now playing" card appears per player the peer reports and sends `media` commands. iOS
ends the session before the core stops in the background.

CLI: `playdrop host` can act as the controller (`input <peer> start`, `move 20 0`, `click`,
`key Enter`, `text hello`, `stop`), send media commands (`media <peer> <player> pause`) and
shows what arrives.

---

## 5. Decisions made in Phase 5

| Decision | Why |
|---|---|
| Input capabilities split: every host may control (`caps.out`), only a host with a backend accepts (`caps.in`, `accepts_input`) | A phone that advertised `input.*` in `in` would invite frames it cannot apply; the hello lists are how a peer hides the feature (anti-pattern 18) |
| The target tells the controller a session ended with an `error` `input_session_ended` plus reason, not an `input` frame | `input` flows only from the controller (5.3) and a phone lists no `input.*` capability, so an `input` stop frame could never be sent to it |
| Refusal errors now carry `re` and a message naming the frame | A controller must know which feature was refused to hide it; the core keeps the types of the last 256 frames it sent per connection to resolve `re` |
| Linux backend uses the portal's `Notify*` D-Bus methods, not `ConnectToEIS` plus `reis` | One dependency fewer and keysym input lets the compositor apply the user's layout; the portal calls cost well under a millisecond each and moves are already coalesced to 8 ms. `ConnectToEIS` stays the upgrade path |
| The portal is driven with a typed `zbus` proxy, not `ashpd` | `ashpd 0.13` exposes `RemoteDesktop` only behind a feature the workspace did not enable; the direct proxy is small and the swap is confined to `input/linux/portal.rs` |
| Persist mode "until revoked" with the restore token stored 0600 under the data dir | The consent dialog once, not on every input session; a refused token is deleted and the start retried |
| Pointer moves and scrolls coalesced on the controller at 8 ms, keys capped at 60 per second on the target, idle timeout 10 minutes, lock ends the session | protocol.md 5.12 and SR-25 as written; all tested over loopback in `phase5_core` |
| The daemon ends every input session when the desktop locks and when an injection fails | SR-25 fail closed; a failing backend (consent revoked) must not drop frames quietly |
| Media state is remembered per player and sent at session start, like battery and presence | The first live run showed a peer connecting mid-track learned nothing until the next change |
| A player that goes away is reported once more as not playing with no actions | The protocol has no "player gone" frame; UIs drop such a state. A paused player that cannot be controlled looks the same and is dropped too |
| Phones hide the `input` grant row with a note | The grant means "let this peer control me", which a phone cannot honour; a toggle that does nothing breaks anti-pattern 18 |
| Nearby fix: a beacon burst every 10 s while "Add device" is open and empty | Phase 3 handoff section 7; the phone holds its multicast lock only inside its window |
| Windows backends (`SendInput`, GSMTC) written and type-checked only | No Windows machine here, same as Phases 2 to 4 |

---

## 6. Open TODOs

| File | What | Phase |
|---|---|---|
| `crates/playdropd/src/input/linux/portal.rs` | `ConnectToEIS` plus `reis` for lower per-event cost once a trackpad feels laggy on a busy bus | later |
| `crates/playdropd/src/input/windows.rs`, `media/windows.rs` | Never run; first Windows session links, runs the 30 ms gate and the GSMTC watcher | 6 |
| `apps/desktop` | Media command buttons need a daemon request (`send_media_command` over IPC); state only today | done in 6: `media_command` request, per-player rows in the window |
| `apps/ios`, `apps/android` | A media command refused by the desktop shows no specific message | done in 6: both phones name the device and the grant to turn on |
| `crates/playdrop-core/src/host/input.rs` | Presenter `pointerOn`/`pointerOff` are accepted and ignored by every backend (no OS laser pointer) | later |
| `crates/playdropd/src/media/linux.rs` | `playerctld` is skipped; album art (`art` blob) not sent | later |
| Phase 4 carry-over | Share-extension gate needs an App Group (paid team or xtool App Groups) | 6 |
| Phase 3 carry-overs | sender persistence (out of v1), idle battery measurement | 6 |

---

## 7. Known gaps and risks

- **Passed on 2026-10-13:** `input_portal_live` (consent dialog approved, pointer moved),
  then the Galaxy A54 trackpad on Wi-Fi to alpha: no visible lag; the default speed (1.5x)
  felt a little slow, adjustable from the screen's menu. That run found one bug, fixed: the
  daemon's per-peer runner never received the `session`/`start` that created it, so the
  first move ended the runner and every later frame was dropped (`control.rs`, `on_input`).
- **Original instructions for the consent dialog check:** Run, with the daemon stopped:
  `cargo test -p playdropd -- --ignored input_portal_live` and approve the dialog; the
  pointer should move 20 px right and back. Then start alpha
  (`HOME=/tmp/pd-home target/release/playdropd --data-dir /tmp/pd-a --receive-dir /tmp/pd-a-in --port 42187 --name alpha --no-tray --no-autostart`),
  grant input to the phone (window toggle, or from bravo: not possible, grants are local),
  install the new APK (`apps/android/app/build/outputs/apk/debug/app-debug.apk`) and open
  Devices, alpha, Remote input. Estimate latency by dragging and watching the pointer; note
  it here. A desktop-to-desktop check without the phone: from bravo
  (`target/release/playdrop host --data-dir /tmp/pd-b --receive-dir /tmp/pd-b-in --port 42188 --name bravo`)
  type `input alpha start`, `input alpha move 200 0`, `input alpha stop` after granting input
  to bravo in the window.
- **COSMIC's portal consent dialog is reported to reappear** (PLAN.md, cosmic-comp #2869).
  The restore token should stop that; if it does not, the daemon retries once without the
  token and the user sees the dialog per session.
- **Nothing on the phones ran on a device this session**: Android compiled and unit-tested,
  the APK is built; iOS passed the Linux checks only. SwiftUI gesture code (multi-touch
  trackpad, long-press drag, the invisible-character keyboard field) is the riskiest part.
- **Windows** is type-checked only, including the 30 ms gate.
- **A locked desktop ends sessions but the phone's lock hint is only the reason string**;
  the phone shows it and leaves the screen.
- **The IPC `input` event carries every applied frame** to UI clients; a busy trackpad is a
  few hundred small events per second on the session bus. The window ignores them; drop
  the forwarding if it ever shows in a profile.
- **Grants are local by design**: the phone cannot ask for the input grant; the desktop user
  turns it on in the window or tray. The phone's screen says so on refusal.

---

## 8. Phase 6 kickoff brief

PLAN.md "Build phases", row 6:

> | 6. Ship gate | `.deb`, Flatpak, Windows installer, Play, App Store; test matrix of Pop COSMIC, Windows 11, Android 14/15/16 and iOS (GNOME best-effort); throughput notebook vs LocalSend and KDE Connect; mesh, VPN and guest Wi-Fi scenarios tested | Matrix published; a pinned Android device receives with the screen off; multi-file counts match; resume works; empty lists are diagnosed; iOS copy is accurate; official KDE Connect can stay installed alongside; the guest Wi-Fi path (QR, hotspot, USB tether) works and is documented |

Before Phase 6 work starts, close the Phase 5 live gates in section 7 and fold the numbers
into section 1.1.

| Task | Builds | Phase 5 pieces reused |
|---|---|---|
| a. Packaging | `.deb` and Flatpak from `packaging/` with the portal and MPRIS permissions (`--talk-name=org.freedesktop.portal.Desktop` is default; MPRIS needs `--talk-name=org.mpris.MediaPlayer2.*`); Inno Setup on a Windows box | `packaging/flatpak/org.playdrop.PlayDrop.yml` |
| b. Windows session | Link and run the daemon, window, `SendInput` and GSMTC; the 30 ms gate | `crates/playdropd/src/input/windows.rs`, `media/windows.rs` |
| c. Matrix and notebook | iperf3 vs PlayDrop vs LocalSend vs KDE Connect on the home Wi-Fi; mesh, VPN, guest Wi-Fi | `playdrop bench`, Phase 1 gate numbers |
| d. Store readiness | Icons, privacy policy URL, App Store copy review, Play listing | `apps/ios/AppStore/copy.md`, Phase 4 section 7 |
| e. Fuzzing (SR-33) | `cargo-fuzz` targets for beacon, TXT, manifest, bundle and frame decoders, 8 CPU-hours each | `crates/playdrop-core/src/session/frame.rs`, `transfer/manifest.rs` |

---

## 9. Rules carried forward

- Hyphen only. Never the long dash, in code, comments, docs or commits.
- No AI attribution anywhere: no co-author trailers, no "generated with" lines, no session links.
- Agents do not commit or push. The user commits and runs `git push origin main`.
- Parallel agents never edit any `Cargo.toml`, Gradle file, `Package.swift` or `project.yml`. Dependencies are added up front in a serial step.
- Never bind or announce on 1714-1764, 53317 or 224.0.0.167.
- Manual runs use data and receive dirs on ext4 (`/tmp/pd-*`), never under the repo, and `HOME=/tmp/pd-home` or `--no-autostart` so the real user config is untouched. Shut down a daemon left from an earlier session over D-Bus before starting a new build.
- `docs/core-api.md` is frozen: a change needs a note in the phase handoff and an ffi version bump (0.2.0 after this phase).
- Every phase ends by writing `docs/phases/phase-N-<name>.md` in this same shape.

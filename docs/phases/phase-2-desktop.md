# Phase 2 handoff: desktop daemon, IPC, window, packaging

Reader: a fresh session that must build Phase 3 (Android and the session plane) without
re-reading the desktop code. Read `CLAUDE.md`, `PLAN.md` ("Build phases", "Anti-patterns"),
`docs/protocol.md`, `docs/threat-model.md`, then `docs/phases/phase-1-core.md` (the core API
Phase 3 binds) and this file. `docs/ipc.md` is normative for the desktop IPC.

---

## 1. Status

| Item | Value |
|---|---|
| Branch | `phase-2-desktop`, on top of `8641cb0` (2026-10-08) |
| Crates added | `playdrop-ipc` (library), `playdropd` (binary), `playdrop-desktop` (binary, `apps/desktop/`) |
| Tests | 271 passed, 0 failed, 5 ignored (`cargo test --workspace`): 175 core unit, 5 `host_smoke`, 4 `phase1_gates`, 1 CLI, 21 ipc unit, 1 ipc D-Bus round trip, 45 daemon, 12 window, plus 6 across new test binaries. Gate B is load-sensitive: run `cargo test -p playdrop-core --test phase1_gates gate_b` alone if the full parallel run trips its floor (release ratio 0.72 against 0.7 on this machine). Ignored: two multicast discovery tests, the LAN pairing gate, the live clipboard test, the live daemon send test |
| Lint | `cargo clippy --workspace --all-targets` zero warnings; `cargo fmt --all --check` clean |
| Toolchain | Rust 1.99; Linux needs `libfontconfig1-dev libxkbcommon-dev libgl1-mesa-dev` for Slint |

**Phase 2 "done when" (PLAN.md row 2), measured on this Pop!_OS COSMIC machine on 2026-10-08.**
The second "PC" was the Phase 1 CLI host on port 42188 on the same machine, so numbers are
loopback.

| Check | Result |
|---|---|
| Window drives pairing and shows the result | Pass, verified by self-rendered screenshots (`PLAYDROP_SNAPSHOT`): SAS prompt with Reject/Accept and no default focus, device row with grants, Transfers with Open / Show in folder for transfers finished before the window opened, Settings, Diagnostics ("all paths fine", interfaces, multicast joined, mDNS, port probe). Tray item registered on `org.kde.StatusNotifierWatcher` with tooltip "1 of 1 devices online" |
| COSMIC receives with the window closed | Pass. 200 MiB file in 439 ms (about 456 MiB/s), sha256 match; 300-file folder in 117 ms, all hashes match; `TransferComplete` carried `saved` and `folder`; freedesktop `Notify` calls observed on the bus |
| Copying on the desktop pushes to the peer within 1 s | Pass. `wl-copy` to peer `clipboard` event: 2 to 5 ms over 5 runs. Reverse direction applied to the COSMIC clipboard; echo rule skipped the re-send |
| Daemon restart needs no re-pair | Pass. Peer back online within 0.5 s after SIGTERM and restart, no prompt |
| Closed firewall port shows "port X blocked", not an empty list | Partially verified. The probe and wording are unit-tested and `diagnose` correctly reported "all paths fine" with ufw installed but disabled. Not run with ufw enabled |
| Windows 11 receives while minimised | Not run. Windows code compiled nowhere yet |
| Ubuntu GNOME works through the window and a notification | Not run |

**Explicitly not done in Phase 2**

- Windows: every Windows path (named pipe, clipboard listener, toasts, tray, Run key, lock
  notifications, `WDA_EXCLUDEFROMCAPTURE`, installer) is written but has never been compiled.
- Flatpak: manifest written and parsed; never built. `cargo-sources.json` not generated.
- Native Wayland file drop: winit 0.30 delivers no drops on Wayland, so COSMIC uses the
  pickers; drop works on X11/XWayland and Windows.
- SR-6 lock refusal: implemented via logind `LockedHint`, not exercised (cannot lock here).
- Blobs (clipboard over 32 KiB, images), outbox, media, input: unchanged from Phase 1.
- `playdrop-ffi` (UniFFI) does not exist; the core API is not yet declared frozen.

---

## 2. Repository map (new in Phase 2)

```
Cargo.toml                            members: playdrop-core, playdrop-cli, playdrop-ipc, playdropd, apps/desktop
docs/ipc.md                           normative IPC spec: contract, D-Bus (SR-26), pipe (SR-27), logging
crates/playdrop-core/src/host/
  transfers.rs                        TransferSnapshot book: last 64 or 10 min of finished transfers
  lock.rs                             LockState trait, AlwaysUnlocked, LockFlag (SR-6)
  diagnostics.rs                      Diagnostics, MulticastStatus, MdnsStatus, PeerDiagnostic, DialFailure
crates/playdrop-core/src/transfer/fsx.rs   now cap-std: openat2(RESOLVE_BENEATH|RESOLVE_NO_SYMLINKS) on Linux (SR-18)
crates/playdrop-ipc/src/
  lib.rs                              Request, Ok, Response, Event, DaemonEvent, Settings, DiagnosticsReport, ErrorCode, RequestHandler, constants
  wire.rs, pipe.rs                    newline JSON framing and id-matched multiplexer (pipe transport, tested in memory on Linux)
  server.rs, server/linux.rs          zbus object, name ownership = single instance, SR-26 caller checks, fd to path
  server/windows.rs, win.rs           named pipe, DACL, SID checks (untested)
  client.rs, client/linux.rs          proxy pinned to the verified owner, StartServiceByName activation, fds for sends
  client/windows.rs                   pipe client (untested)
  linux.rs, util.rs                   procfs checks, Flatpak probe
  tests/dbus_round_trip.rs            private dbus-daemon end to end
  tests/live_daemon.rs                #[ignore]: send a file through a running daemon (PD_PEER=<peer>)
crates/playdropd/src/
  main.rs, cli.rs                     flags: --data-dir --receive-dir --port --name --no-tray --no-clipboard --no-notifications --no-discovery --no-autostart --write-settings --log
  settings.rs                         settings.json (0600, atomic), patch, IPC conversion
  daemon.rs                           Host lifecycle, event drain to broadcast, RequestHandler, restart on receive_dir/name change, error mapping
  lock.rs                             logind LockedHint via zbus; WTS on Windows
  notify.rs                           notify-rust with Open / Show in folder actions; Windows toast
  tray.rs                             ksni menu: devices with last seen, Add device, Send clipboard, Open window, Pause clipboard, Quit
  autostart.rs                        ~/.config/autostart/org.playdrop.Daemon.desktop, ~/.local/share/dbus-1/services/org.playdrop.PlayDrop.Daemon1.service; Run key; skipped under Flatpak
  probe.rs                            loopback-vs-LAN port probe, ufw/firewalld hint, summary lines
  open.rs                             SR-23 rule, receive-folder containment, FileManager1.ShowItems
  network.rs                          NetworkManager StateChanged / networkd, debounced, calls Host::network_changed
  clipboard/mod.rs                    support(), Watcher, read_current, apply; LocalClip Debug prints length and hash only
  clipboard/wayland.rs                wl_clipboard_rs::watch (no polling), x-kde-passwordManagerHint handling
  clipboard/coalesce.rs               100 ms coalescing, newest wins, no repeat of last sent
  clipboard/windows.rs, unsupported.rs
apps/desktop/
  Cargo.toml                          slint 1.18 winit+femtovg+software, unstable-winit-030, rfd portal, qrcode
  src/main.rs                         tokio on a background thread, Slint on main, xdg app id org.playdrop.PlayDrop
  src/ipc.rs                          connect, requests, event pump via invoke_from_event_loop, reconnect
  src/model.rs                        view state, event application, formatting (tests)
  src/screens/*.rs                    connection, devices, pairing, transfers, settings, diagnostics controllers
  src/qr.rs                           qrcode to slint::Image; content protection on Windows
  src/daemon.rs                       spawn playdropd (sibling binary, PATH, flatpak)
  ui/*.slint                          one file per screen plus types, globals, widgets
packaging/
  linux/                              .desktop files, D-Bus service, systemd user unit, metainfo, icons
  deb/                                build-deb.sh with dpkg-deb (no cargo-deb), ufw profile, validated dry run
  flatpak/org.playdrop.PlayDrop.yml   runtime 25.08; finish-args exclude session-bus; owns org.playdrop.PlayDrop.Daemon1
  windows/playdrop.iss                Inno Setup, program-scoped firewall rules TCP+UDP 42187, Run key
```

---

## 3. Public API added to `playdrop_core::host` in Phase 2

Phase 1 handoff section 3 still holds; these are the additions and changes. All types are
`Serialize + Deserialize + Clone + Debug + PartialEq`.

| Signature | What it does |
|---|---|
| `async fn shutdown(&self)` | Was `shutdown(self)`. Idempotent. Saves `last_seen` for every peer |
| `async fn cancel_transfer(&self, id: &str) -> Result<()>` | Either direction. Receiver deletes `.part`, answers later PUTs 410, sends `transfer` cancelled |
| `fn transfers(&self) -> Vec<TransferSnapshot>` | Active plus finished (last 64 and under 10 min) |
| `fn push_clipboard(&self, text: &str, sensitive: bool) -> Result<ClipboardPush>` | Echo rule once, then every online peer with `clipboardOut`. Remembered as sent only if one peer got it |
| `fn diagnose(&self) -> Diagnostics` | Interfaces, multicast join and beacon-seen, mDNS, per-peer last dial failure |
| `fn network_changed(&self)` | Passes to discovery and wakes the last-known dial loop |
| `HostConfig::lock_state: Box<dyn LockState>` | Default `AlwaysUnlocked`; daemon passes a `LockFlag` |
| `open_add_device_window`, `show_qr`, `pair_with`, `scan_qr` | Return `Error::Locked` while locked (SR-6) |
| `PeerSummary.last_seen: Option<u64>` | Unix s of the last frame; persisted in the pin (`Pin.last_seen`, `PinStore::set_last_seen`) |
| `HostEvent::TransferComplete { saved: Vec<PathBuf>, folder: Option<PathBuf>, roots, .. }` | Receive side now reports destinations and roots |
| `HostEvent::TransferOffer { files: Vec<OfferedFile { path, size }>, file_count, .. }` | `files` capped at 1,000 entries; `file_count` is the full count |
| `TransferSnapshot { id, peer, short_id, direction, state: TransferStatus, bytes, total, bytes_per_sec, started_at, file_count, note, saved, folder }` | `state`: offered, active, complete, failed, cancelled |
| `ClipboardPush { sent_to, skipped_echo, skipped_offline, skipped_no_grant }` | |
| `Diagnostics { addr, device_id, short_id, port, bind_ok, interfaces, multicast: MulticastStatus, mdns: MdnsStatus, peers: Vec<PeerDiagnostic> }` | `PeerDiagnostic.last_result: Option<DialFailure { path: DiscoveryPath, failure, at }>` |
| `Error::Locked` | Local only, never on the wire |
| Incoming clipboard `hash` is checked against the text | Mismatch: frame dropped, not stored for echo |

`send_clipboard` now records `last_sent` only after the size and session checks pass.

## 4. IPC contract (`playdrop_ipc`)

See `docs/ipc.md`. Summary: `Request` (serde tag `op`), `Response` (`{"result":"ok","kind":..,"data":..}` or `{"result":"err","code":..,"message":..}`), `Event` (`source: host | daemon`). Linux: `org.playdrop.PlayDrop.Daemon1` at `/org/playdrop/PlayDrop/Daemon1`, methods `Request(s)->s`, `SendFiles(s, ah)->s`, `Version()->(u,s)`, signal `Event(s)`. Windows: `\\.\pipe\playdrop-<SID>`, newline JSON (`wire.rs`). `IPC_VERSION = 1`. `Client::connect()` verifies the owner (UID, `/proc/<pid>/exe` is `playdropd`; both `/proc` checks skipped inside Flatpak), starts the daemon through `StartServiceByName` when a service file exists, and pins all calls to that owner. `Client::send_files` passes fds, never paths, on Linux. At most 250 items per send.

## 5. How it runs today

Build: `cargo build --release --workspace`. Daemon:

```
HOME=/tmp/pd-home target/release/playdropd --data-dir /tmp/pd-a --receive-dir /tmp/pd-a-in --port 42187 --name alpha --no-tray --no-autostart
```

Without `--no-autostart` it writes the per-user autostart entry and the D-Bus activation file
(idempotent, skipped under Flatpak). Data dirs must be on ext4 (the repo volume is not).
Window: `target/release/playdrop-desktop` (connects over D-Bus; "Start" spawns the sibling
`playdropd`). Second PC on one machine: the Phase 1 CLI `playdrop host ... --port 42188 --json`.
Pair over the bus: `busctl --user call org.playdrop.PlayDrop.Daemon1 /org/playdrop/PlayDrop/Daemon1 org.playdrop.PlayDrop.Daemon1 Request s '{"op":"open_add_device_window"}'`, then `pair 127.0.0.1:42187` in the CLI, compare SAS, `{"op":"pairing_respond","accept":true}` and `y`. Live send test: `PD_PEER=bravo cargo test -p playdrop-ipc --test live_daemon -- --ignored`. Live clipboard test: `cargo test -p playdropd -- --ignored clipboard`.

Daemon logs: `RUST_LOG=playdropd=debug,playdrop_core=info` or `--log`. Settings: `<data_dir>/settings.json`.

Window flags and aids: `playdrop-desktop --page devices|add-device|transfers|settings|diagnostics` opens on
that tab (the tray's "Add device" uses `--page add-device`). `PLAYDROP_SNAPSHOT=<file.png>` makes
the window render itself to that file after 2.5 s and exit; this is how the screenshots in section 1
were taken on a compositor that keeps new windows behind the terminal, and it is the way to check a
screen headlessly. COSMIC gives a newly launched window no focus while a terminal is focused; look
for it behind the terminal or in the dock.

---

## 6. Decisions made in Phase 2

| Decision | Why |
|---|---|
| D-Bus name `org.playdrop.PlayDrop.Daemon1`, prefixed with the Flatpak app ID | Flatpak exports activation files only under the app ID; ownership of the app's namespace is implicit, so no `--own-name` |
| Inside Flatpak the `/proc/<pid>` checks are skipped on both sides, UID check kept | Separate PID namespaces; the Flatpak bus proxy already limits who may own or talk to the name |
| One JSON contract on both transports, typed D-Bus methods only as a thin envelope | One serde model, one handler, one test surface; D-Bus stays introspectable |
| Files are sent as fds on Linux, resolved via `/proc/self/fd` and re-checked against the handle | SR-26 literally; the daemon never trusts a path string from the bus |
| Client calls `StartServiceByName` itself rather than relying on implicit activation | The client sends nothing to the daemon before verifying its owner |
| `Error::Locked`, not `Forbidden` | `Forbidden` means "peer lacks a grant"; this is local "unlock first". Maps to IPC `Forbidden` with message "desktop is locked" |
| logind present but unreadable counts as locked; logind absent counts as unlocked (info log) | No lock integration on that system; SR-25's "unknown is locked" is a Phase 5 input rule |
| Window closes "Add device" when the screen locks | SR-6 without a race |
| Finished transfers kept while among the last 64 and under 10 min | Late UIs see recent history; memory bounded |
| `TransferOffer.files` capped at 1,000 entries, `file_count` exact | Keeps one IPC event small |
| Clipboard watcher uses `wl_clipboard_rs::watch`, no polling; data reads on a helper thread with 2 s timeout; `copy` served in the background | 2 ms latency; a stuck source app cannot hang the daemon |
| Password-manager hint unreadable counts as secret | SR-24 fails safe |
| Text over 32 KiB still offered to the core, which answers `TooLarge` | Blobs are Phase 3; the daemon reports the reason instead of silently dropping (anti-pattern 18) |
| Per-user D-Bus service file rewritten every start; autostart entry only on first run or setting change | Activation matters most when autostart is off; a user who deletes the autostart entry is not overruled |
| Non-root ufw: "port may be blocked ... rules unreadable without root" | Honest wording; the probe cannot read ufw rules unprivileged |
| Windows unsafe allowed as `deny` plus scoped `allow` in the three new crates, not workspace `forbid` | Win32 needs FFI; every block carries a SAFETY comment; Linux builds have none |
| Add-device list shows one row per name and type, keeping the best address (private IPv4 first, link-local IPv6 last) | The core reports every address a device is heard on; five "bravo" rows for one laptop with Docker bridges was confusing, and the SAS check guards against a wrong pick |
| Slint `unstable-winit-030` enabled | File drop (X11, Windows) and `set_content_protected` for the QR; Wayland drops still unsupported by winit |
| No `cargo-deb` | It only reads `Cargo.toml` metadata, which agents may not edit; plain `dpkg-deb` script instead |
| Flatpak runtime 25.08 | 24.08 is out of support |

---

## 7. Open TODOs

| File:line | Cites | What | Phase |
|---|---|---|---|
| `playdropd/src/notify.rs:170` | PLAN find | Ring sound on `Find` | 3 |
| `playdropd/src/notify.rs:424` | | Windows toast buttons need a COM activator | 2 follow-up (Windows) |
| `playdropd/src/tray.rs:343` | | Windows tray device lines and battery | 2 follow-up (Windows) |
| `playdropd/src/autostart.rs` | | Background portal autostart under Flatpak | 6 |
| Phase 1 TODOs still open | SR-20, SR-31, SR-24 blob, 5.15, 5.4, SR-15, SR-21, 7.8 | See `phase-1-core.md` section 7; SR-18 `openat2` and SR-6 lock are closed | 2 (Windows), 3 |

---

## 8. Known gaps and risks

- Windows: nothing compiled. Expect API fixes on the first `x86_64-pc-windows-msvc` build in
  `playdrop-ipc` (`win.rs`, `*/windows.rs`), `playdropd` (`lock.rs`, `notify.rs`, `tray.rs`,
  `clipboard/windows.rs`, `open.rs`), `apps/desktop/src/qr.rs`.
- Flatpak: unbuilt. Tray under Flatpak relies on ksni without a bus name; unverified.
- GNOME: no clipboard watcher (reason shown in settings); tray needs the AppIndicator
  extension; untested.
- Lock refusal (SR-6) and the firewall-blocked wording with ufw enabled are unit-tested, not
  exercised live.
- The core emits `AddDeviceSeen` for every advert it hears (44 in 5 s during pairing); the
  window collapses them per device; the core should dedup too.
- A notification with buttons holds a thread; daemon shutdown waits at most 2 s for it.
- The daemon keeps running without IPC if the server fails for a reason other than
  "already running" (files still arrive; the window cannot connect). Logged at error.
- `playdropd` is a binary-only crate; its tests are in-module unit tests.
- Dates in the window are UTC (no time-zone crate).
- Second-machine checks (mDNS and beacon on a real LAN, a phone, ufw enabled) are not done.

---

## 9. Phase 3 kickoff brief

PLAN.md "Build phases", row 3:

> | 3. Android + session plane | Kotlin `connectedDevice` FGS hosting the core; persistent notification with "Send clipboard" and "Send to default" actions; one-tap share-sheet target for the default device; `ACTION_SEND`/`SEND_MULTIPLE`; photo picker/SAF; battery onboarding; session stream with ping, find, clipboard, battery, media; outbox host for iPhones | Share sheet to default device is one tap; the notification action sends the clipboard with the app closed; a 400 MB video arrives with the phone locked; after 30 minutes of Doze the phone is still reachable by last-known IP, or honestly shown as asleep; destroying the Activity does not stop the server; reconnects after a Wi-Fi drop without re-pairing; idle battery cost measured and acceptable |

**Step 0 (serial, one agent):** freeze the core API (PLAN.md: "freeze the core API before
generating UniFFI bindings", anti-pattern 20). Concretely: (1) decide the fd/stream story
for mobile, since `Host::send` takes paths and Android SAF gives content URIs; add
`Host::send_sources(peer, Vec<SendSource>)` where `SendSource` is a path or a name plus an
owned fd / `AsyncRead` with known size, and make `HostConfig.receive_dir` optional in favour
of a `ReceiveSink` trait (anti-pattern 20: no filesystem paths in the core on mobile);
(2) blobs endpoint (protocol.md 5.15) so clipboard over 32 KiB and images work, closing the
Phase 1 TODO at `host/mod.rs` (SR-24) and `router.rs:103`; (3) `Host` methods to send
`battery`, `presence`, `find` and `capabilities` (Phase 1 gap 7); (4) `AddDeviceSeen` dedup
in the core; (5) outbox endpoints (7.8) at least as a receiver-pull skeleton; (6) create
`crates/playdrop-ffi/` with `uniffi` and the Android Gradle skeleton under `apps/android/`,
pre-adding every dependency. Parallel agents then never touch `Cargo.toml` or Gradle files.

| Task | Builds | Phase 2 pieces reused |
|---|---|---|
| a. FFI | `playdrop-ffi`: UniFFI surface over `Host`, `HostEvent` as a callback interface, `LockState`/`InterfaceSource`/`FreeSpace` as foreign traits, Android `connectedDevice` FGS keep-alive profile `Mobile` | The event enum and snapshot types are already serde; the daemon's `RequestHandler` mapping in `playdropd/src/daemon.rs` is the reference for what a host surface needs |
| b. Android host | FGS, boot receiver, Wi-Fi change to `Host::network_changed`, MulticastLock only in discovery windows, Keystore-wrapped identity (SR-28), `exported=false` (SR-29) | `playdropd/src/network.rs` and `lock.rs` show the shape of the OS hooks |
| c. Share and notification | one-tap share target, notification actions, SAF/photo picker to `send_sources` | `apps/desktop/src/screens/devices.rs` one-click send path |
| d. Desktop follow-ups that Phase 3 needs | Windows compile and run; `Find` ring; blobs in the daemon clipboard path; the outbox on the desktop so an iPhone can pull later | This phase's Windows TODO list in section 8 |
| e. Phase 3 tests | Doze, locked-screen receive, Wi-Fi drop, battery cost | `crates/playdrop-ipc/tests/live_daemon.rs` as the pattern for ignored live tests |

Desktop numbers to beat on a phone: 2 to 5 ms clipboard latency on loopback; the real target
stays "under 1 s on Wi-Fi".

---

## 10. Rules carried forward

- Hyphen only. Never the long dash, in code, comments, docs or commits.
- No AI attribution anywhere: no co-author trailers, no "generated with" lines, no session links.
- Agents do not commit or push. The user commits and runs `git push`; agents hand over the command, a PR title and a short bullet description.
- Parallel agents never edit any `Cargo.toml` (or Gradle files). Dependencies are added up front in a serial step.
- Never bind or announce on 1714-1764, 53317 or 224.0.0.167.
- Manual runs use data and receive dirs on ext4 (`/tmp/pd-*`), never under the repo, and `HOME=/tmp/pd-home` or `--no-autostart` so the real user config is untouched.
- Every phase ends by writing `docs/phases/phase-N-<name>.md` in this same shape.

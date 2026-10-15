# Phase 7 handoff: daily use (v0.2.0)

Reader: a fresh session that must finish the owed live gates below, tag `v0.2.0`, or start
Phase 8. Read `CLAUDE.md`, `PLAN.md` ("Build phases" row 7, "Anti-patterns"),
`docs/phases/phase-6-ship-gate.md` sections 5, 6 and 8, then this file. Code to read is
named per item; do not re-read the whole tree.

---

## 1. Status

| Item | Value |
|---|---|
| Base | `main` at `2408182` (Phase 6 complete, v0.1.0 released); this phase is one commit on top |
| Built on | This Pop!_OS COSMIC machine, 2026-10-14 evening to 2026-10-15, one session: scope proposal, five parallel agents (Android landing, CLI over IPC, SR-32 canary, docs, small items), two review agents, five fix rounds, one core durability agent, then integration |
| Version | Workspace `0.2.0` (`Cargo.toml`), `playdrop-ffi` `0.2.1` (content change in the diagnostics report, `docs/core-api.md` section 10), iOS `MARKETING_VERSION` synced by script |
| Scope chosen | Android bulk drops at engine speed; `playdrop` as a client of the daemon; README, user guide and changelog; SR-32 canary test; autostart refusal as an IPC field; SR-30 test; iOS version sync. Not this phase: iroh, Local Push Connectivity, Wi-Fi Aware, libcosmic applet, MSIX, notification mirroring, Windows live items, Android 14 and 15 emulators, SR-34 external review |
| Rust tests | 435 passed, 0 failed, 8 ignored (`cargo test --workspace --no-fail-fast`, up from 394: the quiet double run counted 434 before the slot test was split out of `offer_names_and_cancel_by_id`); `cargo clippy --workspace --all-targets` zero warnings; `cargo fmt --all --check` clean |
| Android tests | 148 JUnit (up from 110); `:app:compileDebugKotlin` no warnings; `:app:assembleRelease -Pplaydrop.signWithDebugKey=true` builds (`apps/android/app/build/outputs/apk/release/app-release.apk`) |
| iOS checks | `apps/ios/scripts/check-linux.sh` exit 0 (now includes the version check); `PlayDropShared` 106 XCTests on Linux (up from 102) |
| Phone | Run on 2026-10-15 after the first commit (the A54 was `unauthorized` on adb during the build): every item in section 6 passed or was measured; the rename into Downloads was removed as a result (section 4). Results in `docs/release/throughput.md` section 8 |

### 1.1 What landed

**Android: landing folder and background move into Downloads** (`apps/android/.../core/DownloadsSink.kt`,
`LandingPolicy.kt`, `LandingTree.kt`, `LandingMover.kt`, `NameClaims.kt`, `MoveTally.kt`,
`FileNames.kt`, `CoreHostImpl.kt`, `service/Notifications.kt`, `NotificationPlan.kt`,
`ActionReceiver.kt`).

- Direct path (Android 11+): unchanged from Phase 6 (create with `O_EXCL` in
  `Download/PlayDrop/<dirs>/<name>` and a kernel copy), except that the target name is now
  claimed under a per-folder lock shared with the landing path. The phase first tried a
  `rename(2)` of the staging file into Downloads ahead of the copy; the phone run on
  2026-10-15 showed MediaProvider serving that rename as a copy behind the scenes, slower than
  the app's own copy and not atomic (section 6), so the rename into Downloads was removed on
  both routes before the commit of the live results. The staging path still comes from
  `/proc/self/fd` for the rename into the landing folder, which is a real rename.
- Landing (Android 11+): a transfer with at least 64 files and a mean size under 1 MiB
  (`LandingPolicy`) is delivered into `getExternalFilesDir("landing")/<tid>/<dirs>/<name>`
  (about 0.2 ms per file, no MediaProvider row). The decision uses `TransferOffer` when one
  arrives (only when the peer lacks the `files` grant) and otherwise `host.transfers()` on the
  first file; it depends only on count and bytes, so a restarted process decides the same.
  `deliver` returns the FileProvider URI of the promised Downloads name.
- Mover: a serial queue on the host scope copies landed files into Downloads with the same
  create-and-copy primitive as direct delivery, batched media scans, and a "Moving n of m
  files into Downloads" notification (tagged, cleared in `finally`, stale ones replaced at
  start). Every move is journaled (target created `O_EXCL`, journal written and fsynced,
  copy, delete landed, clear),
  the MediaStore fallback journals the row URI; recovery at start removes an unfinished copy.
  Retries inside the process at 1, 5, 30 minutes, 2 h, 6 h, on `ACTION_MEDIA_MOUNTED`, and from
  a "Try again" notification action. Landed files are never discarded: a terminal failure or a
  cancel moves what arrived and says so ("n files arrived before the transfer stopped"),
  matching the direct path, which keeps delivered files too. A `TransferFailed` counts as the
  end only when the transfer's state in `host.transfers()` is terminal, because the core also
  emits it for one file's integrity failure while the transfer continues.
- Known limits (documented in the KDoc): a kill between the `O_EXCL` create and the journal
  write can leave one empty file in Downloads; a file renumbered by the mover leaves the
  location the core stored stale (the notification then opens the folder; the in-app transfer
  list still shows the stored location); a landed file's URI does not open until its move
  finishes.

**Core**

- `transfer/receiver.rs`: bundle member delivery is durable. `<staging>/bundle.delivered` is
  an append-only JSON-lines log (header `{"v":1,"root":...}` written atomically after the
  members are synced and the ledger checkpointed; one `SavedFile` line per member, written
  before the rename in `Directory` mode and after `sink.deliver` in `Staged` mode; fdatasync
  every 256 members and at the end; staging files deleted only after the flush that covers
  them). `TransferReceiver::build` on `load` resumes delivery from the log without any new
  download. Before this, a kill during the delivery loop reset the whole bundle and every
  member already placed came back as "name (1)". Staged mode can deliver the one member in
  flight twice after a kill (the trait cannot make hand-off and record atomic); documented on
  `hand_members`. Tests: `bundle_delivery_survives_a_kill_in_directory_mode`,
  `bundle_delivery_survives_a_kill_in_staged_mode` (kill before member 0, 1, n-1 and inside
  each), `delivery_log_drops_a_torn_tail`.
- `host/send.rs`: a local cancel that ended the sender loop as a fatal step (while waiting for
  the decision, or mid-stream) returned without the `DELETE`, so the receiver kept the offer
  pending or the transfer active and counted it against the 4 per peer (SR-20) until the
  24 h sweep; found live by the CLI agent after four Ctrl-C sends ("429 rate_limited"). The
  loop now sends `cancel_remote` on the connection it holds. `host_smoke.rs`
  `offer_names_and_cancel_by_id` asserts the receiver reaches `Cancelled` after a sender
  cancel of a pending offer, and `sender_cancels_free_the_receiver_slots` cancels four
  pending offers and one streaming transfer and checks a fifth offer is accepted; both fail
  without the fix.
- `host/diagnostics.rs`: `Diagnostics.device_id` and `PeerDiagnostic.peer` carry the first 8
  hex characters (SR-32), found by the canary test. No screen matched on them (`short_id` and
  `label` are what the UIs use). `docs/core-api.md` section 10, `playdrop-ffi` 0.2.1.
- `transfer/receiver.rs`: one DEBUG line when a verified file is placed (file names are
  allowed at DEBUG and below only), so the canary test can prove the level rule.
- `tests/sr32_canary.rs`: one global TRACE subscriber into a buffer (plus a second layer for
  `playdrop*` targets), two loopback hosts, SAS pair, unpair, QR pair, short and blob
  clipboard, a streamed plus a bundled file, an input session, both diagnostics reports.
  Asserts absent: both private keys (seed and file, hex both cases, base64, base64url, byte
  list), every announce key on disk including rotated ones, the SAS as a standalone token on
  core lines (numbers the test produces are excluded, with a planted self-check), the QR
  secret, every file token the sink saw, both clipboard canaries, the typed text, file
  contents, full Device IDs in hex and base32. File name at INFO absent in the INFO window,
  present on DEBUG lines only in the DEBUG window, with windows bounded by completion events.
  Ten runs in a row under load passed. Tokens the plain-folder receiver issued and nonces are
  not observable from outside (covered by the existing redaction unit tests).

**CLI** (`crates/playdrop-cli/src/daemon/{mod,peer,output,commands,send}.rs`, `main.rs`):
`playdrop send <PATH>... [--to <peer>] [--no-wait] [--json]`, `peers`, `status`,
`transfers`, `diagnose`, `clip (<TEXT> | --stdin) [--sensitive]`, `ping [MESSAGE]`,
`outbox`, `outbox add <PATH>... --to <peer>`. Peer resolution: exact Device ID, then Short
ID prefix (hyphens optional), then exact label, case-insensitive; two different peers
matching is `Ambiguous`; with `--to` omitted the only online peer is used. Exit codes 0 ok,
1 failed, declined, cancelled or interrupted, 2 usage or no such peer, 3 daemon not running.
`send` subscribes to events before sending, follows the transfer with a once-a-second list
poll (ends with a failure if the transfer was listed and then evicted, or never listed after
10 ticks; a failed poll means "the daemon went away, the transfer resumes when it is back"),
one Ctrl-C listener for the whole command (first cancels, second exits 1), the in-place
progress line cut to `COLUMNS - 1` or 79. `--json` prints the daemon's objects as they are.
Paths are made absolute before they leave the process (on Windows they travel as paths).
Clippy clean on `x86_64-pc-windows-gnu` through the stub-cc method; 35 unit tests. Verified
live against daemon `alpha` and a `playdrop host` `bravo`: 300 MiB, a 231-file folder, 3 GiB
plus a subfolder, all hashes matching; decline, Ctrl-C, unknown peer, missing path, no
daemon each give the documented code. `bench/desktop-to-phone.sh` now uses
`playdrop send --json`; `bench/pd_ipc.py` is deleted (wall time is measured around the CLI
call; `bench/README.md` says so).

**Daemon, IPC, window**: `Settings.autostart_refused: Option<String>` (`docs/ipc.md` 1.3),
persisted in `settings.json`, set when the Background portal denies or is unavailable or the
login entry cannot be written (the daemon also turns `autostart` off, saves and emits
`SettingsChanged`), cleared on the next `autostart` patch (counted as a change so the event
fires) or when the portal later allows. An attempt counter dropped stale portal answers and
deferred failure writes. The window shows the field and the five-minute inference is gone.

**iOS**: `URLSchemeRule.shouldHandle` in `PlayDropShared` (scheme only, case-insensitive;
host, path, query and fragment never read), `URLSchemeHandler` calls it, four tests (SR-30
row now "Pass (unit test)"). `scripts/sync-version.py` rewrites `MARKETING_VERSION` in
`project.yml` from `Cargo.toml` (`--check` in `check-linux.sh`; sync in `build-core.sh` and
`xtool-dev.sh`); the release workflow's override stays.

**Docs and release**: `README.md` (first one), `docs/guide/` (install, pairing, sending,
clipboard, remote input, can't find your device, CLI, troubleshooting), `CHANGELOG.md`
(0.1.0 dated, `0.2.0 - Unreleased`), `release.yml` takes the release body from the matching
changelog section and fails early when it is missing, `metainfo.xml` has a 0.2.0 release
entry with a placeholder date (and the 0.1.0 date corrected to 2026-10-15).

### 1.2 Checks run at integration

| Check | Result |
|---|---|
| `cargo fmt --all --check` | clean |
| `cargo clippy --workspace --all-targets` | zero warnings |
| `cargo test --workspace --no-fail-fast` | 434 passed, 0 failed, 8 ignored, twice in a row on the quiet machine (435 with the split test); the only failure seen during the session was `gate_b_many_small_files_hold_throughput` at 0.40 to 0.49 against the 0.50 debug floor while five agents and Gradle ran (load average up to 57), passing every time alone and on the quiet reruns |
| Android `:app:testDebugUnitTest` | 148 passed |
| `apps/ios/scripts/check-linux.sh` | exit 0, 106 Swift tests |
| `sync-version.py --check` | in step at 0.2.0 |
| `appstreamcli validate --pedantic --no-net` | passes (one pedantic note about the uppercase in the component id, same as before) |

---

## 2. Repository map (new or changed in Phase 7)

```
README.md, CHANGELOG.md
docs/guide/{install,pairing,sending,clipboard,remote-input,cant-find-your-device,cli,troubleshooting}.md
docs/core-api.md                              section 10 (diagnostics IDs, ffi 0.2.1)
docs/ipc.md                                   1.3 autostart_refused
docs/release/{sr-checklist,throughput}.md      SR-20, SR-30, SR-32 rows; pd_ipc.py note
.github/workflows/release.yml                 release body from CHANGELOG.md
packaging/linux/org.playdrop.PlayDrop.metainfo.xml   0.2.0 release entry
crates/playdrop-cli/src/daemon/               mod, peer, output, commands, send
crates/playdrop-cli/src/main.rs               daemon subcommands, ExitCode
crates/playdrop-core/src/host/{send,diagnostics}.rs
crates/playdrop-core/src/transfer/receiver.rs DeliveryLog, restore, hand_members
crates/playdrop-core/tests/{sr32_canary,host_smoke}.rs
crates/playdrop-ipc/src/{lib,pipe}.rs, tests/dbus_round_trip.rs
crates/playdropd/src/{daemon,settings}.rs
apps/desktop/src/{model.rs,screens/settings.rs}
apps/android/.../core/{DownloadsSink,LandingPolicy,LandingTree,LandingMover,NameClaims,MoveTally,FileNames,CoreHostImpl}.kt
apps/android/.../service/{Notifications,NotificationPlan,ActionReceiver}.kt, res/values/strings.xml
apps/ios/Packages/PlayDropShared/.../URLSchemeRule.swift (+ tests), PlayDrop/Share/URLSchemeHandler.swift
apps/ios/scripts/{sync-version.py,build-core.sh,xtool-dev.sh,check-linux.sh}
bench/desktop-to-phone.sh, bench/README.md    (bench/pd_ipc.py deleted)
Cargo.toml 0.2.0, crates/playdrop-ffi/Cargo.toml 0.2.1, crates/playdrop-cli/Cargo.toml (+playdrop-ipc, serde)
```

---

## 3. Public API

| Surface | Change |
|---|---|
| `docs/core-api.md` | No type change. `Diagnostics.device_id` and `PeerDiagnostic.peer` are now 8-character prefixes (section 10); `playdrop-ffi` 0.2.1 |
| IPC | `Settings.autostart_refused: Option<String>` (absent from older daemons reads as `None`) |
| CLI | The daemon subcommands above are new public surface; `host` and `bench` unchanged |
| Receiver on disk | `<staging>/bundle.delivered` (receiver-local, no wire change) |

---

## 4. Decisions made in Phase 7

| Decision | Why |
|---|---|
| Land bulk drops in the app folder and move later, rather than an archive or `MANAGE_EXTERNAL_STORAGE` | Keeps "no throughput drop between 100 and 5,000 files" true at the sender with no new permission and no user step; the MediaProvider cost is paid off the critical path. Thresholds 64 files and 1 MiB mean are documented in `LandingPolicy` |
| Landed files are never discarded | The direct path keeps delivered files on every kind of end, and every landed file is root-verified; discarding would lose data the sender believes arrived |
| Never rename into Downloads; rename only from staging into the landing folder | Measured on the A54 on 2026-10-15 (section 6): `rename(2)` from the app's folder into `Download` succeeds, but MediaProvider serves it as a copy behind the scenes, slower than the app's own copy (8.0 s against 4.1 s for 1 GiB, 29 ms against 14 ms per small file) and not atomic (a force-stop mid-move left one file both landed and in Downloads, then renumbered). The journaled copy is faster and survives a kill. Staging to landing is a real rename (0.1 ms per file) and keeps the per-route gate |
| Bundle delivery log instead of per-member `.part` promotion | Smallest change that makes "a member leaves staging" durable without touching the wire or the `ReceiveSink` trait |
| Diagnostics IDs truncated rather than a new redacted export | The report is the export; truncation matches the log rule and nothing matched on the fields |
| CLI peer order: ID, Short ID prefix, label; ambiguity is an error | A label is the peer's own advertised name and must not capture `--to` meant for another device |
| CLI progress width from `COLUMNS` or 79 | No terminal-size dependency without a `Cargo.toml` edit and no platform-specific code in the CLI |
| `release.yml` fails when the changelog section is missing | Release notes are part of the release; an empty body must not ship silently |
| Scope left out (PLAN row 7 items) | Each needs its own design or a platform this machine lacks; none serves daily use before the owed live gates pass |

---

## 5. Open TODOs

| File or area | What | Phase |
|---|---|---|
| `apps/android/.../core/DownloadsSink.kt` | A renumbered landed file leaves the core's stored location stale in the in-app transfer list; a PlayDrop-owned lookup URI would need a `ContentProvider` | 8 |
| `crates/playdrop-core/src/transfer/receiver.rs` | Bundle members are delivered one at a time after the bundle has fully arrived, about 2 ms each including the sink: 10 s of the 17 to 23 s a 5,000-file drop takes to the phone (`throughput.md` 8.2). Deliver while streaming, or hand the sink a batch | 8 |
| `apps/android/.../core/LandingPolicy.kt` | The 1 MiB mean threshold saves 11.5 ms per file whatever its size (one MediaProvider row); the data would support a mean up to about 4 MiB at the cost of writing those bytes twice. Left as is | 8 |
| `crates/playdrop-core/src/transfer/receiver.rs` | Single streamed files still have the window between `place` and `ledger.finish`: a kill there re-downloads the file and it lands as "name (1)". Same treatment as the bundle log | 8 |
| `crates/playdrop-core/src/transfer/receiver.rs` | A bundle delivery error without a restart (non-sink I/O) still lets the sender restart from 0 and re-download | 8 |
| `crates/playdrop-core/src/host` | The sender's snapshot flips to `active` and zero-byte progress events arrive while the receiver is still deciding; the CLI works around it. No per-file progress in events or snapshots | 8 |
| `crates/playdropd` | The daemon's clip answer sets `skipped_echo` for every "nothing sent" case, so a client cannot tell an echo skip from another reason | 8 |
| `crates/playdrop-cli` | Terminal width from the OS once a dependency is allowed; a Ctrl-C in the first tens of ms (before the listener) still exits 130 | 8 |
| `docs/release/sr-checklist.md` | SR-32 is Partial: daemon and app logs and log file modes are left to a reviewer; SR-34 external review and hostile-LAN run not done | 8 |
| `CHANGELOG.md`, `metainfo.xml` | Put the release date on the 0.2.0 heading and entry before tagging | before `v0.2.0` |
| Windows | Every live item from Phase 6, plus gate B in release on a real machine | when a machine exists |
| Phase 4 carry-over | Share-extension gate needs an App Group | later |

---

## 6. Live runs on the phone (2026-10-15, after the first commit)

Daemon `alpha` rebuilt from the Phase 7 commit, release app installed over the one on the
phone (pairing kept), `adb logcat -s playdrop:*` read for the `[sink]` and `[landing]` lines.
Results in full in `docs/release/throughput.md` section 8.

| Run | Result |
|---|---|
| `tree`, `big-1g`, `medium-100`, `mixed-5000` by hand, then `bench/desktop-to-phone.sh --peer SM-A546U1 --runs 3` | All 15 bench runs verified (counts and sha256). `mixed-5000` 7.2 to 9.5 MiB/s at the sender (was 1.9), `small-1000` 14.8 to 20.1 (was 4.5 to 5.2), `tree` 12.0 to 13.5 (was 7.0), `medium-100` 33.9 to 46.0, `big-1g` 43.5 to 47.6. Files of a bulk drop reach Downloads 2 s (`tree`), 17 s (`small-1000`) or 85 s (`mixed-5000`) after the send returns |
| Rename through FUSE | Succeeds on this phone, but it is a copy behind the scenes (8 ms per MiB), slower than the app's own copy and not atomic (a force-stop mid-move left a duplicate). Removed on both routes, see section 4; the rename from staging into the landing folder is real (0.1 to 0.2 ms) and stays |
| Resume by `am force-stop` at 50% of 1 GiB, relaunch after 8 s | Pass: finished 21.6 s after the relaunch, sha256 equal, one file, no " (1)" |
| Landing notification flow | "Moving 202 of 5,000 files into Downloads" while the mover runs; after a force-stop and relaunch the sweep continues ("Moving 325 of 4,779 files"); ends with "4779 received files moved into Downloads", "Saved in Download/PlayDrop". Read from `dumpsys notification`, not by eye |
| Gate "`mixed-5000` at or above 0.8x of `medium-100`" | Not met: about 0.2. The floor moved from MediaProvider to the core's per-member delivery (section 5, Phase 8) |

Not run: a Wi-Fi drop (instead of a force-stop) mid-transfer; the thresholds were left as
they are (section 5).

---

## 7. Phase 8 kickoff brief

Section 6 is done and the thresholds stay. From section 5 by value: bundle members
delivered while the bundle streams (or in batches to the sink), which is what now bounds a
5,000-file drop to the phone; single-file placement durability (same log idea as the
bundle); the sender's early `active` state and per-file progress; and the stale-location
gap on Android. Tag `v0.2.0` first (date the changelog heading and the metainfo entry).
PLAN.md row 7 items (iroh, Local Push Connectivity, Wi-Fi Aware, libcosmic applet, MSIX)
stay behind flags and each wants a design note before code.

---

## 8. Rules carried forward

- Hyphen only. Never the long dash, in code, comments, docs or commits.
- No AI attribution anywhere: no co-author trailers, no "generated with" lines, no session links.
- Agents do not commit or push. The user commits and runs `git push origin main`.
- Parallel agents never edit any `Cargo.toml`, Gradle file, `Package.swift` or `project.yml`;
  the main session does those edits first, and `sync-version.py` is the only thing that
  changes `MARKETING_VERSION`.
- Never bind or announce on 1714-1764, 53317 or 224.0.0.167.
- Manual runs use data and receive dirs on ext4 (`/tmp/pd-*`), never under the repo, and
  `HOME=/tmp/pd-home` or `--no-autostart`. Shut down a daemon left from an earlier session
  over D-Bus before starting a new build.
- Never judge phone speed from a debug APK; use `assembleRelease -Pplaydrop.signWithDebugKey=true` on a test phone.
- Subagents: Opus for core, protocol or cross-cutting daemon work; Sonnet for UI screens,
  scripts, CI and docs; Haiku for lookups. Send every agent's diff through a reviewer before
  integration: both reviews this phase found bugs the authors' tests missed.
- `docs/core-api.md` is frozen: a change needs a note in the phase handoff and an ffi version bump.
- Every phase ends by writing `docs/phases/phase-N-<name>.md` in this same shape.

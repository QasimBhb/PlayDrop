# Phase 8 handoff: fast bulk and durable placement (v0.3.0)

Reader: a fresh session that starts Phase 9. Read `CLAUDE.md`, `PLAN.md` ("Build phases" row 7,
"Anti-patterns"), `docs/protocol.md` 11.1 (last bullet), `docs/threat-model.md` SR-22 note,
`docs/core-api.md` section 11, `docs/phases/phase-7-daily-use.md` sections 4 and 5, then this
file. Code to read is named per item; do not re-read the whole tree.

---

## 1. Status

| Item | Value |
|---|---|
| Base | `main` at `a7bab88` (Phase 7 complete, v0.2.0 released); this phase is one commit on top |
| Built on | This Pop!_OS COSMIC machine, 2026-10-15 evening to 2026-10-16, one session: scope proposal (seven items, approved with the protocol deviation below), skeleton (versions, `files_done` plumbing, one CLI dependency), five parallel agents (core receiver and core host on Opus; Android, daemon plus CLI, CI on Sonnet), four review agents (one per diff, Opus for core), two fix rounds (Android, receiver), then integration and docs |
| Version | Workspace `0.3.0` (`Cargo.toml`), `playdrop-ffi` `0.3.0` (additive type change, `docs/core-api.md` section 11), iOS `MARKETING_VERSION` synced by script |
| Scope chosen | Bundle members delivered while the bundle streams; single-file placement durability; honest sender state and `files_done`; Android stale location after a renumbered move; clip answers say why; CLI width, early Ctrl-C and files shown; Linux and Android CI. Not this phase: PLAN row 7 items (iroh, Local Push Connectivity, Wi-Fi Aware, libcosmic applet, MSIX), SR-34, Windows live items, `LandingPolicy` thresholds, a batch sink API (unneeded once members go out early) |
| Rust tests | 466 passed, 0 failed, 9 ignored (`cargo test --workspace --no-fail-fast`, up from 435); `cargo clippy --workspace --all-targets -- -D warnings` zero warnings; `cargo fmt --all --check` clean |
| Android tests | 168 JUnit (up from 148); `:app:compileDebugKotlin` no warnings; `:app:assembleRelease -Pplaydrop.signWithDebugKey=true` builds |
| iOS checks | `apps/ios/scripts/check-linux.sh` exit 0; `PlayDropShared` 106 XCTests on Linux; strict type check clean with `filesDone` in the fixtures |
| Release | Not tagged. `CHANGELOG.md` has `0.3.0 - Unreleased`; `metainfo.xml` has a 0.3.0 entry with a placeholder date (`2026-12-31`): date both before tagging, as the release workflow requires |
| Phone | Bench run on 2026-10-16 (section 6, `docs/release/throughput.md` section 9): all 15 runs verified, `mixed-5000` 21.9 to 22.6 MiB/s (was 7.2 to 9.5), wall 7.3 s (was 17.5 to 22.9). The Wi-Fi drop gate is still owed: every Wi-Fi toggle left the phone isolated from the desktop on the mesh network |

### 1.1 What landed

**Core receiver: bundle members placed while the bundle streams**
(`crates/playdrop-core/src/transfer/receiver.rs`, `sink.rs` docs, `sender.rs` comment;
`tests/phase8_receiver.rs`). Phase 7 measured 10 s of a 5,000-file drop's 17 to 23 s in the
serial delivery loop that ran after the `pd-root` trailer. Now:

- A member is eligible once its `data_end()` is at or below the ledger's durable offset
  (`Ledger.durable`, set in `persist`, `load`, `reset`, `finish`), so eligibility moves at
  each checkpoint (16 MiB or 1 s) and never for page-cache-only bytes.
- A dedicated `std::thread` (`playdrop-deliver`, `DeliveryWorker`) takes fids in archive
  order over a channel, owns the delivery log, the `done` map and a `Placer` (folder handles
  walked once per folder, SR-18, synced when the worker stops). The stream thread never
  waits on it. Per-member order is Phase 7's (Directory: reserve, log line, rename; Staged:
  sink, log line, staging file deleted after the batch flush).
- Delivery log v2: header `{"v":2}` at upload start; the verified root lives in the bundle's
  ledger record. A v1 log from a 0.2.x build is read as plain lines.
- `finalize`: root mismatch stops the worker and fails the bundle with the same "failed
  integrity check" wording; members already placed stay (now the rule on every platform)
  and `delivered()` still lists them. Root match persists the root, drains the worker, then
  `finish(Verified)` with `saved` in manifest order.
- Restart at 0 (sender prefix mismatch): the worker stops after the member in flight, the
  `done` set is kept, undelivered staging files are removed, the ledger resets, and no
  placed member is placed again (its re-streamed bytes are dropped; a changed manifest would
  have been a `409`).
- Cancel raises an `AtomicBool` then takes the gate, so it returns after the member in
  flight even with thousands queued; nothing is placed afterwards and no log is re-created
  in the cleared staging folder.
- Kill and load: `recover()` finishes half-done members from the log (Directory: rename onto
  the empty placeholder, or place afresh; a placeholder that already holds data counts as
  placed and the staged copy is dropped), `done_count` is restored for `files_done`, and
  delivery finishes during the load when the root is known.
- A non-sink I/O error in delivery (a destination folder deleted mid-transfer, ENOSPC) no
  longer costs a re-download: `GET` shows `receiving` at the full size; `finish` and a `PUT`
  from 0 on a verified bundle retry with a fresh worker and answer `409 offset_mismatch` at
  the full size; the sender re-plans and its next `GET` finds `verified`. `host/send.rs` maps
  a `500 internal` from a `PUT` to a re-plan too (bounded by the 8-round stall limit).
- Orphan placeholders are removed when the log append or ledger persist right after
  `create_unique` fails; a missing staging file with no log line is believed only when a
  regular file of the member's size sits at the manifest name, else the bundle restarts.
- `Drop for BundleInner` joins the worker through `tokio::task::block_in_place` on a
  multi-thread runtime, so the async thread that drops the last receiver is not blocked.
- `TransferReceiver::files_done()`: streamed files verified or skipped, bundle members placed
  (live), zero-length files once created.
- Benchmark (`--test phase8_receiver -- --ignored --nocapture`, debug): 5,000 members, a
  sink sleeping 0.5 ms per file, stream paced over 3.4 s: wall 4.36 s, 0.98 s after the
  root, 3,480 members already delivered when the stream ended (all after the stream would be
  about 6.5 s). Gate B in debug: 0.68 to 1.24 depending on load.

**Core receiver: single-file placement durability.** `PartInner::finalize` persists the
root, reserves the final name, persists a record with the reserved name while still
`receiving`, renames, syncs the folder, then `finish(Verified)`; `Staged` persists the root,
hands to the sink, finishes, deletes. `build` runs `resume_placement` on load (`.part` gone
and a name reserved: verified at that name; `.part` present: rename onto the still-empty
placeholder, else place afresh). A `PUT` from 0 on a fully received file with a verified root
retries placement and answers `409` at the full size. The kill window between creating the
placeholder and recording it needs `renameat2(RENAME_NOREPLACE)` to close (comment in code).

**Core host** (`host/send.rs`, `transfers.rs`, `events.rs`, `receive.rs`, `mod.rs`;
`tests/phase8_host.rs`). A send is `offered` until the first `PUT` body moves bytes
(`Job.streaming`), `active` from then on across reconnects; no `TransferProgress` while
`offered`; a declined offer emits none. `files_done` on the send side (`DoneFiles`, rebuilt
per connected round from the receiver's `GET` view, marked as `PUT`s return `verified`,
monotonic within a round), one final reading at `bytes == total`, `files_done ==
file_count` before `TransferComplete` (passing through `active` for empty-only sends).
Receive side reads `TransferReceiver::files_done()` once a second and also emits when only
the file count moved. `TransferBook::progress` never changes the state. Finished snapshots
stay listed for at least 60 s (`KEEP_AT_LEAST`), 1,024 hard ceiling, 10 min as before.
`ProgressReading` groups the four reading fields.

**Android** (`core/Relocations.kt` new, `DownloadsSink.kt`, `LandingMover.kt`,
`CoreHostImpl.kt`, `CoreHost.kt`, `ui/transfers/*`, `service/Notifications.kt`,
`NotificationPlan.kt`, `ui/common/Format.kt`, strings). A persistent relocation table
(`filesDir/landing-relocations.txt`, one tab-separated line per renamed file: transfer id,
promised `content://` URI, actual URI; a tombstone line forgets a key; appends are not
fsynced, one fsync per mover pass, a torn last line is cut off on load; readers go through
`ConcurrentHashMap`s with no lock). The mover records before deleting the landed file and
returns `FAILED` without deleting when it cannot record. Pruned on every `refreshTransfers()`
against the ids the core lists. The Transfers tab "Open" and the received notification
resolve through it, a single renamed file opens directly, and the final notice says "N files
were renamed because the name was taken". Rows show "1,204 of 5,000 files" while active.

**Daemon, IPC, CLI, window.** `IpcOk::ClipboardPushed.skipped: Option<ClipSkip>` (`echo`,
`no_peers`, `unsupported`, `other` catch-all; `skipped_echo` kept and derived; `docs/ipc.md`
1.4). `playdrop clip` prints the reason (exit 0 for echo, 1 otherwise); the window and the
daemon's notifications use the same sentences. CLI: width from `terminal_size`, then
`COLUMNS`, then 79; Ctrl-C armed before the first IPC call ("interrupted before the transfer
started", exit 1); progress line and `transfers` show files done. Window rows show
"n of m files".

**CI.** `.github/workflows/linux.yml` (fmt, clippy `-D warnings`, build, `dbus-run-session --
cargo test --workspace`) and `android.yml` (SDK 37, NDK 27.1, cargo-ndk, `compileDebugKotlin`
plus `testDebugUnitTest`, JUnit reports on failure). README badges. Neither had run at the
time of writing: watch the first run (section 5).

### 1.2 Checks run at integration

| Check | Result |
|---|---|
| `cargo fmt --all --check` | clean |
| `cargo clippy --workspace --all-targets -- -D warnings` | zero warnings |
| `cargo test --workspace --no-fail-fast` | 466 passed, 0 failed, 9 ignored |
| Android `:app:compileDebugKotlin`, `:app:testDebugUnitTest` | 168 passed |
| `apps/ios/scripts/check-linux.sh` | exit 0, 106 Swift tests |
| `sync-version.py --check` | in step at 0.3.0 |
| Windows type check (CLI) | `terminal_size` compiles for `x86_64-pc-windows-gnu` |

Disk note: the repo volume filled during the session (`target/debug/incremental` alone was
66 GB); it was deleted. Delete it again when space runs out; it only slows the next build.

---

## 2. Repository map (new or changed in Phase 8)

```
.github/workflows/{linux,android}.yml          new
README.md                                      badges
CHANGELOG.md                                   0.3.0 - Unreleased
docs/protocol.md                               11.1 last bullet (early placement, 409 at full size)
docs/threat-model.md                           SR-18 pointer, SR-22 Phase 8 note
docs/core-api.md                               section 2 deliver wording, section 11 (ffi 0.3.0)
docs/ipc.md                                    1.4 clip reasons, files_done, offered rule
docs/guide/{cli,clipboard,troubleshooting}.md   clip reasons, FILES column, progress line
docs/release/{sr-checklist,test-matrix,distribution}.md
packaging/linux/org.playdrop.PlayDrop.metainfo.xml   0.3.0 entry, placeholder date
crates/playdrop-core/src/transfer/{receiver,sink,sender}.rs
crates/playdrop-core/src/host/{send,receive,transfers,events,mod}.rs
crates/playdrop-core/tests/{phase8_receiver,phase8_host}.rs   new
crates/playdrop-ffi/src/{events,types}.rs      files_done; Cargo.toml 0.3.0
crates/playdrop-ipc/src/lib.rs                 ClipSkip
crates/playdropd/src/daemon.rs                 clip_skip, clip_answer, skip_notice
crates/playdrop-cli/src/daemon/{send,output,commands}.rs; Cargo.toml (+terminal_size)
apps/desktop/src/{model.rs,screens/devices.rs}
apps/android/.../core/{Relocations,DownloadsSink,LandingMover,CoreHostImpl,CoreHost}.kt
apps/android/.../ui/{transfers/*,common/Format.kt,common/Samples.kt}, service/{Notifications,NotificationPlan}.kt, res/values/strings*.xml
apps/ios/PlayDropTests/Fixtures.swift, project.yml (version)
Cargo.toml 0.3.0
```

---

## 3. Public API

| Surface | Change |
|---|---|
| `docs/core-api.md` | `HostEvent::TransferProgress.files_done`, `TransferSnapshot.files_done` (additive); `ReceiveSink::deliver` is called for a bundle member before the bundle root; send `offered` until bytes move; 60 s retention floor. `playdrop-ffi` 0.3.0 |
| Protocol | No wire change. 11.1: members placed early; a placement error after a verified root answers `409 offset_mismatch` at the full size and the sender re-plans |
| IPC | `clipboard_pushed.skipped`, `files_done` on `transfers` entries and progress events (absent from older daemons reads as 0 or absent) |
| CLI | `clip` prints a reason; `send` and `transfers` show files done; exit codes unchanged |
| Receiver on disk | `bundle.delivered` v2 header; ledger records carry the root and a reserved name while placing. A 0.2.x build reading a v2 staging folder places early members again as "name (1)" (changelog note) |

---

## 4. Decisions made in Phase 8

| Decision | Why |
|---|---|
| Place bundle members before the bundle root (protocol 7.7 deviation, user-approved) | The root is computed from the same sender-supplied chunk hashes that already verified every byte, so it adds consistency, not integrity; waiting for it serialised 10 s of sink work after the stream. A batch sink API would have cut crossings but not hidden the work behind the network |
| Eligibility at the durable offset, not the verified one | A renamed file whose data is only in the page cache could be empty after a power loss; checkpoints already fsync, so delivery lags by at most 1 s or 16 MiB |
| Restart at 0 never places a done member again | The manifest is unchanged (a changed one is `409`), so sizes and mtimes match and the restart is a sender that lost its hashes; duplicates after a daemon restart would be worse than trusting the manifest |
| Placement I/O errors answer `409` at the full size instead of `500` | Reuses the sender's existing re-plan path with no wire change; a `500` was fatal and left verified data to the 24 h sweep while holding an SR-20 slot |
| Join the worker in `Drop` with `block_in_place` | Covers every drop site, including future ones, without changing host code |
| Android relocation table keyed by promised URI, appended per file, tombstones to forget | The promised URI is what every reader holds; 5,000 appends beat 5,000 rewrites; a tombstone keeps the forget path append-only |
| Send `offered` until bytes move, no progress while offered | The CLI and UIs showed "active, 0 bytes" while the receiver decided; the state now means what it says |
| `ClipSkip` has only reasons the code can know | No `sensitive_blocked`: the host sends sensitive clips flagged, so that case does not exist |
| Linux CI wraps tests in `dbus-run-session` | The IPC round trip and some daemon code open the session bus; the runner has none |

---

## 5. Open TODOs

| File or area | What | Phase |
|---|---|---|
| `.github/workflows/{linux,android}.yml` | Never run yet. Watch the first push: clippy lint drift between Rust versions, a test that needs a desktop, apt headers for winit, Android cold-run time and disk, `setup-android` NDK install | 9 (first push) |
| `transfer/receiver.rs` | Orphan placeholder removal on append or persist failure has no automated test (no way to inject the failure); the kill window between `create_unique` and the record needs `renameat2(RENAME_NOREPLACE)` | 9 |
| `transfer/receiver.rs` | Early-placed members show `receiving` in the per-file `GET` view until the bundle verifies; `delivered()` fills only at completion while `files_done()` is live | 9 |
| `transfer/receiver.rs` | A folder handle cached by the `Placer` follows a destination folder the local user moves mid-transfer (not sender-controlled; documented) | later |
| `host/send.rs` | A receiver that keeps answering `409` at the full size (a planted symlink at the destination) ends the send through the 8-round stall limit with "keeps dropping the upload"; the message could name placement. The limit is only about 1 s of re-plans (rounds about 100 ms apart), so a placement error that clears after a few seconds (a full disk being emptied) still fails the send: give rounds without new bytes but with a verified root a time budget instead of a count | 9 |
| CI | First run on 2026-10-16: Android and iOS green; three test-only failures fixed the same day (gate B speed floor skipped for debug builds on CI, the folder-deleted test off on Windows where an open folder cannot be deleted, the placement-failure test keyed on the sender's byte counter instead of a fixed delay) | done |
| `apps/android/.../core/DownloadsSink.kt` | `moveLanded` itself has no JVM test; only the extracted `noteMove` decision is covered. A record failure leaves the file in the "could not be moved" notice until the retry timer | 9 |
| `apps/android/.../core/Relocations.kt` | The "N files renamed" count covers the current mover pass only; a power cut can lose the last appended lines (process kill is safe) | later |
| `crates/playdrop-cli` | `truncate` counts chars, not display columns (CJK labels can wrap); needs `unicode-width` | later |
| `docs/release/sr-checklist.md` | SR-32 still Partial (daemon and app logs by review); SR-34 external review and hostile-LAN run not done | 9 |
| Phone | Wi-Fi drop gate and relocation check owed (section 6) | 9 |
| `transfer/receiver.rs`, sender | Small files: `mixed-5000` at 0.43x of `medium-100`; measure the sender's per-file open and the receiver's per-member staging create and close before designing | 9 |
| Windows | Every live item from Phase 6, plus gate B in release on a real machine | when a machine exists |
| Phase 4 carry-over | Share-extension gate needs an App Group | later |

---

## 6. Live runs on the phone (2026-10-16)

Daemon `alpha` 0.3.0 from `target/release/playdropd`, release APK (debug key) installed over
the Phase 7 app with the pairing kept. Full numbers in `docs/release/throughput.md` section 9.

| Run | Result |
|---|---|
| `bench/desktop-to-phone.sh --peer SM-A546U1 --runs 3` | All 15 runs verified (counts and sha256). `mixed-5000` 21.9 to 22.6 MiB/s, wall 7.3 to 7.5 s (Phase 7: 7.2 to 9.5, 17.5 to 22.9 s); `small-1000` 31.4 to 42.6 (was 14.8 to 20.1); `medium-100` 47.5 to 54.1; `big-1g` 35.4 to 44.5; `tree` 6.9 to 15.9. The sink's delivery of 5,000 members now spans 5.9 to 6.4 s inside the 7.3 s send, at 0.45 to 0.50 ms per file; the tail after the last byte is under 1 s |
| Gate "`mixed-5000` at or above 0.8x of `medium-100`" | Not met: about 0.43 (Phase 7: 0.2). The serial delivery floor is gone; what remains is per-member work while the bytes stream (sender open and read per file, receiver staging file per member, hashing small members) |
| Wi-Fi drop mid `big-1g` | Not passed, not failed: owed. Three toggles (`svc wifi`, `cmd wifi`, 10 s and 5 s outages) each left the phone with its address and a route to the router but no path to the desktop (its ARP entry for the desktop `INCOMPLETE`, ping dead both ways), so both sends waited the 10 minute reconnect window and failed "receiver unreachable" at 859 and 486 MiB. Only reconnecting from the phone's own settings cleared it, once. The mesh isolates a rejoining client; the app's listener stayed up throughout |
| Relocation check, "x of 5,000 files" row text | Not run (the phone was isolated when they were due) |

To finish the owed gate: on a network that does not isolate a rejoining client (or with the
desktop on Ethernet to the same router), start `big-1g`, toggle Wi-Fi from the phone's quick
settings at about 40 %, and record the resume offset, the sha256 and that there is one file.
Repeat mid `mixed-5000` and count 5,000 files in `Download/PlayDrop/mixed-5000`. The
relocation and row-text checks are listed in the Android agent's notes in section 1.1.

Two failed sends left verified staging on the phone (`big-1g` at 859 and 486 MiB under
their old ids); a fresh send uses a new id and starts at 0 (sender persistence is
receiver-only), so the 24 h sweep removes them.

## 7. Phase 9 kickoff brief

First: the owed Wi-Fi drop gate and the relocation check (section 6), on a network that
does not isolate the phone after a toggle. Then from section 5 by value: the two CI workflows' first run; the sender's
stall message for a receiver stuck at placement; the Android `moveLanded` test gap. If
`mixed-5000` still sits under 0.8x of `medium-100`, measure where the per-member time goes
with the `[sink]` debug lines (staging create and close per member on the phone, hashing,
the sender's per-file open) before designing anything. `v0.3.0`: date `CHANGELOG.md` and
`metainfo.xml`, commit, tag `v0.3.0`, push the tag; the release workflow takes the notes from
the changelog section. PLAN.md row 7 items stay behind flags and each wants a design note
before code.

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
  integration: all four reviews this phase found real bugs (three blocking in the Android
  diff, three blocking in the receiver diff, one blocking in the host wiring).
- Keep `target/debug/incremental` in mind when the repo volume fills; it is safe to delete.
- `docs/core-api.md` is frozen: a change needs a note in the phase handoff and an ffi version bump.
- Every phase ends by writing `docs/phases/phase-N-<name>.md` in this same shape.

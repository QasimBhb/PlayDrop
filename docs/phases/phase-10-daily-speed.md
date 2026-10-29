# Phase 10 handoff: files in Downloads sooner, and the Windows gaps (v0.5.0)

Reader: a fresh session that starts Phase 11. Read `CLAUDE.md`, `PLAN.md` ("Build phases" row 7,
"Anti-patterns"), `docs/protocol.md` 7.6, 7.7 and 11.1, `docs/threat-model.md` SR-20, SR-28 and
SR-31, `docs/release/throughput.md` section 12, then this file. Code to read is named per item;
do not re-read the whole tree.

---

## 0. Scope as proposed at kickoff (2026-10-28)

Starting point: v0.4.0 released on 2026-10-28. The phone run that day (`throughput.md` section
11) showed a 5,000-file drop finishing on the phone in 6.9 s, with the files reaching Downloads
about 50 s later: the background mover copies each file one at a time at about 10 ms, nearly
all of it MediaProvider creating the file.

Chosen for Phase 10, speed first:

1. **Mover speed** (Android). Measure on the A54 first: today's one-at-a-time create, parallel
   creates, MediaStore batch inserts, a batched journal. Adopt the fastest that keeps every
   kill-safety property of the journaled move (no duplicate, no loss, no overwrite).
2. **Per-file receive cost on the phone** (`mixed-5000` at 0.49 of `medium-100` against the
   0.8 target). One timing pass on the receiver's per-member checkpoint and stream work; fix
   what the numbers justify within the Phase 8 and 9 durability rules, or re-set the target
   with the measured reason.
3. **Windows gaps CI can test.** Real free space for the reserve (SR-20), an exclusive bind on
   the port (SR-31), DPAPI sealing of keys and pins plus an owner-only DACL on the data dir
   (SR-28).
4. **Small fixes.** `playdrop cancel` on a transfer that already ended; gate B made robust to
   machine load; display width in CLI tables (`unicode-width`).
5. **Wi-Fi drop gate** with the user at the phone, at the end.
6. Docs, `CHANGELOG.md` 0.5.0, throughput notes, this handoff.

Not this phase: PLAN row 7 items (each wants a design note first), SR-34, the iOS App Group,
Windows items that need a real machine.

Versions: workspace `0.5.0`; `playdrop-ffi` stays `0.4.0` unless the API changes.

---

## 1. Status

| Item | Value |
|---|---|
| Base | `main` at `3927547` (v0.4.0); this phase is one commit |
| Built on | This Pop!_OS COSMIC machine, 2026-10-28 and 29, one session: scope, skeleton (versions, `windows-sys` and `windows` features, `unicode-width`), four parallel agents (Windows on Opus, CLI and gate B on Sonnet, phone receive cost on Opus, Android mover on Opus), three review rounds on the mover, then the phone runs |
| Version | Workspace `0.5.0`; `playdrop-ffi` stays `0.4.0` (the only API addition, `HostConfig.listener_setup`, is Rust-only: `docs/core-api.md` section 13) |
| Rust tests | 514 passed, 0 failed, 10 ignored (`dbus-run-session -- cargo test --workspace --no-fail-fast`, build dir on ext4); clippy `-D warnings` and `fmt --check` clean; Windows check and clippy clean with a stub C compiler |
| Android tests | 220 JUnit (up from 182); `LandedMoveDeviceTest` 9 of 9 on the A54 |
| iOS | Only `MARKETING_VERSION` changed |
| Phone | `throughput.md` section 12: mover 10-22 to 6.3-6.7 ms per file; isolated bundle receive 3.73 to 1.65 s; `mixed-5000` sent in 6.7 to 7.3 s and in Downloads +30 to 32 s on Wi-Fi, against 0.4.0 Kotlin 6.9 to 8.1 s and +48 to 50 s in the same interleaved A/B (12.5) |

### 1.1 What landed

**Android mover** (`DownloadsSink.kt`, `LandedMove.kt`, `LandingMover.kt`, `LandingTree.kt`,
`NameClaims.kt`; tests `LandedMoveBatchTest`, `LandingTreeTest`, `NameClaimsTest`,
`androidTest/LandedMoveDeviceTest`, opt-in `androidTest/MoverProbe`).

- Batches of 100: journal a tagged entry per file, insert published `MediaStore.Downloads` rows
  in one call (tag `playdrop-move:<uuid>:<k>` in `TITLE`), read back place, name and title, journal
  each row's URI, copy on a 4-thread pool into each row opened "rw" after checking it is empty,
  then per file record the relocation, delete the landed file, clear the journal.
- Anything the provider would rename, any place that already has a row (case-insensitive), any
  read-back mismatch, and any non-empty target goes the one-file `O_EXCL` way under the folder
  lock. A failed insert turns batching off for that pass only.
- Recovery (`JournalRecovery`) settles a multi-entry, append-only journal: by URI first, through
  the file system only when the row is published, ours and `DATA` is its exact place
  (`RowUnlink`), else through the provider; then by tag, owner-scoped; never touches a row whose
  file is larger than the landed file. The 0.4.0 one-entry journal is still read.
- `NameClaims` keys by the core's case fold; `deliverToMediaStore` picks its name under the
  folder lock. The batch path no longer asks the media scanner for files MediaProvider scans
  itself on close.
- `UpdateReceiver` (`service/AutoStart.kt`): after an app update the service starts again by
  itself (found when a reinstall left the phone offline for 50 minutes).

**Receive checkpoint** (`transfer/receiver.rs`, `fsx.rs`): every 64 MiB or 1 s where the bundle
staging folder is flushed with one `syncfs`, 16 MiB or 1 s elsewhere (user-approved; protocol.md
7.6 and 11.1). Finer timing hooks and `PD_BENCH_RUNS`, `PD_BENCH_SYNC_MIB` in the bench.

**Windows** (`playdropd/src/{free_space,at_rest,listener}.rs`, `daemon.rs`, `settings.rs`;
`net/server.rs` hook): real free space through `GetDiskFreeSpaceExW` (SR-20); DPAPI sealing of
`identity.key` and `pins.json` plus a protected user-and-SYSTEM DACL on the data dir, re-applied
at every start (SR-28); `SO_EXCLUSIVEADDRUSE` through `HostConfig.listener_setup`, so the core
keeps `forbid(unsafe_code)` (SR-31). Every Windows-only test runs for the first time in CI.

**CLI**: `playdrop cancel` on an ended transfer says "already complete" or "already ended
(<state>)" and exits 0; tables and the progress line pad by display width; gate B is the best
of up to 3 interleaved rounds (passes on any round meeting the floor).

---

## 2. Repository map (new or changed)

```
Cargo.toml, Cargo.lock                       0.5.0; windows-sys WinSock; windows Cryptography, WinSock; unicode-width
CHANGELOG.md                                 0.5.0
docs/protocol.md 7.6, 11.1; docs/core-api.md 13; docs/guide/cli.md; docs/release/throughput.md 12
crates/playdrop-core/src/{host/mod,host/platform,identity/secure_fs,net/mod,net/server}.rs
crates/playdrop-core/src/transfer/{receiver,fsx}.rs
crates/playdrop-core/tests/{phase1_gates,phase8_receiver,phase9_receiver}.rs
crates/playdropd/src/{at_rest,free_space,listener}.rs (new), daemon.rs, main.rs, settings.rs
crates/playdrop-cli/src/daemon/{commands,output,send}.rs
apps/android/app/src/main/AndroidManifest.xml, core/{DownloadsSink,LandedMove,LandingMover,LandingTree,NameClaims}.kt
apps/android/app/src/main/kotlin/org/playdrop/android/service/{AutoStart.kt (new),BootReceiver.kt}
apps/android/app/src/test/... LandedMoveBatchTest, AutoStartTest (new), LandedMoveTest, LandingTreeTest, NameClaimsTest
apps/android/app/src/androidTest/... LandedMoveDeviceTest, MoverProbe (new)
apps/ios/project.yml                         MARKETING_VERSION 0.5.0
```

---

## 3. Public API

| Surface | Change |
|---|---|
| `docs/core-api.md` | Section 13: `HostConfig.listener_setup` (Rust-only, not in `playdrop-ffi`); `ListenerSetup`, `ListenSocket` re-exported from `host` |
| Protocol | No wire change. 7.6 cadence where `syncfs` is used; 11.1 Phase 10 bullet |
| IPC | None |
| CLI | `cancel` wording and exit 0 on an ended transfer |
| On disk | Windows: sealed `identity.key`, `pins.json` (one-way; 0.4.0 refuses them). Android: multi-entry mover journal (0.4.0 one-entry format still read) |

---

## 4. Decisions made in Phase 10

| Decision | Why |
|---|---|
| 64 MiB checkpoints only where one `syncfs` flushes the folder | `syncfs` costs about 110 ms per call on the A54 whatever the size; per-file flushes scale per member and keep 16 MiB. A kill re-sends at most 64 MiB |
| Batched published rows, not parallel creates or pending rows | MediaProvider serialises creates; pending rows cost 10 ms each to publish (12.2) |
| Recovery by tag and URI, never by name | Name-based recovery could miss renamed rows or remove another PlayDrop file of the same name (review rounds 1 and 2) |
| Never write into or delete a non-empty row | A published row has no file until the copy; another writer could take the place in that window (review round 3) |
| Windows pieces live in the daemon behind hooks | The core forbids `unsafe`; the same pattern as `lock_state`, `free_space`, `secret_store` |
| The Wi-Fi drop gate is closed as "not testable on this network" | The mesh isolates a rejoining phone from the PC every time (Phase 8, and twice on 2026-10-29); with Phase 9 a failed send resumes when the phone is back |

---

## 5. Open TODOs

| Area | What | Phase |
|---|---|---|
| Phone | `mixed-5000` against `medium-100` stays about 0.5 to 0.6 over Wi-Fi (target 0.8); the floor is per-member work on the phone (`throughput.md` 12.1, 12.5). A morning of 13 to 15 s sends was the phone's state (cleared by a reboot), not the code | 11 |
| Phone | USB tethering wins the dial race when it is on and caps sends at about 27 MiB/s; prefer the faster path, or Wi-Fi when both answer | 11 |
| Windows | First CI run of the DPAPI, DACL, free-space and exclusive-port tests; `playdrop host` on Windows gets none of the daemon's Windows pieces | 11 |
| Windows | The data dir no longer grants Administrators; check an elevated uninstaller on a real machine | when a machine exists |
| Android | A direct create of a name with `: ? * < > \| " \` gets `EPERM` and turns direct writes off for the process (unreachable from peers: the core rejects those names) | later |
| Earlier phases | `unicode-width` done; SR-34, iOS App Group, Windows live items | later |

---

## 6. Live runs

`throughput.md` section 12. The desktop ran with load averages from 1 to over 100 during the
night (an NTFS link stall), so read only the runs noted as quiet. Wi-Fi drop: the phone, rebooted
or toggled, could reach the router but not the PC for over 10 minutes each time.

---

## 7. Phase 11 kickoff brief

Start with the first Windows CI results for the new tests (the release run of v0.5.0 is the
first time they run). Then by value: path choice when USB tethering and
Wi-Fi both answer; PLAN.md row 7 items each want a design note before code. Build with
`CARGO_TARGET_DIR=$HOME/.cache/playdrop-target` (the repo's drive is NTFS through ntfs-3g).

---

## 8. Rules carried forward

- Hyphen only. Never the long dash, in code, comments, docs or commits.
- No AI attribution anywhere: no co-author trailers, no "generated with" lines, no session links.
- Parallel agents never edit any `Cargo.toml`, Gradle file, `Package.swift` or `project.yml`.
- Never bind or announce on 1714-1764, 53317 or 224.0.0.167.
- Manual runs use data and receive dirs on ext4 (`/tmp/pd-*`), never under the repo.
- Never judge phone speed from a debug APK; check `ip route` and `ss -tn` for which link a phone
  run actually used (USB tethering wins when it is on).
- Phone work from agents goes under `flock /tmp/pd-phone.lock`; never `pkill -f` a pattern that
  matches the calling shell.
- Subagents: Opus for core and risky Android, Sonnet for apps, scripts and docs, Haiku for
  lookups; every diff goes through an independent reviewer. The mover needed three rounds.
- `docs/core-api.md` is frozen: a change needs a note and an ffi version bump (Rust-only
  additions excepted, as section 13 records).
- Every phase ends by writing `docs/phases/phase-N-<name>.md` in this same shape.

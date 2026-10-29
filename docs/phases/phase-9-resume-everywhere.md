# Phase 9 handoff: resume everywhere and the small-file floor (v0.4.0)

Reader: a fresh session that starts Phase 10. Read `CLAUDE.md`, `PLAN.md` ("Build phases" row 7,
"Anti-patterns"), `docs/protocol.md` 7.6 and 11.1, `docs/threat-model.md` SR-21 and SR-28,
`docs/core-api.md` section 12, `docs/phases/phase-8-fast-bulk.md` sections 5 to 7, then this
file. Code to read is named per item; do not re-read the whole tree.

---

## 0. Scope as proposed at kickoff (2026-10-17)

Starting point: v0.3.0 was tagged on 2026-10-17 but its release run failed twice (Flatpak
crate list one crate short; Windows kill-and-resume test lost a race in `Host::start`). Both
are fixed first, as their own commit, before any Phase 9 code.

Chosen for Phase 9, by value to daily use, speed first:

1. **Sender persistence and automatic resume** (protocol.md 7.6 "sender persistence", the
   `docs/core-api.md` section 8 exclusion, the Phase 8 Wi-Fi finding). The sender keeps a
   record of every outgoing transfer (id, manifest, sources, chunk hashes) under
   `<data_dir>/outgoing/`. A send that fails because the receiver is unreachable keeps its
   record; it resumes by itself when that peer's session comes back (desktop, path sources),
   when the sending host restarts (desktop), or when the same files are sent to the same
   device again within 24 h (every platform, including Android share targets whose openers
   die with the process). A receiver that no longer knows the id (its 24 h sweep, a wiped
   folder) gets the same manifest offered again under the same id. Cancelling a failed send
   drops its record. The reconnect window becomes a `HostConfig` field (default unchanged,
   10 minutes). Nothing changes on the wire.
2. **The small-file floor, measured then cut** (Phase 8 section 5: `mixed-5000` at 0.43x of
   `medium-100`). Loopback timing of the receiver's per-member work (staging file create and
   close on the stream thread, placeholder create plus rename on the delivery thread) and
   the sender's per-member opens, then the fixes those numbers justify. Known candidates
   from reading the code: a no-replace rename instead of the placeholder file (also closes
   the kill window noted in Phase 8 section 5), one blocking call per small member on the
   sender instead of four pool hops. Durability rules from Phase 8 are kept.
3. **Android `moveLanded` test** (Phase 8 section 5): the mover's move, record and delete
   order under a fake resolver, including the record-failure branch.
4. **Docs, CLI and window wording** for a send that will resume, `CHANGELOG.md` 0.4.0,
   `docs/core-api.md` section 12, protocol.md 11.1, the SR checklist, this handoff.

Not this phase: PLAN row 7 items (iroh, Local Push Connectivity, Wi-Fi Aware, libcosmic
applet, MSIX), SR-34, every Windows live item, the iOS App Group, `unicode-width` in the
CLI. The Wi-Fi drop live gate stays with the user: it needs the phone on a network that does
not isolate a rejoining client, and item 1 is what makes a failed attempt there recoverable.

Versions: workspace `0.4.0`, `playdrop-ffi` `0.4.0` (additive `resumable` field, section 1),
iOS `MARKETING_VERSION` synced by script.

---

## 1. Status

| Item | Value |
|---|---|
| Base | `main` at `f366bf0` (v0.3.0 tagged); this phase is two commits: the v0.3.0 release hotfix, then Phase 9 |
| Built on | This Pop!_OS COSMIC machine, 2026-10-17 to 2026-10-22, one session paused once at the usage limit: scope proposal, hotfix, skeleton (versions, `windows-sys`, stubs), three parallel agents (sender persistence and small-file floor on Opus, Android mover tests on Sonnet), then one apps agent (Sonnet); every diff reviewed, the sender four times |
| Version | Workspace `0.4.0`, `playdrop-ffi` `0.4.0` (additive `TransferSnapshot.resumable`, `docs/core-api.md` section 12), iOS `MARKETING_VERSION` synced by script |
| Rust tests | 508 passed, 1 failed (gate B, load; section 5), 10 ignored at the last run; 509 of 509 in the run before (`dbus-run-session -- cargo test --workspace --no-fail-fast`, up from 466); clippy `-D warnings` and `fmt --check` clean |
| Android tests | 182 JUnit (up from 168) |
| iOS checks | `apps/ios/scripts/check-linux.sh` exit 0; 106 `PlayDropShared` tests; new `TransferRowsTests.swift` type-checks, runs only on a Mac |
| Release | v0.3.0 released on 2026-10-26 from the hotfix commit (the tag was re-pointed). v0.4.0 tagged at `3927547` and released on 2026-10-28, five files plus checksums |
| Phone | Run on 2026-10-28 after the push (`throughput.md` section 11): `mixed-5000` 6.9 s at 24 MiB/s (Phase 8: 7.3 s, 22), `big-1g` 46 to 48 MiB/s, all sums verified; kill-and-resume of a 1 GiB send across a daemon restart passed on the real link. The Wi-Fi drop gate is still owed |

### 1.1 The v0.3.0 release hotfix (first commit)

- Flatpak job: `cargo-sources.json` had not been regenerated since 2026-10-13, so `terminal_size`
  (added in 0.3.0) was missing and the offline build stopped. Regenerated;
  `packaging/flatpak/check-sources.py` now fails `linux.yml` on every push touching
  `Cargo.lock` and the release job before the Flatpak build; `update-sources.sh` falls back to
  running the generator inside the Freedesktop SDK (this machine has no pip, venv or uv).
- Windows job, `gate_a_kill_and_resume_500mib`: `Host::start` spawned the server before
  `receive::load_persisted`, so a sender reconnecting at once got `404` for a transfer the
  restarted receiver had not reloaded yet. Persisted transfers and outbox items now load first.

### 1.2 What landed in Phase 9

**Sender persistence and automatic resume** (`host/send.rs`, `host/send_store.rs`,
`host/send_resume.rs`, `host/receive.rs`, `host/transfers.rs`, `host/mod.rs`,
`transfer/sender.rs`; `tests/phase9_send.rs`, 16 tests; spec in core-api section 12 and the
protocol.md 11.1 bullet).

- Record per outgoing send under `<data_dir>/outgoing/`: `<id>.json` (peer, manifest, sources
  without openers, `publicId`) written once; `<id>.state` (chunk size, hash counts, committed
  hash-log length, verified flags, failure kind, re-offer and auto-resume counters) rewritten
  atomically and the commit point; `<id>.hashes` append-only 40-byte entries, cut back to the
  committed length before each append; `<id>.tomb` for cancels not yet delivered. 0600 in a
  0700 folder. In-memory index loaded before the server accepts; full records read only for
  candidates, outside the lock.
- Resume triggers: "receiver unreachable" failures resume when the peer's session comes back
  (path sources only, at most 3 automatic resumes per record); unfinished path records restart
  at host start; any `send_sources` for the same peer and matching sources within 24 h adopts
  the record (the phone case). A stall failure only resumes on a manual re-send.
- A send keeps one local id for its whole life (handle, events, snapshots, `cancel_transfer`,
  records). When a re-sent transfer finds the receiver ended the old one (cancelled, declined,
  failed, expired), it continues under a new wire id; only the offer, HTTP paths and DELETEs
  use the wire id.
- Receiver keeps an end-state record per ended transfer for 24 h and answers `GET`, `PUT`,
  `POST` and `DELETE` for it, so a sender that missed the end never sends the files again. A
  `404` re-offers the same manifest under the same id (at most 3 times, verified flags
  cleared), or counts as complete when every stream was verified.
- Lost hashes below the receiver's offset are rebuilt by hashing the local source, outside the
  sender lock, cancellable, then compared with the receiver's prefix (SR-21). Stream records
  with an unknown mtime drop their hashes on adoption and rebuild from the new handle.
- `TransferSnapshot.resumable`; waiting sends stay listed (also after a restart) as failed and
  resumable with the note "resumes when <device> is back or when these files are sent to it
  again"; `cancel_transfer` forgets them and leaves a tomb when the receiver cannot be told.
  Hourly maintenance expires records and tombs after 24 h. `HostConfig.reconnect_for`.

**Small-file floor** (`transfer/receiver.rs`, `transfer/fsx.rs`, `transfer/bundle.rs`;
`tests/phase9_receiver.rs` ignored bench; numbers in `throughput.md` section 10).

- Android flushed every bundle member with its own `fdatasync`: the batched `syncfs` was gated
  on `target_os = "linux"`. Now `fsx::Flush::for_dir` uses `syncfs` when `fstatfs` says ext2/3/4,
  f2fs, xfs or btrfs, per-file elsewhere (sdcardfs on Android 8 to 10 returns success from
  `syncfs` without flushing). Phone bundle receive: 7.0 to 5.3 s Staged, 9.4 to 4.6 s Directory.
- Placement: delivery log version 3 records the intended name first, then a no-replace rename
  (`RENAME_NOREPLACE`, `RENAME_EXCL` on Apple); " (n)" lines are flushed before their rename;
  where the flag is unavailable (Windows, some FUSE and network mounts) the line is marked
  `reserved` and the 0.3.0 placeholder path runs. v1 and v2 logs from earlier builds are
  finished under their own rules and rewritten as v3. Single files use the same rules.
- Sender reads members under 256 KiB in one blocking call (`bundle::open_source_member`).
  `BundleWriter` no longer returns bytes and an error from the same read (tokio contract).
- `TransferReceiver::delivered()` lists members as they are placed.

**Apps.** `playdrop cancel <id>` (exact or unique prefix; exits 0, 1, 2, 3 as in
`docs/guide/cli.md`); `playdrop transfers` shows `failed (resumes)`; a Forget button on
resumable failed send rows in the window, Android and iOS; `docs/ipc.md` 1.5.

**Android tests.** `DownloadsSink.moveLanded` moved unchanged into `LandedMove` behind a
`DownloadsPlacer` seam; `MoveJournal.settle` extracted; 13 JVM tests cover the move order,
every failure branch and kills between steps. The real placer runs only on a device.

---

## 2. Repository map (new or changed)

```
.github/workflows/{linux,release}.yml        crate list check
packaging/flatpak/{cargo-sources.json,check-sources.py,update-sources.sh,README.md}
CHANGELOG.md                                 0.4.0, 0.3.0 Fixed note
docs/protocol.md 11.1, docs/threat-model.md SR-18, docs/core-api.md 12, docs/ipc.md 1.5
docs/guide/{sending,troubleshooting,cli}.md, docs/release/{throughput,sr-checklist}.md
crates/playdrop-core/src/host/{send,send_store,send_resume,receive,transfers,session,outbox,mod}.rs
crates/playdrop-core/src/transfer/{receiver,fsx,bundle,sender,manifest,mod}.rs
crates/playdrop-core/tests/{phase9_send,phase9_receiver}.rs (new), common/mod.rs
crates/playdrop-core/Cargo.toml              windows-sys for cfg(windows) (unused: Windows keeps the placeholder)
crates/playdrop-ffi/{Cargo.toml,src/types.rs}
crates/playdrop-cli/src/{main.rs,daemon/{mod,commands,output,send}.rs}
apps/desktop/{src/model.rs,src/screens/{mod,transfers}.rs,ui/{types,globals,transfers}.slint}
apps/android/.../core/{LandedMove.kt (new),DownloadsSink.kt,LandingTree.kt}, ui/transfers/*, ui/common/Samples.kt, res/values/strings_ui.xml, tests
apps/ios/.../TransferRows.swift, TransferRowView.swift, TransfersStrings.swift, PlayDropTests/*, project.yml
Cargo.toml 0.4.0
```

---

## 3. Public API

| Surface | Change |
|---|---|
| `docs/core-api.md` | Section 12: `HostConfig.reconnect_for`, `TransferSnapshot.resumable`, sender resume behaviour, `cancel_transfer` on a waiting send. `playdrop-ffi` 0.4.0 |
| Protocol | No wire change. 11.1: sender persistence; placement without a placeholder |
| IPC | `resumable` on snapshots (absent reads as false); `cancel_transfer` forgets a waiting send |
| CLI | `playdrop cancel <id>`; `failed (resumes)` state |
| On disk | `outgoing/` records (new); `incoming/<id>.json` now kept 24 h after a transfer ends; delivery log v3 (0.3.0 reads it as "no log": downgrade mid-transfer may place members again as " (n)", in the changelog) |

---

## 4. Decisions made in Phase 9

| Decision | Why |
|---|---|
| Fix the v0.3.0 release first, as its own commit | The tag pointed at a commit whose release run failed; nothing was published |
| Resume by re-sending the same files, not by persisting share handles | Android share URI grants die with the process; matching sources by name, size, mtime and file id is what both platforms can do |
| Only "receiver unreachable" auto-resumes, capped at 3 per record | A receiver that refuses or stalls every time would otherwise fail a send again at every session for 24 h |
| A host-restarted send that finds the receiver ended the transfer just ends; a user re-send continues under a new wire id | Never re-offer files the other user explicitly cancelled without someone asking |
| One local id for the life of a send | Every client tracks the send by the handle's id; a new wire id must not be visible locally |
| Receiver keeps end-state records 24 h | A 404 must not mean "send everything again" after a receiver restart |
| `syncfs` only on known local filesystems | sdcardfs and FUSE return success without flushing; the Phase 8 durable-offset rule depends on a real flush |
| Log line before the no-replace rename | One rename per member and no empty placeholder after a kill; " (n)" lines flushed first so recovery never names another file |
| Windows keeps the placeholder path | No handle-relative no-replace rename through `cap-std`; untestable without a Windows machine |
| `syncfs` and `renameat2` need no API gate | Both are in bionic's app seccomp allowlist from API 26 (checked by the receiver review) |

---

## 5. Open TODOs

| Area | What | Phase |
|---|---|---|
| Phone | Wi-Fi drop gate (Phase 8 section 6) still owed, on a network that does not isolate a rejoining client; with Phase 9 a failed attempt now resumes when the phone is back | 10 |
| Phone | The `mixed-5000` against `medium-100` gate (0.8) reads 0.49 end to end (`throughput.md` 11.2); the floor is per-member work on the phone, about 0.85 ms per member in Staged mode. Decide whether to chase it (batch the checkpoint further, or write members straight into the landing tree) or re-set the gate | 10 |
| CI | `gate_b_many_small_files_hold_throughput` (debug ratio 0.5) is load-sensitive here. At the end of the phase, with a browser and other work on the machine, v0.3.0 and this tree both failed it in alternating runs (v0.3.0 0.44 and 0.50, Phase 9 0.45 to 0.47; small files 199 to 204 MiB/s against 181 to 196). Watch the first CI run, and measure small-file speed in release on a quiet machine before calling the 5 to 10 % a regression | 10 |
| Windows | Single-file and bundle placement keep the placeholder path; a no-replace rename through `MoveFileExW` needs a real machine (`windows-sys` is already a dependency) | when a machine exists |
| Receiver | Renames to the manifest name inside a 256-member batch can still outlive their log line on power loss (recovery then finds the member by name and size); closing it costs one fsync per member | later |
| Sender | Records rewrite the hash log from scratch when it holds more than twice the valid entries plus 4096; fine below 100 GiB sends | later |
| CLI | `playdrop cancel` on a transfer that already finished: check whether core answers not found or ok | 10 |
| Android | The real `DownloadsPlacer` (descriptor close, MediaStore fallback, renumber rule) is exercised only on a device | later |
| Earlier phases | `unicode-width` in the CLI, SR-34, iOS App Group, every Windows live item | later |

---

## 6. Live runs

None end to end. The phone was used only for the receiver benchmark through `adb shell`
(`throughput.md` section 10). The machine was heavily loaded by parallel agent builds through
most of the session (load average often above 30), so every speed figure here is relative.

---

## 7. Phase 10 kickoff brief

The phone bench and the kill-and-resume check were run on 2026-10-28 (`throughput.md` section
11) and the first CI runs are green on every platform after `4d5f0af` (two Windows-only test
fixes: a staging-folder cleanup that retries while Windows still holds the folder, and the
collision test hook planting its file on the placeholder path too). Still owed and needing the
user at the phone: the Wi-Fi drop gate. Then from section 5 by value. PLAN.md row 7 items stay behind flags
and each wants a design note before code.

---

## 8. Rules carried forward

- Hyphen only. Never the long dash, in code, comments, docs or commits.
- No AI attribution anywhere: no co-author trailers, no "generated with" lines, no session links.
- Agents do not commit or push. The user commits and runs the push commands.
- Parallel agents never edit any `Cargo.toml`, Gradle file, `Package.swift` or `project.yml`
  (the ffi version line was the one exception this phase, by brief).
- Never bind or announce on 1714-1764, 53317 or 224.0.0.167.
- Manual runs use data and receive dirs on ext4 (`/tmp/pd-*`), never under the repo.
- Never judge phone speed from a debug APK.
- Subagents: Opus for core, Sonnet for apps, scripts and docs, Haiku for lookups; every diff
  goes through a reviewer. This phase the sender work needed four review rounds (two blocking
  bugs in the first, one in the third), the receiver two.
- Avoid running speed gates while other agents build; record the load average with any number.
- `docs/core-api.md` is frozen: a change needs a note and an ffi version bump.
- Every phase ends by writing `docs/phases/phase-N-<name>.md` in this same shape.

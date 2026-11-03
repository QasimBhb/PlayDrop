# Phase 11 handoff: the faster path, and small files on the phone (v0.6.0)

Reader: a fresh session that starts Phase 12. Read `CLAUDE.md`, `PLAN.md` ("Build phases" row 7,
"Anti-patterns"), `docs/protocol.md` 3.3 to 3.5, `docs/design/later-items.md`,
`docs/release/throughput.md` section 13, then this file. Code to read is named per item; do not
re-read the whole tree.

---

## 0. Scope as proposed at kickoff (2026-10-29)

Starting point: v0.5.0 released on 2026-10-29.

1. **Path choice** when USB tethering and Wi-Fi both answer (protocol 3.5).
2. **The small-file critical path on the phone** (`mixed-5000` at 0.8 of `medium-100`).
3. **Windows `playdrop host`** gets the daemon's Windows hooks.
4. **A design note** for the PLAN row 7 items.
5. Docs, `CHANGELOG.md` 0.6.0, throughput section 13, this handoff, release.

Versions: workspace `0.6.0`; `playdrop-ffi` keeps its own version, its API did not change.

---

## 1. Status

| Item | Value |
|---|---|
| Base | `main` at `80d0272` (v0.5.0); this phase is one commit |
| Built on | This Pop!_OS COSMIC machine, 2026-10-29 to 2026-11-02, one session interrupted by a pause and two reboots: an Opus agent on path choice (five review rounds), an Opus agent on the phone path (three review rounds), Sonnet on the design note |
| Rust tests | 586 passed, 0 failed, 11 ignored (`dbus-run-session -- cargo test --workspace --no-fail-fast`, build dir on ext4); clippy and `fmt --check` clean; Windows check clean with a stub C compiler; Flatpak `check-sources` in step |
| Android tests | 226 JUnit passed, 0 failed |
| iOS | Only `MARKETING_VERSION` changed |
| Release | v0.6.0 from `cee48dc` on 2026-11-02, five files plus checksums; Linux, Windows, iOS and Android CI green |
| Phone | `throughput.md` 13.4: `mixed-5000` about half the time of 0.5.0, target of 0.8 missed, all sums verified |

### 1.1 What landed

**Path choice** (`host/path.rs`, `net/probe.rs`, `tests/phase11_path.rs`; protocol 3.3, 3.4, 3.5).
A dial returns its first winner and holds the other candidates for a check. The dialler measures with
`GET /v1/probe` (128 KiB estimate, then 4 MiB) and moves to a path at least 1.25 times faster, only
when idle. Replaced connections drain for 10 s and are PINGed first, so a dead one closes within 1 s
(`Connection::heard_from`). A session moves in two steps with no offline flicker. Back-off doubles to
160 minutes; no check on low battery or while asleep, and a skipped check is owed for later.

Late fix, found by reading the code after the phone runs: `busy` ignored a transfer the peer was
delivering here, so a phone could probe on the same radio during a receive. `busy` now includes
`receive::receiving_from` (accepted, and active within 30 s), tested by
`a_receive_in_progress_blocks_a_path_check`, which fails without the fix. The speed rows in
`throughput.md` 13.4 were measured before it.

**Small files on the phone** (`transfer/receiver.rs`, `transfer/sink.rs`; Android `DownloadsSink.kt`,
`NameClaims.kt`, `LandingPolicy.kt`, `CallbackThreads.kt`; `throughput.md` 13.1 to 13.6). 4 sink
threads, background flush from a snapshot, `flush_failed` back to the durable offset, `trim_member`
shared with `reconcile`. On Android, name claims in 256 slots, folder listings, callback threads kept
attached to the JVM. Write-ahead of bundle chunks was tried and dropped: no gain.

**Windows host** (`crates/playdrop-platform`). `at_rest`, `free_space` and `listener` moved out of
`playdropd`; `apply_hooks` and `prepare_data_dir` are used by both hosts.

**Design note** (`docs/design/later-items.md`). Verdict for each row 7 item; iroh is next, as a
flagged spike.

---

## 2. Repository map (new or changed)

```
crates/playdrop-platform/*                   New: Windows hooks shared by both desktop hosts
crates/playdrop-core/src/host/path.rs        New: when to check, probing, deciding, switching
crates/playdrop-core/src/net/probe.rs        New: GET /v1/probe, client and server
crates/playdrop-core/tests/phase11_path.rs   New: 48 tests, proxies with throttles and gates
crates/playdrop-core/src/net/*               Held candidates, drain, heard_from, in-use rule
crates/playdrop-core/src/host/{session,connect,router,registry,send,receive,status,mod}.rs
                                             Session hand-over, wiring, reports, receiving_from
crates/playdrop-core/src/transfer/*          Sink threads, background flush, flush_failed
apps/android/.../core/*                      NameClaims, LandingPolicy, DownloadsSink, CallbackThreads
bench/desktop-to-phone.sh                    --playdrop
docs/protocol.md, threat-model.md, core-api.md, release/throughput.md section 13, design/later-items.md
```

---

## 3. Public API

`Host::path_reports` and `HostConfig.path_timing` are Rust-only, as is `network_changed`. The daemon's
Diagnose summary prints "note: path to ...". `ReceiveSink` calls may now overlap, and up to four files
may be delivered again after a kill (`docs/core-api.md`). No ffi change.

---

## 4. Decisions made in Phase 11

- The dialler measures; interface types decide nothing (phones hide them, and USB 3 can beat poor Wi-Fi).
- After a silent path loss the first frames on the new side arrive up to 1 s late, none lost.
- A kill can leave up to 4 visible duplicates such as `a (1).txt`, accepted for the speed (`throughput.md` 13.6).
- The 0.8 target stays unmet and on record, not re-set: 0.38 to 0.53 in fast rounds.
- The user cut the final phone testing to two small files to save time. So `big-1g` and force-stop
  resume on the final build were not run (`throughput.md` 13.5, 13.7).

---

## 5. Open TODOs

| Area | What | Phase |
|---|---|---|
| Phone | **Owed: path choice on a real phone with USB tethering and Wi-Fi both on.** Only proxies on one machine so far | next |
| Phone | **Owed: force-stop resume on this build.** Desktop tests cover the fixes; the last phone runs used one delivery thread | next |
| Phone | **`big-1g` unexplained.** Old beat new in the fast 15:33 rounds (22.1 s against 27.3 s), not in the 17:33 pair (24.7 against 23.7). The `busy` fix above may or may not bear on it. Measure interleaved | next |
| Phone | Slow rounds on both apps, not explained by desktop load; fits phone warmth, not proven (`throughput.md` 13.5) | later |
| Network | Phone and desktop lost each other twice in one day on the first `mixed-5000` after an install, no toggle, cleared by rejoining from Settings. Desktop to router read 84 to 200 ms then. Cause unknown; whether PlayDrop plays a part is unknown (`throughput.md` 13.7) | watch |
| Core | One radio on three addresses (link-local, mapped, plain IPv4) is probed three times per check, up to 4 MiB each. Treat them as one path | next |
| Core | `mixed-5000` target of 0.8 unmet; what is left is listed in `throughput.md` 13.5 | later |
| Core | A check on the phone's side cannot be read in its release log; the one-path check covers the daemon's side only | later |
| Tests | `long_clipboard_text_arrives_through_a_blob` times out rarely: 1 of 600 here, 4 of 600 on v0.5.0, unexplained | later |
| Tests | The pin check before attach in `switch` is not caught by a test (mutation run) | later |
| Core | Older race: a cancel can catch a just-started re-send (`host/send.rs`). A stale `accepted` after `forgotten` (`host/send.rs`) is practically unreachable, guard not added | later |
| Earlier phases | SR-34, iOS App Group, Windows live items, elevated uninstaller check on a real machine | later |

---

## 6. Live runs

`docs/release/throughput.md` section 13. Bench scaffolding is outside the repo in `~/.cache/pd-p11/`
(two trees, scripts, raw rows in `ab/final.txt` and `ab/final2.txt`); alpha's data dir is
`~/.cache/pd-p11/alpha`, moved off `/tmp` because reboots wiped its identity.

---

## 7. Phase 12 kickoff brief

v0.6.0 is released. First clear the three "owed" phone rows in section 5, which need the user to turn
tethering on for the first. Then treat one radio's addresses as one path. Then pick from
`docs/design/later-items.md`, where iroh for cross-network reach is the recommended next build, as a
flagged spike. Build with `CARGO_TARGET_DIR=$HOME/.cache/playdrop-target` (the repo's drive is NTFS
through ntfs-3g).

---

## 8. Rules carried forward

- Hyphen only. Never the long dash, in code, comments, docs or commits.
- No AI attribution anywhere: no co-author trailers, no "generated with" lines, no session links.
- Parallel agents never edit any `Cargo.toml`, Gradle file, `Package.swift` or `project.yml`.
- Never bind or announce on 1714-1764, 53317 or 224.0.0.167.
- Manual runs use data and receive dirs on ext4, never under the repo; `/tmp` does not survive a reboot.
- Never judge phone speed from a debug APK; check `ip route` and `ss -tn` for which link a phone run
  actually used (USB tethering wins the dial when it is on).
- Phone work from agents goes under `flock /tmp/pd-phone.lock`, and a daemon started inside it inherits
  and holds the lock, so start daemons outside. Never `pkill -f` or `pgrep -f` a pattern that matches
  the calling shell. Never toggle Wi-Fi over adb.
- A/B old against new, interleaved, in one sitting. Do not run load tests during a phone sitting.
- Subagents: Opus for core and risky Android, Sonnet for apps, scripts and docs, Haiku for lookups;
  every diff goes through an independent reviewer. Path choice needed five rounds.
- `docs/core-api.md` is frozen: a change needs a note and an ffi version bump (Rust-only additions
  excepted).
- Every phase ends by writing `docs/phases/phase-N-<name>.md` in this same shape.

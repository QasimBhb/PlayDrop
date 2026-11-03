# Throughput notebook

Phase 6 ship gate. PLAN.md "Speed targets" asks for: at least 50% of `iperf3` on the same
link for one large file (80% stretch), no drop between 100 and 5,000 files, and a published
speed matrix against LocalSend and KDE Connect. This file holds the matrix, the method and
the pass/fail verdicts. Every number below was measured on the date shown with the command
shown; a cell that says "to run" has not been measured, and "not run" means the user
chose not to measure it (the incumbents, see section 5 item 6).

**Headline (2026-10-13): the numbers do not meet the targets yet, and the app measured
was a debug build.** Read "Caveats" before quoting anything.

## 1. Method

Scripts are in `bench/` (`bench/README.md` explains each). Datasets are generated, seeded
and checked by sha256 manifest, so another machine gets the same bytes.

| Dataset | Files | MiB | Notes |
|---|---|---|---|
| `big-1g` | 1 | 1024.0 | one file |
| `small-1000` | 1,000 | 97.7 | 100 KiB each |
| `mixed-5000` | 5,000 | 165.2 | 4 to 64 KiB, seeded sizes |
| `medium-100` | 100 | 168.8 | 1.0 to 2.3 MiB; the same bytes as `mixed-5000` with 1/50 of the files |
| `tree` | 160 | 15.6 | 4-level folder tree, 3 empty folders |

- PlayDrop desktop to phone: `bench/desktop-to-phone.sh`. Files go through the running
  daemon (`SendFiles` with file descriptors). Wall time is from just before the call to
  the first 100 ms poll that sees the transfer complete, so it includes manifest, hashing
  on both ends and the phone's MediaStore writes. After every run the script counts the
  files on the phone and compares the sha256 of each one with the manifest. A run without
  "all sums match" is a failure.
- Baseline: `bench/iperf3-phone.sh`, `iperf3 -c <phone> -t 10` and with `-R`, 1 and 4
  streams, 3 runs each. One stream is the fair comparison for PlayDrop (one TCP connection).
- Loopback: `bench/loopback.sh` (`playdrop bench` h2 and raw, iperf3 on 127.0.0.1).
- Units: MiB/s, 1 MiB = 1,048,576 bytes. iperf3 `sum_received` bits per second divided by 8
  and by 1,048,576.

### Hardware and link (2026-10-13)

| Item | Value |
|---|---|
| Desktop | Pop!_OS COSMIC, 16 cores, kernel 7.1.5. Wi-Fi client `wlp98s0` (the Ethernet port `eno1` is down), so both devices are on Wi-Fi and every packet crosses the access point twice |
| Desktop link | `nmcli`: SSID "Unreal", channel 149 (5745 MHz), rate 270 Mbit/s, signal 95 |
| Phone | Samsung Galaxy A54 (SM-A546U1), Android 16, Wi-Fi 6 (802.11ax) |
| Phone link | `adb shell dumpsys wifi`: 5745 MHz, same BSSID, RSSI -50 to -52 dBm, link speed 408 to 576 Mbit/s tx, 864 to 1200 Mbit/s rx (varies minute to minute) |
| Router | Same access point for both (BSSID b4:23:a2:6d:da:5d); mesh model unknown |
| PlayDrop | commit `b1f7195` plus the uncommitted working tree of 2026-10-13; `playdropd` 0.1.0 release build on the desktop; phone app 0.1.0, **debuggable APK** (see caveats) |
| iperf3 | desktop 3.16; phone 3.15 built with NDK 27.1.12297006 (recipe in the header of `bench/iperf3-phone.sh`) |
| Load | The desktop was shared with other build jobs; load average 3 to 12 during the runs, 38 at the start of the first loopback attempt (discarded, see 3.1) |

## 2. Results over Wi-Fi

### 2.1 iperf3 baseline, desktop and phone (`bench/iperf3-phone.sh`, 10 s x 3)

| Direction | Streams | Runs MiB/s | Mean MiB/s | Mean Mbit/s |
|---|---|---|---|---|
| desktop to phone | 1 | 50.8, 43.7, 38.0 | 44.2 | 370 |
| desktop to phone | 4 | 52.9, 53.0, 47.6 | 51.2 | 429 |
| phone to desktop | 1 | 51.3, 49.3, 44.2 | 48.3 | 405 |
| phone to desktop | 4 | 43.7, 46.3, 45.2 | 45.1 | 378 |

The radio spread between runs (38 to 51 MiB/s) is large, so a single PlayDrop run cannot
resolve less than about 15% of the baseline.

### 2.2 The matrix, desktop to phone, MiB/s

PlayDrop cells are from `bench/desktop-to-phone.sh` with the **debug** phone app. Every
PlayDrop run listed completed with all file counts and sha256 sums matching on the phone.

| Dataset | iperf3 (1 stream) | PlayDrop (debug phone app) | LocalSend | KDE Connect |
|---|---|---|---|---|
| `big-1g` | 44.2 | 9.5, 9.8, 10.3, 10.0 (mean 9.9; 99.8 to 108.0 s) | not run | not run |
| `medium-100` | 44.2 | 13.7, 16.4, 15.9 (mean 15.3; 10.3 to 12.3 s) | not run | not run |
| `small-1000` | 44.2 | 1.83 (53.2 s) | not run | not run |
| `mixed-5000` | 44.2 | 0.72 (228.9 s) | not run | not run |
| `tree` | 44.2 | 2.19, 2.27 (7.1 and 6.9 s) | not run | not run |

Run order, with the load average at the time: `tree` and `small-1000` at about 8,
`big-1g` (run 1), `mixed-5000` and `medium-100` (run 1) at 12 to 38 while another build
ran, the rest at 4 to 9. The `big-1g` runs under heavy and light load agree (9.5 vs 9.8 to
10.3), so the desktop load did not set the rate.

Share of the iperf3 one-stream baseline (44.2 MiB/s):

| Dataset | PlayDrop / iperf3 |
|---|---|
| `big-1g` | 22% |
| `medium-100` | 35% |
| `small-1000` | 4% |
| `mixed-5000` | 1.6% |
| `tree` | 5% |

Per-file cost on the phone: `tree` 44 ms, `small-1000` 53 ms, `mixed-5000` 46 ms per file,
nearly constant while the file size changes from 4 KiB to 100 KiB. That pattern points at
a fixed cost per delivered file on the phone (the staged-receive plus MediaStore sink,
`MediaStoreSink.kt`) rather than at the network or at the size of the data. This is an
inference from the timings, not a profile. During the `big-1g` run the phone app process
sat at about 110% CPU (two threads at 65% and 42%) while the desktop was idle, and the
transfer rate was steady at 8 to 12 MiB/s throughout (`transfers` sampled every 3 s).

### 2.3 Phone to desktop

| Dataset | iperf3 -R (1 stream) | PlayDrop | LocalSend | KDE Connect |
|---|---|---|---|---|
| any | 48.3 | not scripted (needs taps in the phone UI) | not run | not run |

Earlier evidence only, not comparable: Phase 3 (`docs/phases/phase-3-android.md`, section
1.1) sent a 400 MiB file from the phone's share sheet to the daemon in 18 s (22 MiB/s) with a
debug build, SHA-256 equal.

## 3. Results on the desktop alone

### 3.1 Loopback, `bench/loopback.sh` (1 GiB, 3 runs, release build, 127.0.0.1)

Run on 2026-10-13 at a load average of about 7:

| Path | Mean MiB/s | Min | Max |
|---|---|---|---|
| iperf3 -c 127.0.0.1 -t 10, 1 stream (no TLS, no hashing) | 13,163 | | |
| `playdrop bench --mode h2` | 1,857 | 1,670 | 1,957 |
| `playdrop bench --mode raw` | 2,870 | 2,767 | 2,964 |

h2 is 0.65 of raw framing. Phase 1 recorded h2 about 2,080 and raw about 2,845 MiB/s on a
quiet machine (`docs/phases/phase-1-core.md`, gate C). A first attempt at load average 38
gave 730 and 1,134 MiB/s and is discarded; the figures move a lot with other builds
running. Loopback iperf3 has no encryption or hashing, so the "share of iperf3" on loopback
(14% for h2) says nothing about the 50% target; that target is about a real link.

### 3.2 Daemon to a second host on the same machine (the transfer engine without the phone)

`alpha` (daemon) to `bravo` (`playdrop host`) over loopback, receive dir on ext4,
`bench/pd_ipc.py run bravo <dataset>` (a D-Bus shim since replaced by `playdrop send --to bravo --json`), 2026-10-13:

| Dataset | MiB/s | Wall s |
|---|---|---|
| `big-1g` | 260 | 3.9 |
| `medium-100` | 93, 105, 93 | 1.8, 1.6, 1.8 |
| `mixed-5000` | 170, 191, 220, 221 | 1.0, 0.87, 0.75, 0.75 |
| `small-1000` | 187 | 0.52 |
| `tree` | 51 | 0.31 |

5,000 small files arrive as fast as or faster than 100 medium files here, so the engine and
protocol do not slow down with the file count (the small files travel as a bundle,
protocol.md 7.7). The gate-B test records the same property: `gate_b_many_small_files_hold_throughput`
0.88 of one big file in release (Phase 1).

## 4. Against the PLAN.md targets

| Target | Verdict | Evidence |
|---|---|---|
| One large file at 50% or more of iperf3 on the same link (floor) | **Pass with the release app** (section 7): 43.7 and 41.4 MiB/s against 44.2, 94 to 99% | The debug app measured 22% (9.9 MiB/s) earlier the same day, section 2.2; the Gradle debug variant compiles the Rust core without optimisation |
| 80% (stretch) | **Pass** (section 7) | as above |
| No throughput drop between 100 and 5,000 files | **Pass on the engine, partial to the phone** | desktop to desktop: 5,000 files at 170 to 221 MiB/s vs 100 files at 93 to 105 MiB/s. Desktop to phone with the Phase 7 landing folder (section 8): 7.2 to 9.5 vs 33.9 to 46.0 MiB/s, about 0.2 (was 1.9 vs 39.8, about 0.05, in Phase 6); the floor is now the core's per-member delivery, Phase 8 |
| Published matrix against LocalSend and KDE Connect | **Dropped** | the user chose on 2026-10-14 not to run the incumbents (section 5 item 6); the claim in PLAN.md stays unmeasured; `bench/incumbents.md` keeps the method if anyone wants it later |
| Cold send under 1 s, warm clipboard under 50 ms | Not measured here | Phase 2 recorded desktop clipboard to peer at 2 to 5 ms |

## 5. Caveats

1. **Debug build.** `adb shell dumpsys package org.playdrop.android` reports `DEBUGGABLE`,
   and the Gradle debug task builds the Rust core without `--release`
   (`apps/android/app/build.gradle.kts`). The 10 MiB/s figure matches the debug figure in
   Phase 3 (about 10 MiB/s), while that phase's release build did 32 MiB/s. The ship
   numbers must come from `./gradlew :app:assembleRelease`. Installing it over the debug
   app needs the same signing key or an uninstall, which would drop the pairing, so it was
   not done here. `bench/desktop-to-phone.sh` now prints "App build: DEBUGGABLE" or "release".
2. **Both ends on Wi-Fi.** The desktop is wireless (270 Mbit/s reported). A wired desktop
   would raise the iperf3 ceiling and change the shares.
3. **Single phone, single day, one access point.** Wi-Fi varies by several MiB/s from
   minute to minute (iperf3 runs of 38 to 51 MiB/s). Treat shares within 15% as equal.
4. **Screen state.** The main runs had the phone screen on. A screen-off pair was run
   afterwards (`mWakefulness=Dozing`, `input keyevent 223`): `tree` 2.18 MiB/s and
   `medium-100` 16.4 MiB/s, both complete with all sums matching, the same as screen on.
5. **The desktop was busy** with other builds during part of the runs.
6. **LocalSend and KDE Connect are not measured, by choice.** The user decided on
   2026-10-14 not to install or run them for a comparison: KDE Connect was unreliable
   for them, which is why this project exists, and they do not want to operate the
   LocalSend UI. The numbers in PLAN.md "What is actually fast on a LAN" are third-party
   reports from other hardware and are not comparable. The method stays in
   `bench/incumbents.md` for anyone who wants the comparison.
7. Empty folders are not recreated on the phone (MediaStore keeps none), so the 3 empty
   folders of `tree` are not recreated there; all 160 files and their sums matched.

## 6. To run (needs the user or a different machine)

| Item | How | Needs |
|---|---|---|
| Release-app PlayDrop rows | done: sections 7 and 8 | |
| LocalSend and KDE Connect rows | `bench/incumbents.md` | dropped by the user on 2026-10-14; only if someone else wants the comparison |
| Wired-desktop rows | same scripts with `eno1` up | Ethernet cable |
| Phone to desktop PlayDrop rows | share-sheet send with a stopwatch or screen recording, or a future `playdrop` CLI/instrumented path | user at the phone |
| Windows 11 rows | CI runner has no radio | a Windows machine |
| iPhone rows | free-team build via xtool, `scripts/xtool-dev.sh` | user at the phone |

## 7. Release core, sink rewrite (2026-10-13, evening)

Same phone, link and daemon as section 1 (phone RSSI -48 to -51 dBm, link 576 to 648 Mbit/s
tx, 1200 Mbit/s rx, screen `Dozing`; desktop load average 10 to 16). The phone app is now a
**release build** (`./gradlew :app:assembleRelease -Pplaydrop.signWithDebugKey=true`,
installed over the debug app, pairing kept). Every row completed with all file counts and
sha256 sums matching on the phone, and every file was listed in the MediaStore Downloads
collection afterwards.

### 7.1 Desktop to phone, MiB/s (`bench/desktop-to-phone.sh`, one run per cell unless listed)

| Dataset | Debug app, old sink (section 2.2) | Release app, old MediaStore sink | Release app, new direct-write sink |
|---|---|---|---|
| `big-1g` | 9.9 | 43.7 (23.4 s) | 41.4 (24.7 s) |
| `medium-100` | 15.3 | not run | 39.8 (4.2 s) |
| `small-1000` | 1.83 | 2.22 (44.0 s) | 4.67, 4.52, 5.20, 4.52 (18.8 to 21.6 s) |
| `mixed-5000` | 0.72 | 0.85 (195.0 s) | 1.83, 1.91, 1.92 (85.9 to 90.4 s) |
| `tree` | 2.19 | not run | 7.02 (2.2 s) |

### 7.2 Where the phone spends its time per file (sink debug log, `adb logcat -s playdrop:*`)

Old sink (MediaStore insert with `IS_PENDING`, `openFileDescriptor`, copy, `fsync`, then
`IS_PENDING = 0`), `mixed-5000`: insert 6.7 ms, open 5.2, copy 1.7, fsync 1.5, close 0.8,
publish 18.5; **34.5 ms per file**. `small-1000` gave the same split (34.6 ms).

New sink (Android 11 and later: `open(O_CREAT|O_EXCL)` straight into
`Download/PlayDrop/<dirs>`, kernel copy, close, no per-file fsync below 8 MiB, one media scan
per 500 files in the background), `mixed-5000`: create 11.2 to 11.9 ms, copy 1.0 to 1.3,
close 0.3, URI 0.04; **12.9 to 13.2 ms per file**. On top of the sink, the time between two
deliveries outside the sink (core staging open and delete, the UniFFI callback) is 2.5 to
3.3 ms per file.

The create step is MediaProvider inserting a database row for each new file on shared
storage (the FUSE create). Measured from a test build: 6 to 8 ms per file for the first few
hundred files after a start, then 12 to 20 ms; the same from `adb shell` (22 to 25 ms); 4 or
8 parallel writers gave the same total rate (18 ms per file), so MediaProvider serialises
it. Writing into the app's own `Android/data` folder costs 0.23 ms per file, but a folder
cannot be renamed from there into `Download` (`EXDEV`), and files there are not in
Downloads. The rows created by the FUSE create stay hidden (`is_download = 0`, no size)
until the scanner runs, which is why the sink still scans (about 8 ms per file in the
background, 4 to 5 s per batch of 500).

### 7.3 Verdict against "no drop between 100 and 5,000 files"

Still **fail to the phone**: `mixed-5000` at 1.9 MiB/s against `medium-100` at 39.8 MiB/s.
The sink rewrite took the per-file cost from about 35 ms to about 13 ms (2.2 to 2.7 times
faster on small files), but a file that must show in the public Downloads collection costs
one MediaProvider insert, and that alone is above the 3 ms target on this phone. Delivering
bundle members while the bundle still streams (core pipelining) would hide at most the
7 to 8 s of network and hashing time of `mixed-5000` behind about 78 s of delivery, about
10%.

Decision (2026-10-14): v0.1.0 ships at this floor. Large and medium files pass the gate and
floods of thousands of tiny files are rare in daily use. The two ways below the floor
(receive into the app's own folder and file into Downloads in the background, or send very
large counts as one archive) are product changes and go to Phase 7. The incumbents run in
section 2.2 is not run (user choice); LocalSend would pay the same insert plus one
request per file, so it is expected to sit at or below this floor. Phase 7 took the first
of those two ways; section 8 has the result.


## 8. Phase 7: landing folder and the rename finding (2026-10-15)

Same phone and link (Android 16 now, RSSI -56 dBm, 408 Mbit/s tx, 720 to 1200 Mbit/s rx,
screen `Dozing`), desktop load average 2.5 to 10, release app from the Phase 7 commit plus
the rename change below (`assembleRelease -Pplaydrop.signWithDebugKey=true`), daemon
`0.2.0`, `bench/desktop-to-phone.sh --peer SM-A546U1 --runs 3`. Every row completed with all
file counts and sha256 sums matching on the phone. A bulk drop (64 files or more, mean under
1 MiB: `tree`, `small-1000`, `mixed-5000`) lands in the app's own folder and a mover copies
it into Downloads after the send returns; "In Downloads +s" is how much later the last file
was there.

### 8.1 Desktop to phone, MiB/s (three runs each)

| Dataset | Phase 6 release app, direct-write sink (section 7.1) | Phase 7 | Wall s | In Downloads +s |
|---|---|---|---|---|
| `big-1g` | 41.4 | 43.9, 43.5, 47.6 | 23.3, 23.6, 21.5 | 0 |
| `medium-100` | 39.8 | 33.9, 39.9, 46.0 | 5.0, 4.2, 3.7 | 0 |
| `small-1000` | 4.67, 4.52, 5.20, 4.52 | 14.8, 15.4, 20.1 | 6.6, 6.3, 4.9 | 17.9, 17.9, 16.9 |
| `mixed-5000` | 1.83, 1.91, 1.92 | 9.46, 8.77, 7.20 | 17.5, 18.8, 22.9 | 83.4, 86.4, 85.4 |
| `tree` | 7.02 | 12.0, 13.5, 12.4 | 1.30, 1.16, 1.26 | 2.2, 2.2, 3.2 |

### 8.2 Where the time goes now (sink debug log)

- Landing, `mixed-5000`: 0.69 ms per file inside the sink (claim 0.47, rename from staging
  into the landing folder 0.20, URI 0.02), down from 13 ms. The whole delivery of 5,000
  members still spans 10.1 s, so about 1.3 ms per file is spent outside the sink: the core's
  per-member staging file, verification, descriptor hand-over and delete. That, plus the
  7 to 8 s of network and hashing, is the 17 to 23 s wall time.
- Mover, `mixed-5000`: 84 s for 5,000 files, 14 ms each (create 9.8, journal 1.7, copy 0.4,
  close 0.3), in the background; the sender is free after 17 to 23 s instead of 86 to 90 s.
- Direct path, `medium-100`: 16 ms per file (create 11.5, copy 4.4), unchanged.
- `big-1g`: the copy from staging into Downloads is 4.4 to 5.7 s of the 21.5 to 23.6 s.

### 8.3 The rename finding

Phase 7 was written with a `rename(2)` of the staging file (or the landed file) into
Downloads ahead of the copy, gated per route on `EXDEV`, `EACCES` or `EPERM`. On this phone
the rename succeeds, and the first runs of the day used it:

| Route | Rename | The app's own copy |
|---|---|---|
| staging into Downloads, 1 GiB | 8.3 s, 8.0 s | 4.4 s, 4.1 s |
| staging into Downloads, 1.7 MiB files (`medium-100`) | 35 ms per file | 16 ms per file |
| landing folder into Downloads, `mixed-5000` | 145 s, 124 s (29 and 26 ms per file) | 59 s (14 ms per file) |
| staging into the landing folder (same tree) | 0.1 to 0.2 ms per file | not needed |

The rename into Downloads scales with the file size at about 8 ms per MiB: MediaProvider's
FUSE layer serves it as a copy behind the scenes, through a slower path than the app's
kernel copy. It is also not atomic: a force-stop of the app in the middle of the move left
`m0221.bin` complete in Downloads and still in the landing folder, so the next pass moved it
again as `m0221 (1).bin` (5,001 files). The same force-stop with the journaled copy ended
with exactly 5,000 files and an empty landing folder. The rename into Downloads was removed
from both routes (`DownloadsSink.kt`); the rename from staging into the landing folder is a
real rename and stays. Table 8.1 is measured after that change.

### 8.4 Verdict against "no drop between 100 and 5,000 files"

At the sender, `mixed-5000` runs at 7.2 to 9.5 MiB/s against 33.9 to 46.0 for `medium-100`:
about 0.2, still under the 0.8 gate, but 4 to 5 times the Phase 6 figure, and the floor is
no longer MediaProvider. The remaining cost is the core's per-member delivery (about 2 ms
a file including the sink) while the bundle has already fully arrived; delivering members
while the bundle still streams, or handing the sink several members at once, is the Phase 8
item. Files reach Downloads 85 s after a 5,000-file drop; the notification shows the move.

### 8.5 Resume over Wi-Fi

`big-1g` with `adb shell am force-stop org.playdrop.android` at 50% and the app relaunched
8 s later: the transfer finished 21.6 s after the relaunch (the remaining half plus the copy
tail; a restart from zero would have needed about 35 s), 42.6 s in all, sha256 equal, one
file, no " (1)" copy.

## 9. Phase 8: members placed while the bundle streams (2026-10-16)

Same phone and link (Android 16, RSSI -46 to -53 dBm, 408 Mbit/s tx, 720 to 1200 Mbit/s
rx, screen off), desktop load average 1.6 to 1.9, release app from the Phase 8 tree
(`assembleRelease -Pplaydrop.signWithDebugKey=true`), daemon `0.3.0`,
`bench/desktop-to-phone.sh --peer SM-A546U1 --runs 3`. Every row completed with all file
counts and sha256 sums matching on the phone. The receiver now hands each bundle member to
the sink as soon as its chunks are verified and durable, on a delivery thread, instead of
all members after the `pd-root` trailer (`docs/phases/phase-8-fast-bulk.md` section 1.1,
protocol.md 11.1).

### 9.1 Desktop to phone, MiB/s (three runs each)

| Dataset | Phase 7 (section 8.1) | Phase 8 | Wall s | In Downloads +s |
|---|---|---|---|---|
| `big-1g` | 43.9, 43.5, 47.6 | 35.4, 44.5, 41.6 | 28.9, 23.0, 24.6 | 0.1 |
| `medium-100` | 33.9, 39.9, 46.0 | 47.5, 52.1, 54.1 | 3.6, 3.2, 3.1 | 0.0, 0.1, 0.1 |
| `small-1000` | 14.8, 15.4, 20.1 | 31.4, 42.6, 31.7 | 3.1, 2.3, 3.1 | 11.7, 10.7, 10.7 |
| `mixed-5000` | 9.46, 8.77, 7.20 | 22.6, 22.6, 21.9 | 7.3, 7.3, 7.5 | 52.2, 51.1, 53.3 |
| `tree` | 12.0, 13.5, 12.4 | 14.6, 15.9, 6.9 | 1.1, 1.0, 2.3 | 2.2 |

A first `mixed-5000` run before the three above, with the desktop still settling after
the install, took 9.3 s at 17.8 MiB/s.

### 9.2 Where the time goes now (sink debug log)

- `mixed-5000`: the sink's first-to-last delivery now spans 5.9 to 6.4 s, inside the 7.3 s
  the send takes, at 0.45 to 0.50 ms per file in the sink (claim 0.27 to 0.28, rename from
  staging into the landing folder 0.16 to 0.21, URI 0.01). In Phase 7 the same delivery
  started after the stream and spanned 10.1 s on its own. The tail after the last byte is
  under 1 s.
- `small-1000`: delivery 1.5 to 2.2 s inside a 2.3 to 3.1 s send, 0.43 to 0.62 ms per file.
- Mover (landing folder into Downloads, in the background): 52 s for 5,000 files, about
  10 ms each, unchanged in kind (MediaProvider insert per file); the sender is free after
  7.3 s instead of 17 to 23 s.
- `medium-100` direct path: 9.6 to 12.6 ms per file (create 7.4 to 8.5, copy 2.0 to 4.0),
  the same floor as before.
- `big-1g`: the copy from staging into Downloads is 4.0 to 4.1 s of the 23 to 29 s. Run 1 at
  35.4 MiB/s was the first large run after the install; runs 2 and 3 sit in the Phase 7
  band.

### 9.3 Verdict against "no drop between 100 and 5,000 files"

At the sender, `mixed-5000` runs at 21.9 to 22.6 MiB/s against 47.5 to 54.1 for
`medium-100`: a ratio of about 0.43, up from 0.2 in Phase 7 and still under the 0.8 gate.
The serial delivery floor is gone; what remains is per-member work on both ends while the
bytes stream (the sender's open and read per source file, the receiver's staging file per
member, the hashing of 4 to 64 KiB members at 1 MiB chunks) plus the sink's 0.45 ms. The
next measurement belongs in Phase 9: time the sender's per-file open and the receiver's
per-member staging create and close separately before designing anything.

### 9.4 Resume by Wi-Fi drop (owed from Phase 7)

`big-1g` with `adb shell svc wifi disable` at about 50 % and `svc wifi enable` 10 s later:
the phone came back with the same address and reached the router, but phone and desktop
could not reach each other at all afterwards (ping failed both ways, the phone's ARP entry
for the desktop stuck in `PROBE`, both sides dialling every last-known address and timing
out). The sender waited its 10 minute reconnect window at 859 MiB and failed with
"receiver unreachable: every candidate failed"; a second Wi-Fi toggle did not clear it.
This is the mesh network isolating the rejoined client, not the app: the listener was up
and the desktop reached another LAN device meanwhile. The receiver kept its verified
859 MiB under the old transfer id; a fresh `playdrop send` creates a new id and starts from
0 (sender persistence is receiver-only, protocol.md 11.1), so that staging folder waits for
the 24 h sweep. The gate is still owed; run it again after the phone rejoins the network
from its own settings, with a shorter outage, and record the resume offset and the sha256.

## 10. Phase 9: the small-file floor measured on both ends (2026-10-17 to 2026-10-22)

Not an end-to-end phone run: the receiver's bundle path was timed in isolation with the
ignored benchmark `cargo test --release -p playdrop-core --test phase9_receiver -- --ignored
--nocapture` (5,000 members of 4 to 64 KiB, seeded, 170.9 MiB bundle, 1 MiB chunks), on the
desktop (`/tmp`, ext4) and on the Galaxy A54 by running the same test binary from
`/data/local/tmp` (f2fs) as the shell user. Before is the 0.3.0 receiver with the same timing
hooks; runs were interleaved. Times per member in ms.

### 10.1 Desktop, ext4 (medians of 18 runs)

| Mode | Wall s | Stream | Checkpoint | Delivery |
|---|---|---|---|---|
| Directory, before | 0.50 | 0.024 | 0.055 | 0.026 |
| Directory, after | 0.49 | 0.024 | 0.055 | 0.021 |
| Staged, before | 0.44 | 0.021 | 0.050 | 0.020 |
| Staged, after | 0.44 | 0.022 | 0.050 | 0.020 |

### 10.2 Phone, f2fs (medians of 9; noisy, read as relative)

| Mode | Wall s | MiB/s | Stream | Checkpoint | Delivery |
|---|---|---|---|---|---|
| Directory, before | 9.43 | 18.1 | 0.45 | 0.89 | 0.185 |
| Directory, after | 4.62 | 37.0 | 0.33 | 0.26 | 0.068 |
| Staged, before | 7.01 | 24.4 | 0.32 | 0.64 | 0.143 |
| Staged, after | 5.31 | 32.2 | 0.37 | 0.37 | 0.100 |

### 10.3 What made the difference

- The checkpoint flush: 0.3.0 used one `syncfs` for many members only under
  `cfg(target_os = "linux")`, which Android does not match, so the phone flushed every member
  with its own `fdatasync`. 0.4.0 uses `syncfs` on Linux and Android when the staging folder is
  on ext2/3/4, f2fs, xfs or btrfs, and per-file flushes elsewhere (sdcardfs on Android 8 to 10
  reports success from `syncfs` without flushing).
- Placement: a no-replace rename instead of an exclusive create followed by a rename onto it.
- Sender: members under 256 KiB are read in one blocking call (0.18 to about 0.11 ms per
  member on the phone, reading from page cache, no hashing).

The `mixed-5000` against `medium-100` gate (0.8) still needs a real desktop-to-phone run with
`bench/desktop-to-phone.sh` on a release APK; the phone-side receive of the bundle alone is now
roughly twice as fast in Directory mode and a quarter faster in Staged mode, which the app uses.

## 11. Phase 9: end-to-end phone run and the kill-and-resume check (2026-10-28)

Same phone (Galaxy A54, Android 16), 5 GHz 11ax link at RSSI -51 dBm, 612 Mbit/s tx and 960
to 1200 Mbit/s rx, screen on and the app in the foreground. Release app from the Phase 9 tree
at `4d5f0af` (`assembleRelease -Pplaydrop.signWithDebugKey=true`), daemon `0.4.0` with a
fresh data dir (the machine had rebooted, so the pairing was redone over the SAS code),
`bench/desktop-to-phone.sh --runs 3`. Every row completed with all file counts and sha256
sums matching on the phone.

### 11.1 Desktop to phone, MiB/s (three runs each)

| Dataset | Phase 8 (section 9.1) | Phase 9 | Wall s | In Downloads +s |
|---|---|---|---|---|
| `big-1g` | 35.4, 44.5, 41.6 | 48.1, 46.2, 46.2 | 21.3, 22.1, 22.2 | 0.1, 0.0, 0.1 |
| `medium-100` | 47.5, 52.1, 54.1 | 18.2, 49.2, 48.9 | 9.3, 3.4, 3.5 | 0.1, 0.0, 0.1 |
| `small-1000` | 31.4, 42.6, 31.7 | 30.7, 36.9, 35.2 | 3.2, 2.7, 2.8 | 11.8, 8.6, 11.7 |
| `mixed-5000` | 22.6, 22.6, 21.9 | 14.9, 24.0, 24.0 | 11.1, 6.9, 6.9 | 52.2, 53.2, 50.1 |
| `tree` | 14.6, 15.9, 6.9 | 5.1, 14.7, 14.1 | 3.0, 1.1, 1.1 | 2.2, 2.2, 2.2 |

### 11.2 Reading

- `mixed-5000` lands in 6.9 s (24 MiB/s) against 7.3 s (22 MiB/s) in Phase 8: the phone-side
  bundle receive measured in section 10.2 (7.0 to 5.3 s in Staged mode) is the whole of the
  gain, and it shows end to end. `big-1g` is at 46 to 48 MiB/s, the top of every run so far.
- The first run of `mixed-5000`, `medium-100` and `tree` was slow (11.1, 9.3 and 3.0 s) with the
  rate dropping to 4 to 6 MiB/s for a few seconds before recovering; the second and third
  runs of each were steady. Each slow run came right after the phone's mover had finished
  copying the previous dataset into Downloads, so the likely cause is the phone indexing
  those new files (not measured; a guess). Read the second and third runs as the number.
- The `mixed-5000` against `medium-100` gate (0.8) is still not met: 24.0 against 49.0 is 0.49
  (Phase 8: 0.43). The remaining floor is per-member work on the phone (section 10.2: about
  0.37 ms stream plus 0.37 ms checkpoint plus 0.10 ms delivery per member in Staged mode),
  not the link; the Downloads copy afterwards is a further 50 s that the wall time excludes.

### 11.3 Kill the daemon mid-send, then restart it

`playdrop send --no-wait /tmp/pd-bench/big-1g`, then `kill -9` on `playdropd` 7 s in, at
424 MiB of 1 GiB. The record was on disk under `outgoing/` (`.json`, `.state`, `.hashes`).
On restart the daemon logged `resuming a recorded send` at once, the transfer showed
`active` from 484 MiB on, and it reached `complete` 18 s after the restart, with no command
from the user and no prompt on the phone. The file on the phone has the expected sha256
(`42019ed2c3a4` prefix) and the `outgoing/` folder was empty afterwards. This is the Phase 9 claim
("a send survives its own restart") on a real link; the loopback version is
`phase9_send::a_send_survives_a_host_restart`.

## 12. Phase 10: the mover, the checkpoint cadence, and a USB surprise (2026-10-28 and 29)

Same phone (Galaxy A54, Android 16), daemon `0.5.0` with the same data dir as section 11, release
app (`assembleRelease -Pplaydrop.signWithDebugKey=true`). Every row below completed with all
file counts and sha256 sums matching on the phone.

### 12.1 The receive checkpoint (phone, isolated bench)

`cargo test --release -p playdrop-core --test phase9_receiver -- --ignored --nocapture` run
from `/data/local/tmp` (f2fs) with finer timing hooks, 5,000 members of 4 to 64 KiB, Staged
mode, medians of 5 to 10 runs. The checkpoint's per-member cost was almost all `syncfs`: about
105 to 117 ms per call on the A54 (f2fs writes a full checkpoint each time), and each flush also
stalls the stream's creates and writes.

| Cadence | Checkpoints | Wall s | `syncfs` ms per member |
|---|---|---|---|
| 16 MiB or 1 s (0.4.0) | 11 | 3.73 to 4.56 | 0.233 to 0.258 |
| 64 MiB or 1 s where `syncfs` is used (0.5.0) | 3 | 1.65 to 1.78 | 0.036 to 0.039 |

### 12.2 The mover (phone, 1,000 files of 4 to 64 KiB, `MoverProbe` and `LandedMoveDeviceTest`)

| Way into Downloads | ms per file (median) |
|---|---|
| One `O_EXCL` create at a time, one-slot journal (0.4.0) | 10.4 to 14.0 (single runs 7.5 to 22) |
| Creates on 2 / 4 / 8 threads | 13.5 / 16.2 / 14.9 |
| MediaStore pending rows, published in batches | 14.4 to 18.3 |
| Published rows inserted 100 at a time, copies on 4 threads (0.5.0) | 5.0 (probe), 6.3 to 6.7 (real code) |

MediaProvider serialises creates, so threads alone do not help; inserting rows in batches does.

### 12.3 Desktop to phone over USB tethering (2026-10-28, evening)

USB tethering was on, and PlayDrop's connection ran over it (`usb0`, RNDIS on USB 2.0), not over
Wi-Fi: big files topped out at 26 to 28 MiB/s while raw TCP over Wi-Fi did 48 to 56. The phone's
engine was checked against 0.4.0 on the phone itself (`phase1_gates` gate A, 500 MiB, both
hosts on the phone): 0.4.0 102 to 107 MiB/s, 0.5.0 119 to 122 MiB/s. Neither the phone nor the
desktop (0.4.0 daemon gave the same 26.7 MiB/s) regressed.

| Dataset | Wall s | MiB/s | In Downloads +s |
|---|---|---|---|
| `small-1000` | 3.8 | 25.1 to 26.0 | 6.5 (0.4.0 on Wi-Fi: 8.6 to 11.8) |
| `mixed-5000` | 8.0 to 8.1 | 20.3 to 20.7 | 30.6 to 32.9 (0.4.0 on Wi-Fi: 50 to 53) |
| `medium-100` | 5.5 to 5.6 | 30.0 to 30.7 | 0.0 to 0.1 |
| `big-1g` | 36.5 to 36.7 | 27.9 to 28.1 | 0.0 to 0.1 |

### 12.4 Desktop to phone over Wi-Fi (2026-10-29, morning)

Tethering off, 5 GHz, RSSI -53, tx 408 Mbit/s, raw TCP 45.7 to 55.6 MiB/s.

| Dataset | Wall s | MiB/s | In Downloads +s |
|---|---|---|---|
| `tree` | 1.3 | 11.7 to 11.9 | 2.2 |
| `small-1000` | 3.7 to 3.9 | 25.3 to 26.6 | 7.5 to 8.6 |
| `mixed-5000` | 12.9 to 15.8 | 10.5 to 12.8 | 38.1 to 41.7 |
| `medium-100` | 4.4 to 4.6 | 36.1 to 38.3 | 0.0 to 0.1 |
| `big-1g` | 27.0 to 29.8 | 34.3 to 38.0 | 0.0 |

`mixed-5000` sent more slowly than in section 11 (6.9 s). An A/B the same morning first pointed
at the 0.5.0 Kotlin (0.4.0 Kotlin with the 0.5.0 core: 7.2 to 8.8 s, sink 0.43 ms per file;
0.5.0: 13.6 to 15.8 s, sink 0.85 to 1.23 ms), but an hour later 0.4.0's Kotlin was just as slow
(12.5 to 13.6 s, sink 0.73 to 0.75 ms) and 0.4.0's own mover took 80 to 96 s instead of 50: the
whole phone had slowed down (no heat, no battery saver, MediaProvider restarted without effect;
the battery was discharging on USB after tethering was turned off). The phone was rebooted to
repeat the A/B; see 12.5.

### 12.5 The A/B after a reboot (2026-10-29, evening)

Phone rebooted (the mesh then isolated it from the PC until its Wi-Fi was turned off and on
from Settings), battery charged to 79 %, Wi-Fi, desktop load average 0.7 to 2.5. Old and new
app installed alternately (`adb install -r`), 45 s settle, two `mixed-5000` runs each. "0.4.0
Kotlin" is the 0.4.0 app code with the 0.5.0 core (byte-identical `libplaydrop_ffi.so`).

| App | Wall s | MiB/s | Sink ms per file | In Downloads +s |
|---|---|---|---|---|
| 0.4.0 Kotlin | 8.08, 6.95 | 20.4, 23.8 | 0.429, 0.415 | 47.7, 47.8 |
| 0.5.0 | 7.35, 6.95 | 22.5, 23.8 | 0.452, 0.443 | 29.6, 31.7 |
| 0.4.0 Kotlin | 7.40, 6.89 | 22.3, 24.0 | 0.455, 0.439 | 47.7, 50.1 |
| 0.5.0 | 7.28, 6.73 | 22.7, 24.5 | 0.426, 0.424 | 31.8, 31.9 |

The receive is the same speed with either app, and the morning's 13 to 15 s were the phone's
state, not the code. 0.5.0 gets 5,000 files into Downloads about 37 % sooner. The
`mixed-5000` against `medium-100` ratio stays about 0.5 to 0.6 over Wi-Fi (the 0.8 target is
not met; the floor is per-member work on the phone, 12.1).

## 13. Phase 11: the small-file critical path (2026-10-30 and 2026-11-02)

Same phone (Galaxy A54, Android 16), 5 GHz 11ax, RSSI -46 to -53, tx 544 to 680 Mbit/s, on USB for
adb with tethering off. Release apps only (`assembleRelease -Pplaydrop.signWithDebugKey=true`), each
built from an isolated copy of the tree. Sections 13.1 and 13.3 hold only these changes on top of
`80d0272` (v0.5.0), without the parallel path-choice work, on a daemon of that tree whose sending is
unchanged; another agent's load tests (many `yes` processes) and builds ran on the desktop during 13.3
(load average 3 to 20). Sections 13.4 and 13.7 are the final tree with the path-choice work, on a quiet
desktop.

Bench rows ran through `bench/desktop-to-phone.sh`; each ended with all counts and sha256 sums
matching on the phone unless a row says otherwise. The phone was in the slow state of 12.4 throughout
(`mixed-5000` on 0.5.0 takes 13 to 17 s here, against 7 s in 12.5), so read the old against new
columns of one table, and do not compare them with earlier sections.

### 13.1 Where the seconds went (`mixed-5000`, per-thread timers, all runs of 2026-10-30)

Three runs of 0.5.0 and two of the final design (that day's build, before write-ahead was tried, so
it holds exactly the changes that stayed). The sender was never the limit: reading and hashing the bundle took 0.12 to 0.86 s of the send, and
on the desktop the isolated bench gives the sender 0.14 s and the whole receive 0.52 s
(`phase9_receiver`, 5,000 members, this tree; the daemon-to-second-host run of 3.2 was not repeated).
The remaining time is two slow lines on the phone, side by side.

| | 0.5.0 (3 runs) | Final design (2 runs) |
|---|---|---|
| Wall | 14.7 to 15.1 s | 6.9 to 8.3 s |
| Body received, last byte in | 7.1 to 8.1 s | 4.7 to 6.6 s |
| of that: waiting for the network and hashing | 1.3 to 1.6 s | 1.4 to 3.2 s |
| of that: chunk writes on the stream thread | 5.3 to 6.4 s | 2.5 s |
| of that: `syncfs` on the stream thread | 3.5 to 4.4 s (11 calls) | none (4 to 5 calls in the background, 3.9 s in all) |
| Members already with the sink at that point | 2,217 to 2,846 | 2,595 (one run) |
| Tail: last byte to all members delivered | 6.3 to 7.4 s | 1.5 s (one run; the sender saw 2.1 to 2.5 s after its trailer) |
| First to last delivery | 13.0 to 13.4 s | 5.1 to 6.3 s |

The delivery thread was the longer line: a sink call took 2.4 to 2.6 ms per member against the 1 ms
the sink itself counted. The parts, with the one-thread build on the same day:

| Part of one delivery | ms |
|---|---|
| Call through JNA and UniFFI, not counting the sink body | about 0.75 |
| Name check: one `stat` through MediaProvider, under the folder lock | 0.50 to 0.58 |
| Rename from staging into the landing folder | 0.25 to 0.77 |
| `getExternalStorageState` (binder) | 0.155 |
| `getExternalFilesDir` | 0.15 |
| `readlink` of the descriptor | 0.06 |
| Core: `stat` and `open` of the staged member | 0.06 each |

JNA also attached and detached the thread for every call: `top -H` showed the thread's name counting
up (`Thread-82`, `Thread-194`, and so on), one `java.lang.Thread` per file. More threads alone did not
help the name check: with 4 threads and the `stat` outside the lock it still cost 1.3 ms per file,
so MediaProvider handles those lookups one at a time.

### 13.2 What changed

- **Four sink threads** for bundle members in `Staged` mode (`DeliveryWorker::serve_sink`). One thread
  keeps the log. `Directory` mode stays on one thread: placing costs 0.02 ms there.
- **The checkpoint's flush runs in the background** (`BundleInner::checkpoint_due`). The stream goes on
  while `syncfs` runs; the ledger then records the snapshot taken before the flush began.
- **One folder listing instead of one `stat` per name** when promising a landing name
  (`FolderListings`, `NameClaims.generation`): at most one listing per folder per 2 s. The pick stands
  only if nothing in this process could have created a file in that folder since.
- **The native thread stays attached to the VM** between sink calls (`CallbackThreads`).
- **`getExternalStorageState` at most once a second and the landing root cached.**

Tried and dropped: **writing each bundle chunk while reading the next one.** It looked right on paper
(the stream thread's writes and the wait for the network added up), but 13.3 shows no gain, and it
widened the time the bundle state is off in another thread, so it is gone. FFI batching (one call per
many members) was not tried: it needs an API change, and the threads got most of what it would.

### 13.3 One change taken out at a time

`mixed-5000`, one run per install after a 45 s settle, interleaved, wall seconds. Second runs straight
after a mover are in the notes below, not here. The row with everything is the build that still had
write-ahead, compared with the same build without it.

| Build | Wall s | Median | Against all changes |
|---|---|---|---|
| All changes | 7.27, 7.98, 7.86, 8.09, 8.30 | 7.98 | |
| No write-ahead | 8.10, 7.85, 7.92, 9.18, 7.84 | 7.92 | 0.0 (dropped) |
| No `getExternalStorageState` and root caching | 8.26, 8.32, 8.28, 8.21, 7.81 | 8.26 | +0.3 |
| No JNA keep-attached | 8.44, 8.30, 9.60, 8.64, 8.46 | 8.46 | +0.5 |
| No folder listing | 8.25, 13.25, 9.25, 9.01, 8.54 | 9.01 | +1.0 |
| No background flush | 9.60, 9.93 | 9.77 | +1.8 |
| One sink thread | 10.44, 10.32 | 10.38 | +2.4 |

The caching row is near the noise of these runs. It stays because it is the smallest of the changes
and each of the five runs but one is slower without it; another sitting could overturn that.

### 13.4 Old against new on the final tree (2026-11-02)

Old is the 0.5.0 app, new is the final tree with the path-choice work. Both ran against one daemon, the
final tree's, so the phone side is what differs and "old" is not a full 0.5.0 stack. Each install got a
45 s settle, then the four runs in the table's order. All sums verified on every row here. Three new
rounds and four old rounds completed:

- 15:33 to 16:03: old, new, old, new, old. Desktop load 0.3 to 2.2, no load tests running. The next new
  round was lost to the link (13.7), so it is not in the table.
- 17:33 to 17:45, after the link came back and the phone had cooled and charged (battery 28.6 C): new,
  then old. Load 0.7 to 2.8. Ping just before the runs was 2 to 10 ms from desktop to phone, 2 to 12 ms
  from desktop to router, 4 to 7 ms from phone to router.

| Dataset | 0.5.0 wall s | New wall s | 0.5.0 MiB/s | New MiB/s | In Downloads +s, 0.5.0 | New |
|---|---|---|---|---|---|---|
| `mixed-5000`, first after the install | 18.1, 19.4, 16.8, 14.5 | 8.6, 8.2, 10.6 | 9.1, 8.5, 9.9, 11.4 | 19.3, 20.1, 15.6 | 39.3, 42.8, 81.3, 41.4 | 40.7, 41.5, 40.5 |
| `mixed-5000`, second | 14.0, 14.5, 39.3, 13.6 | 7.4, 16.8, 8.0 | 11.8, 11.4, 4.2, 12.2 | 22.3, 9.9, 20.8 | 39.4, 40.5, 79.4, 40.5 | 39.3, 74.9, 41.5 |
| `medium-100` | 6.8, 7.8, 8.0, 4.4 | 4.0, 8.1, 4.1 | 25.0, 21.6, 21.1, 38.2 | 42.3, 20.8, 41.1 | 0.0 | 0.1, 0.0, 0.0 |
| `big-1g` | 22.1, 22.1, 34.3, 24.7 | 27.3, 34.6, 23.7 | 46.4, 46.4, 29.9, 41.4 | 37.5, 29.6, 43.3 | 0.0 | 0.0 |

An earlier sitting (09:50 to 10:21, a build that still had write-ahead, with another agent's load tests
on the desktop) read the same way: `mixed-5000` first after the install 15.9 to 16.6 s old, 7.7 to 8.3 s
new; into Downloads 37 to 46 s on both.

### 13.5 Reading

- **`mixed-5000` takes about half the time.** First after an install: 14.5 to 19.4 s old, 8.2 to 10.6 s
  new, in one phone state and on one daemon.
- **The 0.8 target was missed.** In the two new rounds with a fast `medium-100` (41 to 42 MiB/s),
  `mixed-5000` at 15.6 to 22.3 MiB/s is 0.38 to 0.53 of it. In the slow round both sit near 20 MiB/s;
  that is a slower `medium-100`, not a better small-file path. Measured in the phone's present state
  only; it was never in the good state of 12.5 during this work.
- **Slow rounds are not explained by desktop load.** The slow period seen from 10:08 came back from
  about 15:50 at load 2.2 or below: `medium-100` near 21 MiB/s and `big-1g` near 30, for both apps, and
  second runs of up to 39 s old and 17 s new, each with a mover of 75 to 81 s. So the earlier guess that
  the load tests may explain them does not hold. The cause is not known. Seen alongside it: the battery
  warmed from 29.4 to 33 C while discharging on USB, and after a pause to 28.6 C the 17:33 rounds were
  fast again. That fits a phone-side cause without showing one. Second runs straight after a mover are
  also slower more often, which fits the indexing guess in 11.2, not measured.
- **`big-1g` is UNEXPLAINED.** In the fast rounds of the 15:33 sitting old beat new, 22.1 and 22.1 s
  against 27.3 s, with no change in this path from the small-file work. The 17:33 pair did not repeat it
  (new 23.7 s, old 24.7 s), which is too few runs to clear it. It was not investigated further, by the
  user's choice to cut testing, so whether the new phone build slows a big file is open. Reading the code
  afterwards found a real gap: a path check on the sending side did not run during a send, but the
  phone's own check was not held back while it received, so its probes could compete with a transfer on the same link. That is
  fixed in the released tree (`a_receive_in_progress_blocks_a_path_check`, after these rows were
  measured). Whether it caused this result is not known, because `big-1g` was not measured again.
- **Into Downloads did not get worse.** Clean runs read about 40 to 41 s on both apps, and both reached
  75 to 81 s in the slow period. This settles the doubt from the earlier sitting.
- **Left on the table:** the staging creates (0.3 ms per member, 1.6 s, on the stream thread and stalled
  by the flush), a flush thread that is nearly always busy (3.9 s out of 4.7 s), the renames at 0.6 ms,
  and about 0.75 ms per call in JNA and UniFFI, which only an API that batches calls would remove.

### 13.6 Durability

Nothing here changes the wire. Tests (`transfer/receiver.rs`):
`a_background_flush_records_only_what_it_flushed` holds a flush back and shows the verified offset
moving in memory while the ledger, the record on disk and the sink stay put, then that the ledger
records only the snapshot, only members inside it are delivered, and a kill reopens at exactly that
offset and resumes to one delivery per member. `a_failed_flush_keeps_its_members_owed` shows a failed
flush records nothing and its members stay owed. `parallel_sink_delivery_survives_a_kill` bounds
second deliveries by the threads.

A failed flush goes back to the last checkpoint instead of trying again in place, because Linux may
mark pages clean when their writeback fails, so a second flush could succeed with the bytes not on
disk: the bytes past that checkpoint leave staging and the ledger, the request fails and the sender
resumes from there (`a_failed_flush_goes_back_to_the_last_checkpoint`,
`a_failed_final_flush_is_received_again`). A member whose data starts exactly at that checkpoint keeps
its empty staging file, by the same rule a load uses (`a_failed_flush_at_a_member_start_can_resume`);
staging that no longer matches means starting again from 0. Streamed files go back to their durable
offset the same way and overwrite what is there
(`a_failed_flush_of_a_streamed_file_goes_back_to_its_checkpoint`).

Behaviour that did change: after a process kill up to four members can reach the sink twice, not one.
On Android each such member then **shows twice in Downloads** (`a.txt` and `a (1).txt`), because the
first copy is already landed and its name promised (up to four such files per kill, none lost, none
overwritten). A kill can also lose up to one more checkpoint of already-received bytes, which are sent
again. Android's sink renames the staged file away, so a kill between that rename and the log line
finds the member missing, and the bundle's bytes start again from 0 with logged members kept (existing
behaviour, now up to four members wide instead of one).

Checked on the A54 with small sends (1,500 files of 4 KiB, 2026-11-02):

- **A listing can miss what a `stat` sees.** A probe build in the app process, with three files
  written by `adb shell` into `Download/PlayDrop` (owner `com.android.shell`; `.txt`, `.bin`, `.jpg`):
  `File.list()` returned none, while `File.exists()` said true, `canRead` false and an `O_EXCL` create
  `EEXIST`. So the shortcut can promise such a name. The outcome is safe: with two such files in the
  destination the send finished, the other app's files were unchanged, ours arrived as
  `c0001 (1).txt` and `c0100 (1).txt` with the right sums, 1,502 files in all, and the mover logged the
  renumbering and recorded the relocation. Until then the promised `content://` location names a file
  the app cannot read, so opening it fails rather than showing the other app's file. The old `stat`
  would have picked the " (1)" name at the start; the file ends in the same place.
- **The native threads stay attached and leave cleanly.** Over two sends, each of the four sink threads
  kept one Java thread name for the whole transfer (`Thread-60` to `Thread-63`) where 0.5.0 counted up
  once per file. They were gone after the transfer, logcat had no `Native thread exiting`, JNA or fatal
  line, and the process stayed the same.

- **Listing a large folder.** `ls -f` over shared storage took 0.39 s for 5,000 names (0.10 s for
  1,500), about 80 us per name against 0.5 ms per `stat`. Listing is at most once per 2 s and folder,
  one thread at a time, and pays until about 18,000 names at full delivery speed; a folder of more than
  10,000 names keeps no listing, is listed again only every minute, and goes the `stat` way.

### 13.7 What was checked on the final build, and what was not

**Two small files, 2026-11-02, 17:45.** New app, new daemon at debug level, USB tethering off:
`p11-a.bin` (300,000 bytes, sha256 `24cbb3c62c74119b4999b04d23a901ea6f32df1bf256be3682cef14e754ebaef`)
and `p11-b.bin` (1,500,000 bytes, sha256
`b0e8823dba1556b7cbae4cfd0bcd28474a08974b92d808cd54f65b190e22bd87`) both completed, and the sums on the
phone match.

**The same two files on the released code, 18:10.** After the fix noted in 13.5, version 0.6.0 of both the
app and the daemon: both completed with the same sums, and the phone stayed `online` on the same address.
Nothing else was run on this build.

**One path.** `playdrop peers`, read every 2 s for 90 s through those sends, said `online` on the same
address every time (one empty first reading while the daemon was starting). The daemon's debug log has
one path check, 0.5 s after it started, set off by an mDNS advert from an address that was not the one
in use; it lasted 0.66 s and ended `path kept`, with no switch and no check during the sends. This is
the daemon's side only: the phone's release log does not show its checks.

**The "kept ... MiB/s" line.** The phone has one radio but answers on three addresses: link-local IPv6,
IPv4-mapped IPv6 and plain IPv4. The check treats each as a candidate and probes it, up to 4 MiB each,
and `playdrop diagnose` lists the rates: `kept 43 s ago; [fe80::...%3]:42187 35.2 MiB/s,
[::ffff:192.168.86.74]:42187 35.8 MiB/s, 192.168.86.74:42187 32.8 MiB/s`. A switch needs a clearly higher
rate than the path in use, so these stay put. The 46.0 MiB/s seen earlier was the same kind of reading.
So one-path hosts do carry a little probe traffic at a check; it is three names for one radio.

**Link losses.** Twice the phone and desktop stopped reaching each other with no Wi-Fi toggle by anyone
here: at about 12:18 and about 16:03 on 2026-11-02. Both came on the first `mixed-5000` soon after an app
install, after 30 minutes to 2 hours of runs. Both sides stayed on the same access point
(`B4:23:A2:6D:DA:5D`, channel 149), each still reached the router, ARP entries looked fine, and ping
between them failed both ways. Desktop to router read 84 to 200 ms in the outages against 2 to 12 ms
normally, while phone to router stayed at 6 to 12 ms, so the desktop's radio may be involved as well as
the mesh isolation of 9.4. Each send waited its 10 minute window and ended `failed`, at 740 and 622 s;
those rows are out of the tables. Each cleared only when the user rejoined the phone's Wi-Fi. Two events
are too few to name a cause.

**Not run on the phone on the final build:**

- **Force-stop resume.** Only the desktop tests in 13.6 cover these fixes, in-process. The phone runs in
  8.5 and 11.3 were on older builds with one delivery thread.
- A separate old-peer check. The old rounds above show the 0.5.0 app staying connected through the 45 s
  settle and transferring with the new daemon, and no more than that.

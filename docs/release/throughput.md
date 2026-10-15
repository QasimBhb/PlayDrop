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
| No throughput drop between 100 and 5,000 files | **Pass on the engine, fail to the phone** | desktop to desktop: 5,000 files at 170 to 221 MiB/s vs 100 files at 93 to 105 MiB/s. Desktop to phone after the sink rewrite (section 7): 1.9 vs 39.8 MiB/s, from a 13 ms per-file floor that is one MediaProvider insert per file landing in Downloads; a product decision (section 7.3) |
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
| Release-app PlayDrop rows | `cd apps/android && ./gradlew :app:assembleRelease`, install, then `bench/desktop-to-phone.sh --runs 3` | release signing key; accept a re-pair if the key differs |
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
request per file, so it is expected to sit at or below this floor.

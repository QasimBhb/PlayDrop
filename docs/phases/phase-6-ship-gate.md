# Phase 6 handoff: ship gate

Reader: a fresh session that must either close the items still owed here or start Phase 7
(later work). Read `CLAUDE.md`, `PLAN.md` ("Build phases" row 6, "Speed targets",
"Anti-patterns"), `docs/threat-model.md` (SR-33, SR-34), then `docs/release/*.md`, then
`docs/phases/phase-5-companion.md` sections 1.1 and 8, and this file.

---

## 1. Status

| Item | Value |
|---|---|
| Base | `main` at `b1f7195` (Phase 5 complete); this phase is one commit on top |
| Built on | This Pop!_OS COSMIC machine, 2026-10-13, one session: serial IPC and Gradle steps, six parallel agents (fuzz, release CI and packaging, daemon and window, Android, iOS, bench and release docs), then one Android sink agent against the Galaxy A54, then integration |
| Distribution decision | No Play Store, no App Store. GitHub Releases only: signed APK (Obtainium-friendly), `.deb`, single-file `.flatpak`, Inno Setup installer, unsigned `.ipa` for sideloading. `docs/release/distribution.md` |
| Release signing | Android release key generated 2026-10-13, kept outside the repo at `~/.local/share/playdrop-release/playdrop-release.jks` (mode 600, properties backup next to it); `apps/android/keystore.properties` points at it and is gitignored. **Must be backed up offline.** `versionCode` and `versionName` now derive from `[workspace.package]` |
| CI | `.github/workflows/windows.yml` (MSVC build, tests, clippy `-D warnings`, Inno Setup installer artifact), `release.yml` (tag `v*`: deb, flatpak, android, windows, ios, `SHA256SUMS`, GitHub release; manual run is a dry run), `fuzz.yml` (nightly, 10 min per target, corpus cached). None has run yet: the first push proves them |
| Packaging | `.deb` built and inspected (`desktop-file-validate`, `appstreamcli validate --pedantic` clean); Flatpak built offline from `cargo-sources.json` (753 crates, committed), installed, `playdropd --help` ran, uninstalled again; MPRIS talk-name added; Windows installer script fixed for strict mode, unbuilt |
| Core | `media_command` IPC request (`docs/ipc.md`); `#[cfg(fuzzing)] fuzz_api` and `discovery::mdns::txt_advert` so the live browser and the fuzzer read TXT the same way; workspace `check-cfg` for `fuzzing`; `fuzz/` excluded from the workspace |
| Daemon | Flatpak start at login through the Background portal (`autostart/background.rs`, refusal surfaces in a notification, Diagnose and the window); Diagnose notes port 1716 held by KDE Connect or GSConnect as fine; coexistence test `tests/coexistence.rs` |
| Window | Per-player media rows (previous, play/pause, next, volume) sending `media_command`; media refusal text; "Can't find your device?" panel (`ui/help.slint`) from Devices, Add device and Diagnostics, inline with the likely fix first when Diagnose names a failed layer |
| Android | `DownloadsSink` (direct writes under `Download/PlayDrop` on Android 11+, batched media scan, `fdatasync` only above 8 MiB; MediaStore `IS_PENDING` kept for Android 10, app folder for 8 and 9); `TetherWatcher` so a hotspot or USB tether triggers `networkChanged()`; "Can't find your computer?" screen with a tethering-settings button; media refusal message; release build signed with the release key, `-Pplaydrop.signWithDebugKey=true` for test phones |
| iOS | `ConnectivityHelpView` (same help, Personal Hotspot and USB text), `PeerRefusals` shared mapping with a 5 s throttle, `hasMulticastEntitlement` read from the embedded profile (free-team builds say address or QR is the normal path), `InterfaceKinds.carriesLanPeers` Linux-tested, App Store copy marked as kept for later |
| Fuzzing | `fuzz/` with seven targets (beacon, mdns_txt, manifest, bundle, frame, session_json, qr_uri), 27 committed seeds, README; the 8 CPU-hour gate passed on every target after one fix, see 1.2 |
| Bench | `bench/` scripts: datasets, loopback, iperf3 to the phone (NDK-built iperf3 in `/tmp/pd-bench-tools`), desktop to phone through the daemon (`pd_ipc.py` passes fds over D-Bus), `incumbents.md` for the manual LocalSend and KDE Connect runs |
| Rust tests | 390 passed, 0 failed, 8 ignored (`cargo test --workspace --no-fail-fast`; up from 381); `cargo clippy --workspace --all-targets` zero warnings; `cargo fmt --all --check` clean |
| Android tests | 110 JUnit (up from 89); `:app:compileDebugKotlin` and `:app:assembleRelease` build |
| iOS checks | `apps/ios/scripts/check-linux.sh` exit 0; `PlayDropShared` 102 XCTests on Linux (up from 92); SwiftUI views compile only on GitHub's macOS CI |

### 1.1 Gate results (PLAN.md row 6)

| Check | Result |
|---|---|
| Matrix published | `docs/release/test-matrix.md` (platforms, every row 6 check, earlier gates, evidence or "owed"); `docs/release/throughput.md` (method, link, iperf3, PlayDrop, targets); `docs/release/sr-checklist.md` (SR-1 to SR-34 with verification and a sign-off column, hostile-LAN plan written, not run) |
| Throughput vs iperf3 | **Pass.** Release app, Galaxy A54, 5 GHz Wi-Fi: 1 GiB at 43.7 and 41.4 MiB/s against iperf3 44.2 (94 to 99%; the 80% stretch target is met). The debug app measured 22% earlier the same day: the Gradle debug variant compiles the core without optimisation, so never judge speed from a debug APK |
| No drop between 100 and 5,000 files | **Pass on the engine** (desktop to desktop: 5,000 files at 170 to 221 MiB/s vs 100 at 93 to 105). **Fail to the phone**: 1.9 vs 39.8 MiB/s after the sink rewrite (was 0.72 vs 15 with the old sink). The 13 ms per-file floor is one MediaProvider insert per file landing in Downloads: direct File writes go through FUSE and trigger the same insert, parallel writers serialise, `.nomedia` does not help. Product decision open, section 6 |
| LocalSend and KDE Connect columns | **Dropped by the user** (2026-10-14): KDE Connect was unreliable for them and they do not want to operate LocalSend for a comparison. Method kept in `bench/incumbents.md` |
| A pinned Android device receives with the screen off | Pass (bench runs of `tree` and `medium-100` with the screen off, same rates and sums) |
| Multi-file counts match | Pass: every bench run checked counts and sha256 sums on the phone (1 to 5,000 files) |
| Resume works | Carried from Phase 1 (gate A) and Phase 3; not re-run over Wi-Fi this phase |
| Empty lists are diagnosed | Pass by code and screenshots: Diagnose names the layer; the window, Android and iOS now show the help panel with the matching fix (hotspot or USB for "no interface on the same network", QR or address for "multicast blocked", the firewall command for a blocked port) |
| iOS copy is accurate | Copy kept with a note that the app is sideloaded today; no listing |
| Official KDE Connect can stay installed alongside | Pass by test (`coexistence.rs`: no socket on 1714-1764 or 53317, 224.0.0.167 not joined) and by the 1716 note in Diagnose; the phone has run the official KDE Connect app beside PlayDrop through every bench run. Live desktop run dropped with the incumbents run |
| Guest Wi-Fi path (QR, hotspot, USB tether) works and is documented | Documented on all three platforms with the OS menu paths; Android opens the tethering settings; a hotspot or tether now wakes discovery on Android. **Not exercised on a real hotspot or cable** (agents were told not to change phone settings) |
| Windows 11 | CI only; every live item (receive while minimised, 30 ms trackpad gate, `SendInput`, GSMTC, installer, firewall rule) still owed, `test-matrix.md` section 4 |
| Android 14 and 15 | Not covered: no emulator images in the SDK and the user is not in the `kvm` group; setup in `test-matrix.md` 1.1 |
| Mesh, VPN | Not run |

### 1.2 SR-33 fuzz gate (8 CPU-hours per target)

Run on 2026-10-13 from the scratchpad script: one target at a time, `-fork=8`, 3600 s,
`-rss_limit_mb=2048 -timeout=10`, corpus plus seeds. The binaries must be run from a path
without spaces (`/tmp/pd-fuzz-src` symlink) and with an explicit spaceless
`-artifact_prefix`; fork mode re-execs argv[0] unquoted (`fuzz/README.md`).

| Target | Result |
|---|---|
| beacon | Pass: 1 h, about 35k exec/s per core, 0 crashes. Six `timeout-*` files all replay in 40 ms; they landed in the minute a full `cargo test --workspace` and a Gradle release build ran, so they are load stalls |
| mdns_txt | Pass: 1 h, about 28k exec/s per core, 0 crashes, 0 timeouts |
| manifest | Pass: 1 h, about 38k exec/s per core, 0 crashes, 0 timeouts |
| bundle | Pass: 1 h, about 6k exec/s per core, 0 crashes, 0 timeouts |
| frame | Pass: 1 h, about 10k exec/s per core, 0 crashes, 0 timeouts |
| session_json | **Found one bug** after 4 minutes: `sanitize_name` trimmed before cutting to 64 characters, so a name cut on a space kept a trailing space (SR-8 cosmetic, no trust impact). Fixed in `session/hello.rs` (trim after the cut), unit test added, the input is committed as `fuzz/seeds/session_json/hello-name-cut-on-space`. Full hour re-run started 2026-10-14 01:02 after `qr_uri`; result below |
| qr_uri | Pass: 1 h, about 118k exec/s per core, 1.9 billion executions, 0 crashes, 0 timeouts |

| session_json re-run | Pass: 1 h with the fix, about 7k exec/s per core, 0 crashes, 0 timeouts |

All seven targets hold the 8 CPU-hour gate; SR-33 is met for this release.

---

## 2. Repository map (new or changed in Phase 6)

```
.github/workflows/{windows,release,fuzz}.yml
fuzz/                                        cargo-fuzz workspace: Cargo.toml, fuzz_targets/*.rs, seeds/, make-seeds.sh, README.md
bench/                                       make-dataset.sh, loopback.sh, iperf3-phone.sh, desktop-to-phone.sh, pd_ipc.py, incumbents.md, README.md
docs/release/{distribution,test-matrix,throughput,sr-checklist}.md
packaging/flatpak/{cargo-sources.json,update-sources.sh,README.md,org.playdrop.PlayDrop.yml}
packaging/linux/{org.playdrop.PlayDrop.desktop,org.playdrop.PlayDrop.metainfo.xml}
packaging/windows/build-windows.ps1
crates/playdrop-ipc/src/lib.rs               Request::MediaCommand
crates/playdrop-core/src/lib.rs              fuzz_api (cfg(fuzzing))
crates/playdrop-core/src/discovery/mdns.rs   txt_advert
crates/playdropd/src/autostart/background.rs Background portal proxy
crates/playdropd/src/{daemon,probe}.rs       media_command handler, 1716 note
crates/playdropd/tests/coexistence.rs
apps/desktop/ui/help.slint, devices.slint, types.slint (MediaRow), diagnostics.slint, add-device.slint
apps/desktop/src/{model,main}.rs, screens/{devices,diagnostics,settings}.rs
apps/android/app/build.gradle.kts            version from Cargo.toml, release signing, signWithDebugKey
apps/android/.../core/{DownloadsSink,ReceivePolicy,SinkTimings,FileNames,CoreHostImpl}.kt
apps/android/.../service/TetherWatcher.kt
apps/android/.../ui/help/{CantFindScreen,HelpTopic}.kt, ui/device/MediaRefusal.kt, ui/diagnostics/*
apps/ios/PlayDrop/UI/Settings/{ConnectivityHelpView,ConnectivityHelpStrings}.swift
apps/ios/Packages/PlayDropShared/.../{ConnectivityHelp,PeerRefusals,EmbeddedProvisioningProfile}.swift
Cargo.toml                                   exclude fuzz, check-cfg fuzzing
```

---

## 3. Public API

No change to `docs/core-api.md`; `playdrop-ffi` stays 0.2.0. IPC adds one request:

| Surface | Addition |
|---|---|
| IPC | `media_command { peer, player, action, value? }` answering `done`; refusals arrive as the existing `peer_refused` event |
| Android build | `-Pplaydrop.signWithDebugKey=true`; env `PLAYDROP_KEYSTORE_FILE`, `PLAYDROP_KEYSTORE_PASSWORD`, `PLAYDROP_KEY_ALIAS`, `PLAYDROP_KEY_PASSWORD` for CI |

---

## 4. Decisions made in Phase 6

| Decision | Why |
|---|---|
| GitHub Releases only, no stores | The user's call: personal use first, store listings are not worth the effort today. Obtainium gives Android users updates from releases; F-Droid is the later step (`distribution.md`) |
| One Android release key, generated now and kept outside the repo | Sideloaded updates only install over the same signer; the key had to exist before the first public APK |
| `versionCode` from the workspace version (`major * 10000 + minor * 100 + patch`) | Every artifact of a release carries one version; sideloaded updates stay installable |
| Android receive writes directly under `Download/PlayDrop` on 11+ | Cuts the per-file cost from 35 to 13 ms; the MediaStore `IS_PENDING` path stays for Android 10 where direct writes are refused |
| Small received files are not `fsync`ed on Android (threshold 8 MiB) | The core already synced the staging copy; a small-file fsync cost 1.5 to 2.3 ms each on this phone and Java has no `syncfs` |
| Fuzz targets live in their own workspace under `fuzz/` | The main build never needs nightly or libFuzzer; `cfg(fuzzing)` opens one `fuzz_api` module |
| Fuzz runs skip cargo-fuzz for the long gate on this machine | Fork mode cannot cope with the space in the checkout path |
| Flatpak start at login through the Background portal, refusal turns the setting off and says so | Anti-pattern 18; the window had no IPC change available, so it infers a refusal from the setting flipping back within 5 minutes |
| Windows stays CI-only | No machine; `playdrop-ffi` (mobile only, uses `std::os::fd`) is excluded from the Windows job |
| Android small files ship at the MediaProvider floor (decided 2026-10-14) | Big and medium files pass the gate; floods of thousands of tiny files are rare in daily use and the two ways below the floor (app-folder landing, archive threshold) are product changes for Phase 7. LocalSend pays the same insert plus a request per file, so it is expected at or below the same floor (not measured, user choice) |

---

## 5. Open TODOs

| File or area | What | Phase |
|---|---|---|
| `apps/android/.../core/DownloadsSink.kt` | 13 ms per file floor in Downloads; shipped as is, see section 6 for the two ways below it | 7 |
| `crates/playdrop-core/src/transfer/receiver.rs` | Big-file delivery copies staging to Downloads across mounts on Android (4.7 s of 24.7 s for 1 GiB); staging on the same mount or delivering while streaming would save it | 7 |
| `crates/playdrop-ipc` | `playdrop send --daemon <peer> <paths>` CLI over `Client::send_files` would replace `bench/pd_ipc.py` and work on Windows | 7 |
| `apps/desktop/src/screens/settings.rs` | Start-at-login refusal is inferred; an IPC field would be exact | 7 |
| `docs/release/sr-checklist.md` | SR-32 canary-log CI test missing; SR-30 iOS URL-scheme rule has no test; SR-34 external review and hostile-LAN run not done | 7 |
| `apps/ios/project.yml` | `MARKETING_VERSION` fixed at 0.1.0; the release workflow overrides it from `Cargo.toml` | 7 |
| Windows | Every live item | when a machine exists |
| Phase 4 carry-over | Share-extension gate needs an App Group; SideStore or AltStore may register one for the unsigned IPA, unverified | later |

---

## 6. Known gaps and risks

- **Small files on Android.** After the rewrite, 5,000 files of 4 to 64 KiB arrive at
  1.9 MiB/s while 100 medium files arrive at 39.8 MiB/s. The floor is one MediaProvider
  insert per file for anything that lands in the public Downloads folder; parallel
  writers, `.nomedia`, hidden folders and MediaStore-row-first all measured the same or
  failed. Three ways below it, all product changes: (1) receive into the app's own folder
  (0.2 ms per file) exposed through a PlayDrop entry in the Files app, then move into
  Downloads as a background job; (2) deliver very large counts as one archive; (3)
  `MANAGE_EXTERNAL_STORAGE`, unverified and policy-limited. Decided 2026-10-14: ship as
  is, revisit in Phase 7. LocalSend is not measured (user choice, `bench/incumbents.md`
  keeps the method); it pays the same insert plus a request per file.
- **Nothing in CI has run.** Windows link, MSVC clippy at `-D warnings`, the installer,
  the Flatpak in the container, the Android job with secrets, the unsigned iOS archive:
  all proven only by the first push and a manual `workflow_dispatch` dry run.
- **Guest Wi-Fi paths are documented and wired but not exercised** on a real hotspot or
  USB tether; `TetherWatcher` relies on hidden platform broadcast actions, with the core's
  interface poll as the fallback.
- **Phone screens.** Devices, device detail with media buttons, Diagnostics and "Can't find your
  computer?" checked on the A54 on 2026-10-14; the empty state after forgetting a device is
  unchecked. iOS screens were only type-checked.
- **Windows, Android 14 and 15, mesh, VPN, resume over Wi-Fi**: owed, listed in
  `test-matrix.md`.

---

## 7. What the user must do before the first release

1. Back up `~/.local/share/playdrop-release/` offline. Done 2026-10-14.
2. `git update-index` already marks the scripts executable; commit, then
   `git push origin main` and watch the Windows and iOS workflows.
3. Set the four secrets with the commands in `docs/release/distribution.md`
   ("Putting the key into GitHub secrets"), then run `Release` by hand as a dry run.
4. Tag `v0.1.0` when the dry run is green.

The incumbents run (`bench/incumbents.md`) was dropped by the user on 2026-10-14.

---

## 8. Phase 7 kickoff brief

PLAN.md row 7 ("Later") plus what this phase left: pick from section 5 by value. The
likely first items are the Android small-file floor (section 6, app-folder landing or an
archive threshold), the `playdrop send --daemon` CLI, staging on the same mount on Android, and the SR-32 canary test. iroh,
Local Push Connectivity, Wi-Fi Aware, the libcosmic applet and MSIX stay behind flags.

---

## 9. Rules carried forward

- Hyphen only. Never the long dash, in code, comments, docs or commits.
- No AI attribution anywhere: no co-author trailers, no "generated with" lines, no session links.
- Agents do not commit or push. The user commits and runs `git push origin main`.
- Parallel agents never edit any `Cargo.toml`, Gradle file, `Package.swift` or `project.yml`.
- Never bind or announce on 1714-1764, 53317 or 224.0.0.167.
- Manual runs use data and receive dirs on ext4 (`/tmp/pd-*`), never under the repo, and
  `HOME=/tmp/pd-home` or `--no-autostart`. Shut down a daemon left from an earlier session
  over D-Bus before starting a new build.
- Never judge phone speed from a debug APK; use `assembleRelease -Pplaydrop.signWithDebugKey=true` on a test phone.
- Subagents: Opus only for core, protocol or cross-cutting daemon work; Sonnet for UI
  screens, scripts, CI and docs; Haiku for lookups.
- `docs/core-api.md` is frozen: a change needs a note in the phase handoff and an ffi version bump.
- Every phase ends by writing `docs/phases/phase-N-<name>.md` in this same shape.

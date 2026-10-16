# Test matrix

Phase 6 ship gate (PLAN.md "Build phases", row 6). One rule: a cell holds a result only if
something was run and the evidence is named. Anything else says **owed**, with what it needs.
Dates are 2026; the evidence column names the test, script or phase document.

Status words: **Pass** (run, passed), **Partial** (part run, the rest named), **CI only**
(compiled and unit-tested on a hosted runner, never run by a person on that OS), **Owed**
(not run), **n/a** (does not apply).

## 1. Platforms

| Platform | Hardware here | Coverage |
|---|---|---|
| Pop!_OS COSMIC (Wayland) | This desktop, 16 cores, Wi-Fi client | Full: daemon, window, tray, clipboard, input portal, MPRIS; GitHub Actions `ubuntu-24.04` also runs format, clippy, build and tests on every push (`.github/workflows/linux.yml`: proves the Linux code compiles and its non-desktop tests pass, nothing that needs a screen) |
| Windows 11 | None. GitHub Actions `windows-latest` only (`.github/workflows/windows.yml`: build, test, clippy, Inno Setup compile) | **CI only.** The first real test run on 2026-10-14 failed (file delivery `Access is denied`, one send timed out, two gates hung); fixed the same day, the fourth run is green with gate B ignored there (`phase-6-ship-gate.md` section 6). Everything that needs a person at the screen is owed (section 4) |
| Android 16 (API 36) | Galaxy A54 (SM-A546U1), on adb, same Wi-Fi | Real device; GitHub Actions (`.github/workflows/android.yml`) regenerates the UniFFI bindings, compiles the Kotlin and runs the JUnit tests on every push, nothing on a device |
| Android 15 (API 35) | None | Owed. See 1.1 |
| Android 14 (API 34) | None | Owed. See 1.1 |
| iOS | An iPhone, built and installed from Linux with xtool on a free personal team (no App Group, no multicast; pair by address or QR). GitHub macOS CI for compile and unit tests | Partial: four of five Phase 4 gates passed 2026-10-12 |
| Ubuntu GNOME | None | Best effort, never run (Phase 2 "Not run"). No clipboard watcher on GNOME by design (in-window button) |

### 1.1 How Android 14 and 15 would be covered

The SDK on this machine (`/media/calliope/New Volume/Android/Sdk`) has the emulator binary
and platforms 36, 36.1 and 37.0, but **no system images** (`system-images/` does not exist),
and the user is not in the `kvm` group (`/dev/kvm` is `root:kvm`, mode 660), so a local x86
emulator would be unusably slow until that changes. Two routes, neither done:

1. Local: `sdkmanager "system-images;android-34;google_apis;x86_64" "system-images;android-35;google_apis;x86_64"`,
   `sudo usermod -aG kvm $USER` and a re-login, `avdmanager create avd`, then run the
   instrumented checks. Emulator Wi-Fi is NATed, so pairing is by typed address
   (`adb forward`/`adb reverse`) and discovery cannot be tested there.
2. CI: an emulator job on a hosted Linux runner (KVM available) for API 34 and 35 that
   installs the debug APK and runs the JUnit suite plus a foreground-service start/stop
   smoke test. Same limit on discovery.

What the emulator can show for 14 and 15: the app installs and starts, the `connectedDevice`
foreground service starts with its permission (`FOREGROUND_SERVICE_CONNECTED_DEVICE`),
notifications and the share sheet behave. What it cannot show: screen-off receive under real
Doze and OEM battery rules, Wi-Fi discovery, throughput. Say "emulator only" in the matrix
when that is all there is.

## 2. PLAN.md row 6 checks

| Check | Pop COSMIC (with the Android 16 phone) | Windows 11 | Android 14 / 15 | Android 16 | iOS | Ubuntu GNOME |
|---|---|---|---|---|---|---|
| A pinned Android device receives with the screen off | Pass | n/a | Owed | **Pass** | n/a (foreground only) | Owed |
| Multi-file counts match | **Pass** | Owed | Owed | **Pass** | Partial | Owed |
| Resume works | Pass (loopback and phone over Wi-Fi, 2026-10-15) | Owed | Owed | **Pass** | Owed | Owed |
| Empty device lists are diagnosed | Partial | Owed | Owed | Partial | Partial | Owed |
| Official KDE Connect installed alongside | Partial | Owed | Owed | Partial | Owed | Owed |
| Guest Wi-Fi path (QR, hotspot, USB tether) works and is documented | Partial | Owed | Owed | Partial | Partial | Owed |
| Mesh Wi-Fi | Partial | Owed | Owed | Partial | Owed | Owed |
| VPN on phone or PC | Owed | Owed | Owed | Owed | Owed | Owed |
| iOS copy is accurate | n/a | n/a | n/a | n/a | Owed | n/a |

### 2.1 Evidence for each cell

| Check | Evidence |
|---|---|
| Screen-off receive, Android 16 | 2026-10-13: with the screen off (`input keyevent 223`, `mWakefulness=Dozing`) the daemon sent `tree` (160 files) and `medium-100` (100 files, 168.8 MiB) to the pinned A54; both complete, every file and sha256 matched on the phone (`bench/desktop-to-phone.sh`). Earlier: Phase 3, 400 MiB with the screen off, SHA-256 equal (`docs/phases/phase-3-android.md` 1.1); forced 10-minute deep idle kept the session (`dumpsys deviceidle force-idle`). Not done: a natural 30-minute Doze, an overnight run |
| Multi-file counts | 2026-10-13: 1, 100, 160, 1,000 and 5,000 files desktop to phone, all counts and sums matched (`docs/release/throughput.md` 2.2). Desktop to desktop: gate B (`gate_b_many_small_files_hold_throughput`), `playdrop-core` `receiver_full_flow_with_bundle_and_reload`. Empty folders are recreated on Android 11 and later since the sink rewrite (`DownloadsSink.kt`); Android 10 keeps none. iOS: camera-roll dump of 6 photos intact (Phase 4 1.1), no large count run |
| Resume | Loopback: `gate_a_kill_and_resume_500mib` and `duplex_kill_and_resume_64mib` (hash matches, no re-pair); SR-21 tests `stream_source_resume_check`, `resume_decisions`, `outbox_range_and_prefix`. Phone, 2026-10-15: 1 GiB over Wi-Fi, `am force-stop` at 50%, relaunch after 8 s, done 21.6 s later, sha256 equal, one file (`throughput.md` 8.5). A Wi-Fi drop mid-transfer is still owed. Sender-side resume across a sender restart is out of v1 (`docs/core-api.md` 8) |
| Empty lists diagnosed | Daemon: `probe.rs` tests `ufw_blocking_the_port_is_named_with_the_fix`, `multicast_and_peer_failures_name_the_layer`, `listener_down_comes_first`, `healthy_system_says_all_paths_fine`. Core: diagnostics per peer (`PeerDiagnostic`). Android: `CantFindScreen.kt`, `HelpTopic.kt`. iOS: Local Network fix screen passed on the iPhone 2026-10-12; `DiagnosticsSummaryTests`, `ConnectivityHelpTests`. Owed: the real thing with `ufw` enabled and the port closed (Phase 2 "partially verified"), multicast blocked, client isolation |
| KDE Connect alongside | Ports: `tests/coexistence.rs` (`constants_stay_clear_of_kde_connect_and_localsend`, `daemon_never_binds_or_dials_kde_connect_or_localsend_ports`) and gate E on the wire (`gate_e_no_kde_ports_and_no_kdeconnect_on_the_wire`). The phone already runs the official KDE Connect app (`org.kde.kdeconnect_tp`) beside the PlayDrop app, and all of 2026-10-13's transfers ran with both installed; the PlayDrop app did not conflict. The desktop `kdeconnect` package is removed (`dpkg` state `rc`). The desktop live run (`sudo apt install kdeconnect`, both daemons up, send with each) was dropped by the user on 2026-10-14 with the incumbents run; `bench/incumbents.md` keeps the steps |
| Guest Wi-Fi path | Pairing by QR: `qr_pairing_and_single_use_secret`, `full_qr_pairing`, `qr_wrong_secret_and_expiry`; by typed address: used for every iPhone gate. Last-known dial: `verified_uses_source_ip_and_stored_port`. Owed on a real client-isolated network: QR pairing, phone hotspot (desktop joins the phone's hotspot) and USB tether (adb reverse or `usb0` interface) transfers, each timed and written up. The in-app help exists (Android `ui/help`, iOS `ConnectivityHelp`); a user-facing page under `docs/` does not yet |
| Mesh | The home Wi-Fi ("Unreal") is the mesh network named in Phase 5 section 1.1; every 2026-10-13 run and the Phase 3 and 5 live checks used it, both devices on one node (same BSSID). Owed: phone and desktop on different mesh nodes, and the 10 s empty-window beacon burst re-measured ("written, not re-measured on the mesh Wi-Fi", Phase 5) |
| VPN | Owed. Plan: desktop on a VPN with LAN access, then phone on a VPN; expect mDNS and beacons to fail and last-known dial, QR and typed address to work |
| iOS copy | `apps/ios/AppStore/copy.md` written; the review against shipped behaviour waits for the share-extension gate, which needs an App Group. Distribution is GitHub releases only now (no stores), so the copy is for the README and release notes |

## 3. Earlier phase gates, re-checked where possible

| Phase | Gate | Status | Evidence |
|---|---|---|---|
| 1 | A: 500 MiB kill and resume, hash equal | Pass (test) | `phase1_gates.rs` `gate_a_kill_and_resume_500mib` |
| 1 | B: 5,000 small files hold throughput | Pass (test, load-sensitive) | `gate_b_many_small_files_hold_throughput`: 0.88 release (Phase 1), 0.80 release (Phase 5 run under load) |
| 1 | C: h2 vs raw, iperf3 recorded | Re-run 2026-10-13 | `bench/loopback.sh`: h2 1,857, raw 2,870 MiB/s (`throughput.md` 3.1) |
| 1 | D: restart needs no re-pair | Pass (test) | `gate_d_restart_reconnects_without_pairing` |
| 1 | E: no KDE or LocalSend ports on the wire | Pass (test) | `gate_e_no_kde_ports_and_no_kdeconnect_on_the_wire` |
| 2 | COSMIC receives with the window closed | Pass (Phase 2, 2026-10-08) | `docs/phases/phase-2-desktop.md` (gate table) |
| 2 | Desktop copy reaches a peer within 1 s | Pass (Phase 2): 2 to 5 ms | same |
| 2 | Daemon restart needs no re-pair | Pass | same; also `gate_d` |
| 2 | Windows 11 receives minimised | Owed | never run |
| 2 | Ubuntu GNOME window plus notification | Owed | never run |
| 3 | Share sheet to default device, one tap | Pass (Phase 3, 2026-10-09) | `phase-3-android.md` 1.1 |
| 3 | "Send clipboard" notification action | Pass (Phase 3) | same |
| 3 | Reconnect after a Wi-Fi drop, no re-pair | Pass (Phase 3): 4 s | same. Re-seen 2026-10-13: a freshly started daemon had the phone online within seconds, no re-pair |
| 3 | Destroying the Activity keeps the service | Pass (Phase 3) | same |
| 3 | Natural 30-minute Doze; idle battery cost | Owed | forced 10-minute idle only; battery not measured |
| 4 | Camera-roll dump, receive while open, pull on open, Local Network fix | Pass (2026-10-12, iPhone via xtool) | `phase-4-ios.md` 1.1 |
| 4 | Share-extension queue handoff | Owed | blocked: free team has no App Group |
| 5 | Portal consent dialog, input moves the pointer | Pass (2026-10-13) | `phase-5-companion.md` section 7 |
| 5 | Android trackpad over Wi-Fi | Pass, no visible lag, no number | same. A latency figure is owed |
| 5 | Trackpad under 30 ms on Windows | Owed | loopback stand-in 0.12 ms release (`gate_input_latency_on_loopback`) |
| 5 | iOS trackpad, keyboard, presenter on the iPhone | Owed | Linux checks only |
| 5 | Media control (MPRIS) | Pass (2026-10-12/13 on COSMIC) | `phase-5-companion.md` 1.1; GSMTC on Windows is CI only |
| 7 | Bulk drop lands at engine speed, counts and sums match | Pass (2026-10-15), gate ratio 0.2 (partial) | `throughput.md` 8.1: `mixed-5000` 7.2 to 9.5 MiB/s (was 1.9), all 15 runs verified |
| 7 | Rename through FUSE into Downloads | Measured and removed | `throughput.md` 8.3: a copy behind the scenes, slower and not atomic |
| 7 | 1 GiB resume by force-stop on the phone | Pass (2026-10-15) | `throughput.md` 8.5 |
| 7 | Landing notification flow | Pass (2026-10-15) | "Moving 202 of 5,000 files into Downloads", survives a force-stop (sweep resumes at "Moving n of 4,779"), ends "4779 received files moved into Downloads"; read from `dumpsys notification` |

## 4. Windows 11 live items (all owed, need a machine)

Installer runs and adds the program-scoped firewall rule; daemon starts at login; receives
while minimised and on a locked screen policy; tray; clipboard watcher with
`ExcludeClipboardContentFromMonitorProcessing` (SR-24); `Zone.Identifier` on received files
(SR-23); named pipe and its DACL (SR-27); DPAPI key storage (SR-28); exclusive bind
(SR-31); `SendInput` and the 30 ms trackpad gate; GSMTC media; `WDA_EXCLUDEFROMCAPTURE` on
the QR window (SR-9). What the runner already gives: the workspace compiles and links with
MSVC, the unit and loopback tests run, clippy runs with warnings denied, the Inno Setup
script compiles. No run result was checked in this pass (the GitHub API answered 404 from
this machine).

## 5. What was run on 2026-10-13 for this document

- `bench/make-dataset.sh`, `bench/loopback.sh`, `bench/iperf3-phone.sh`,
  `bench/desktop-to-phone.sh` (screen on and screen off), daemon to second host transfers.
  Numbers and commands are in `docs/release/throughput.md`.
- The Android app on the phone was a debug build; see the caveat there.

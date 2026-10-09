# Phase 3 handoff: core API freeze, UniFFI, Android host, session plane

Reader: a fresh session that must build Phase 4 (iOS) without re-reading the Android or
core code. Read `CLAUDE.md`, `PLAN.md` ("Build phases", "Anti-patterns"), `docs/protocol.md`,
`docs/threat-model.md`, `docs/core-api.md` (the frozen surface iOS binds), then
`docs/phases/phase-2-desktop.md` sections 2 to 4 and this file. `docs/ipc.md` stays
normative for the desktop IPC.

---

## 1. Status

| Item | Value |
|---|---|
| Base | `main` at `1b62b0f` (2026-10-08); this phase is one commit on top |
| Crates added | `playdrop-ffi` (cdylib + rlib, UniFFI 0.32.2), `uniffi-bindgen` (binary) |
| App added | `apps/android` (Gradle 9.6, AGP 9.4.1 built-in Kotlin, Compose, minSdk 26, targetSdk 36, compileSdk 37) |
| Rust tests | 306 passed, 0 failed, 5 ignored (`cargo test --workspace`, ffi included): 194 core unit, 58 daemon, 23 ipc unit, 13 window, 6 `phase3_core`, 5 `host_smoke`, 4 `phase1_gates`, 1 ffi, 1 ipc D-Bus, 1 CLI. Ignored: two multicast tests, the LAN pairing gate, the live clipboard test, the live daemon send test |
| Android tests | 59 JUnit tests in `apps/android/app/src/test` (`./gradlew :app:testDebugUnitTest`) |
| Lint | `cargo clippy --workspace --all-targets` zero warnings on Linux and on `x86_64-pc-windows-gnu` (type check only, see section 6); `cargo fmt --all --check` clean |
| Toolchain | Rust 1.99; Android SDK platform 37.0, NDK 27, Java 17, `cargo-ndk` 4.1.2; Rust targets `aarch64-linux-android`, `x86_64-linux-android`, `x86_64-pc-windows-gnu` |

**Phase 3 "done when" (PLAN.md row 3), measured on 2026-10-09 on a Samsung Galaxy A54
(Android 16, API 36) against the Phase 1 CLI host "bravo" and the Phase 2 daemon "alpha"
on this Pop!_OS machine over the home Wi-Fi (both ends on 192.168.86.0/24).**

### 1.1 Gate results

| Check | Result |
|---|---|
| Share sheet to default device is one tap | Pass. `ACTION_SEND` of a 400 MiB `content://` item to `ShareActivity`: first use shows the device picker (choice becomes the default), later sends go straight to the card; the file reached alpha in 18 s with a matching SHA-256 (debug build on the phone) |
| Notification action sends the clipboard with the app closed | Pass. The user copied fresh text on the phone and tapped "Send clipboard" in the persistent notification; the text landed in the desktop clipboard (`wl-paste`). An earlier run with the text a peer had just pushed was correctly skipped by the echo rule |
| A 400 MB video arrives with the phone locked | Pass. 400 MiB from bravo with the screen off: debug build 42 s after accept (about 10 MiB/s); release build through the alpha daemon, auto-accepted, 13 s (32 MiB/s average, 57 MiB/s peak), SHA-256 equal on both ends |
| After 30 minutes of Doze the phone is reachable by last-known IP or shown as asleep | Pass for a forced 10 minute deep idle (`dumpsys deviceidle force-idle`): the phone sent presence `asleep`, the session stayed up, a clipboard push from the desktop was applied on the phone while idle. A natural 30 minute Doze was not waited for |
| Destroying the Activity does not stop the server | Pass. The task was swiped away in recents: no `MainActivity` record left, same process PID, foreground service still up, session online, a ping from bravo arrived as a notification |
| Reconnects after a Wi-Fi drop without re-pairing | Pass. `svc wifi disable` for 20 s then enable: session back 4 s after Wi-Fi returned, no pairing prompt, battery and presence re-sent |
| Idle battery cost measured and acceptable | Not measured (needs an unattended multi-hour run; section 8) |

Also verified live: SAS pairing from the phone to alpha (manual address and the Nearby list
both worked; the SAS, name and Short ID render as SR-8 asks, Reject and Pair unfocused),
"New device paired: alpha (znrt-kd3k-sgxn-nw5d)" notification, "Received pd-video.bin, saved
in Download/PlayDrop" notification, the Transfers tab offer card with Accept and Decline,
alpha's laptop battery shown on the phone's device row, sealed `identity.key` and `pins.json`
surviving a reinstall without re-pairing, and SR-29 refusals from the shell UID.

**Explicitly not done in Phase 3**

- Remote input (`input.*`): Phase 5. The core keeps a TODO in `host/session.rs`.
- Sender-side resume across a sender restart (protocol.md 7.6): deliberately out of v1,
  see `docs/core-api.md` section 8.
- Windows: type-checked on the gnu target through a stub C compiler, never linked or run.
- Desktop MPRIS (media state from the daemon): Phase 5; the frames and the host methods exist.
- iOS: nothing. The FFI surface and the outbox holder are ready for it.

---

## 2. Repository map (new in Phase 3)

```
docs/core-api.md                        the frozen core surface, v1; every binding follows it
crates/playdrop-core/src/
  transfer/source.rs                    SendSource, StreamSource, SourceOpener, SourceRef (section 3 of core-api)
  transfer/sink.rs                      ReceiveSink, DeliverItem, SinkError, DeliveredFile (section 2)
  identity/sealing.rs                   SecretStore, PDSEALED1 file format (section 2a, SR-28)
  host/blobs.rs                         in-memory blob store: 5 min TTL, 64 MiB cap, bound to a Device ID (5.15)
  host/status.rs                        battery, presence and media state push, rate limits (5.8, 5.9, 5.10)
  host/outbox.rs                        holder and puller sides of the outbox (7.8)
  tests/phase3_core.rs                  blobs, staged receive, stream sources, outbox, battery on session start
crates/playdrop-ffi/src/
  lib.rs                                setup_scaffolding, core_version
  host.rs                               PlayDropHost object: tokio runtime, event pump, every Host method
  types.rs, events.rs                   FFI records and enums mirroring the core, one From impl each
  traits.rs                             foreign traits: HostListener, ReceiveSink, SourceOpener, SecretStore, NetworkInterfaces
  error.rs                              PlayDropError (Kotlin PlayDropException); field is `detail`, never `message`
  logging.rs                            init_logging: logcat layer on Android, stderr elsewhere
  uniffi.toml                           Kotlin package org.playdrop.core, Swift module PlayDropCore
crates/uniffi-bindgen/                  `cargo run -p uniffi-bindgen -- generate --library ...`
crates/playdropd/src/battery.rs         sysfs and GetSystemPowerStatus battery reader
apps/android/
  settings.gradle.kts, build.gradle.kts, gradle/libs.versions.toml, gradle.properties
  app/build.gradle.kts                  BuildRustTask (cargo ndk per ABI) and GenerateBindingsTask wired through the variant API
  app/src/main/AndroidManifest.xml      connectedDevice FGS, exported=false everywhere but ShareActivity and the launcher
  app/src/main/kotlin/org/playdrop/android/
    PlayDropApp.kt                      channels, logging, foreground discovery window
    MainActivity.kt                     Compose root, clipboard auto-push while resumed, starts the service on open
    core/CoreHost.kt                    the facade every screen uses (flows plus withHost); CoreHostImpl.kt behind it
    core/MediaStoreSink.kt              ReceiveSink into MediaStore.Downloads/PlayDrop (API 29+), FileProvider on 26 to 28
    core/UriSource.kt                   content:// item to SendSource.Stream, SourceOpener via openFileDescriptor
    core/KeystoreSecretStore.kt         AES-256-GCM Keystore key, TEE fallback from StrongBox, SealFormat.kt
    core/AndroidInterfaces.kt           NetworkInterfaces from java.net.NetworkInterface
    data/Settings.kt, SettingsImpl.kt   DataStore preferences
    service/CoreService.kt              LifecycleService, startForeground(connectedDevice), network callback, signals
    service/MulticastPolicy.kt          MulticastLock only in discovery windows (anti-pattern 10)
    service/DeviceSignals.kt            battery and presence broadcasts to the core
    service/Notifications.kt, NotificationPlan.kt, FindRinger.kt, ClipboardApplier.kt, ActionReceiver.kt, BootReceiver.kt
    share/ShareActivity.kt              ACTION_SEND and SEND_MULTIPLE, 2 s card with Cancel (SR-29), device picker on first use
    share/ClipboardSendActivity.kt      notification action "Send clipboard"
    share/PickAndSendActivity.kt        notification action "Send to <default>" via the photo picker
    ui/                                 PlayDropRoot (navigation), devices, device, adddevice, transfers, settings, diagnostics, onboarding, pairing
    qr/                                 zxing encode, CameraX + zxing scan, FLAG_SECURE while the QR shows (SR-9)
  app/src/test/                         59 JUnit tests
```

---

## 3. Public API

`docs/core-api.md` is the frozen surface. Section 4 of this file is the only change log:
everything marked **new** or **changed** there is implemented; the deviations recorded during
implementation are:

- `ClipboardPush.skipped_unsupported: usize` (additive, serde default): peers without
  `blobs` or `clipboard.image`.
- `TransferSnapshot.delivered` mirrors the event field.
- The new traits live in `transfer::{source,sink}` and `identity::sealing` and are
  re-exported from `playdrop_core::host`.
- `add_stream` refuses a stream whose declared size differs from the opened file; the resume
  check records the opened file's own mtime (0 when the declared mtime is 0).
- `pull_outbox` returns items already being pulled as well as new ones; a declined or
  cancelled pull still sends `DELETE` to the holder.
- `send_media_command` returns `Forbidden` when the peer lacks `media.control`.
- `protocol.md` 11.1 gained six bullets (removed-by-peer stored, blob text uses LF so the
  blob id equals the clipboard hash, blob 410 window, outbox `source_changed` gives 410, the
  puller deletes a declined item, media state needs the sender's `media` grant).

### 3.1 FFI (Kotlin names)

`PlayDropHost(config: HostConfig, listener: HostListener)` starts the core (blocking, call
off the main thread); methods mirror `Host` with hex Device IDs and `ip:port` strings;
`suspend fun` for everything the core awaits (`connect`, `setGrants`, `setLabel`, `unpair`,
`pairWith`, `scanQr`, `send`, `cancelTransfer`, `respondOffer`, `queueOutbox`,
`removeOutbox`, `pullOutbox`, `shutdown`). Foreign interfaces: `HostListener.onEvent`,
`ReceiveSink.deliver(item, fd)` and `makeDir`, `SourceOpener.open(): Int` (a detached fd
the core owns), `SecretStore.seal/open`, `NetworkInterfaces.ipv4Interfaces`. Errors:
`PlayDropException.{NotPaired, Forbidden, Locked, NotFound, TooLarge, PortHeld,
InsecurePermissions, NoSpace, PathInvalid, PairRejected, Io, Other}` with a `detail` field.
Top-level: `coreVersion()`, `initLogging(level)`.

Every synchronous method enters the tokio runtime first: the core spawns tasks from methods
such as `pairingRespond` and `setBattery`, and a JNA caller thread has no runtime context.
This was found on the phone (crash on answering a SAS prompt) and is fixed in `host.rs`.

---

## 4. How it runs today

Desktop and CLI: unchanged from Phase 2 (`CLAUDE.md` Commands), plus the CLI REPL commands
`outbox <peer> <path>...`, `outbox`, `outbox rm <id>`, `pull <peer>`, `find <peer> [stop]`.

Android:

```
cd apps/android
./gradlew :app:compileDebugKotlin      # Kotlin check: host Rust build plus bindgen, about a minute
./gradlew :app:assembleDebug           # APK with Rust for arm64-v8a and x86_64 (several minutes cold)
./gradlew :app:testDebugUnitTest
adb install -r app/build/outputs/apk/debug/app-debug.apk
adb logcat -s playdrop:* AndroidRuntime:*
```

`playdrop.rustAbis` in `gradle.properties` picks the ABIs. The generated bindings land in
`app/build/generated/uniffi/kotlin`; never commit them. Release builds (`assembleRelease`)
compile the Rust core with `--release`; use them for any speed or battery number.

On the phone: first run shows onboarding (notification permission, start service). Devices
tab: "Add device" opens the 120 s window with the candidate list, manual `ip:port`, "Show QR"
and "Scan QR". The persistent notification carries "Send clipboard" and "Send to <default>".
Received files land in `Download/PlayDrop/` through MediaStore.

---

## 5. Decisions made in Phase 3

| Decision | Why |
|---|---|
| Core API frozen in `docs/core-api.md` before the first binding | PLAN.md anti-pattern 20; iroh's binding churn |
| Mobile receive = private staging dir plus a `ReceiveSink` that moves each verified file | Android scoped storage has no raw path into Downloads; the core keeps its `.part` and ledger machinery unchanged and the host owns the final copy (MediaStore `IS_PENDING`) |
| Mobile send = `SendSource::Stream` with a reopenable `SourceOpener` returning a detached fd | Share-sheet items are `content://` URIs; the resume check uses `fstat` size, mtime when known and `(dev, ino)` |
| `SecretStore` seals `identity.key` and `pins.json` as `PDSEALED1 || sealed`, migration from plain on first open | SR-28 on Android (Keystore AES-256-GCM, TEE fallback from StrongBox) without changing the desktop's 0600 files |
| No sender persistence across a sender restart | Share intent URI grants die with the process; a restarted sender re-offers |
| Blobs in memory, 5 min TTL, 64 MiB cap, bound to the announced Device ID | protocol.md 5.15; nothing large lives on disk |
| Static v1 capabilities; no `set_capabilities` | Grants are the per-peer toggle; the `capabilities` frame stays parsed but unsent |
| UniFFI proc-macros in library mode, foreign traits all synchronous | Avoids UniFFI issue #2576 (async foreign traits on tokio) |
| FFI methods: async ones run on the host's own tokio runtime via `JoinHandle`; sync ones `Runtime::enter` | One runtime for every core task; no reliance on UniFFI's async runtime |
| Error field named `detail` | A UniFFI error field called `message` clashes with Kotlin `Throwable.message` and does not compile |
| AGP 9.4.1 with built-in Kotlin, Gradle 9.6, compileSdk 37 | Current AndroidX libraries refuse AGP 8.13; platform package name is `android-37.0` |
| Rust built from Gradle through typed tasks on the variant API (`addGeneratedSourceDirectory`) | Configuration-cache safe; Gradle sees the `.so` files and the generated Kotlin as outputs |
| MulticastLock only while the "Add device" window is open, 60 s after start or a network change, and while the app is in the foreground | Anti-pattern 10; last-known dial covers the rest |
| "Send clipboard" is a transparent activity, not a receiver | Android 10+ gives clipboard reads only to the focused app |
| `BootReceiver` exported (system broadcast with an action check); every other component `exported=false` | SR-29 |
| Daemon strips image bytes from `ClipboardImage` before forwarding to UI clients | The IPC event cap is 4 MiB and no UI renders the bytes; the daemon already applied the image |
| Desktop battery `charging` means "on external power" | Matches how phones show it |
| Outbox items with stream sources are memory-only; path-only items persist under `<data_dir>/outbox/` | A URI grant cannot be reopened after a restart |

---

## 6. Open TODOs

| File | What | Phase |
|---|---|---|
| `playdrop-core/src/host/session.rs` | Remote input frames (`TODO(Phase 5, protocol.md 5.12, SR-25)`) | 5 |
| `playdrop-core/src/host/send.rs` | Sender persistence, out of v1 by decision | later |
| `playdrop-core/src/host/platform.rs`, `net/server.rs`, `net/mod.rs`, `transfer/receiver.rs` | Windows free space, `SO_EXCLUSIVEADDRUSE`, socket buffers, `fallocate` | 2 follow-up / 6 |
| `playdropd/src/notify.rs`, `tray.rs` | Windows toast buttons (COM activator), Windows tray device lines | 2 follow-up |
| `playdrop-ffi/src/host.rs` | JNI `AttachCurrentThread` on tokio worker start if event callbacks ever show up in CPU profiles | 6 |
| `apps/android` | Doze and battery-cost measurement script; Windows and iOS builds of the outbox puller | 4, 6 |

Windows type check recipe (no MinGW installed): `ring`'s build script needs a C compiler, so
set `CC_x86_64_pc_windows_gnu` to a script that writes empty object files and `AR=ar`, then
`cargo check --workspace --exclude playdrop-ffi --exclude uniffi-bindgen --target x86_64-pc-windows-gnu --all-targets`.

---

## 7. Known gaps and risks

- Not tapped through on the phone: the photo picker from "Send to <default>" and the
  battery-optimisation request. The user confirmed the "Show QR" screen screenshots as black
  on the phone (`FLAG_SECURE`, SR-9) and scanned a QR shown by a fresh CLI host "charlie":
  charlie asked "ADD SM-A546U1 (Short ID)?", the phone was pinned after one `y` and came
  online with battery and presence (protocol.md 6.3 scanner side).
- Nearby discovery on this mesh Wi-Fi: the phone listed alpha only after alpha reopened its
  window while the phone's window was already open. The phone holds the MulticastLock only
  inside its window, and the desktop sends its beacon burst at 0, 1 and 3 s after opening then
  every 60 s, so a phone that opens its window after the burst waits up to a minute. Fix
  candidates for Phase 5: a fresh burst on every `AddDeviceSeen`-less 10 s, or the phone
  asking for one. Manual address and QR cover it today.
- Speed on the phone: 32 MiB/s average on the release build over 5 GHz Wi-Fi to a locked
  phone (57 MiB/s peak); the debug build manages about 10 MiB/s. No iperf3 baseline on this
  link yet (PLAN.md "50% of iperf3" floor still unmeasured on Wi-Fi).
- Both IPC transports drop a single event over 4 MiB; the daemon now trims images, but a
  future large event needs the same treatment.
- Windows images use the registered `PNG` clipboard format only; no bitmap conversion.
- A Windows find ring ends only on stop or after 60 s (toast dismissal is not observed).
- Android 8 and 9 (API 26 to 28) have no MediaStore Downloads: received files go to the
  app's own folder behind a FileProvider and do not show in the system Downloads app.
- Leaving Doze reports presence `Locked` (screen still off), not `Active`.
- A shared item that cannot be reopened falls back to its held descriptor, whose read
  position is shared with the original; resume on such an item restarts from 0.
- `pull_outbox` resume is unit-tested only; the integration test does a full pull.
- The idle battery cost of the foreground service is unmeasured.

---

## 8. Phase 4 kickoff brief

PLAN.md "Build phases", row 4:

> | 4. iOS | SwiftUI app; share extension that hands large items to the main app through the App Group; foreground receive; outbox pull from desktops and Android phones; Local Network fix-it screen; "Send clipboard" Shortcut action | A camera-roll dump to Linux works; a 2 GB video goes through the share extension handoff; receives while open; pulls queued files on open; denied Local Network permission shows the fix screen; App Store copy is accurate |

**Step 0 (serial):** generate Swift bindings (`cargo run -p uniffi-bindgen -- generate
--library target/release/libplaydrop_ffi.dylib --language swift`, module `PlayDropCore`
from `uniffi.toml`), build an XCFramework for `aarch64-apple-ios` and
`aarch64-apple-ios-sim`, and pre-add every Swift package. No core API changes: anything iOS
needs that `docs/core-api.md` lacks is a deliberate freeze break and goes into the handoff.
Needs a Mac with Xcode; this Linux machine cannot build it.

| Task | Builds | Phase 3 pieces reused |
|---|---|---|
| a. iOS host | App process hosts `PlayDropHost` with `ReceiveTarget.directory` under the app's Documents (iOS has a real folder), `SecretStore` on the Keychain (`kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly`), `NetworkInterfaces` from `getifaddrs`, `KeepAliveProfile.mobile`, presence `asleep` on background | `apps/android/.../core/CoreHostImpl.kt` is the shape; `MediaStoreSink` shows the sink contract even though iOS can use a directory |
| b. Outbox pull on open | `pullOutbox(peer)` for every paired peer at foreground; the `OutboxReady` event; desktop and Android already hold outboxes (`QueueOutbox` IPC, `queueOutbox` on Android) | `host/outbox.rs` puller, `phase3_core.rs` outbox test |
| c. Share extension | App Group queue plus URL scheme (SR-30: no parameters); the main app converts queued items to `SendSource.stream` openers over the App Group files | `UriSource.kt` for the opener shape |
| d. Local Network permission | Bonjour `_playdrop._tcp` in `NSBonjourServices`; denied permission shows the fix screen from the `DiscoveryDiagnostic` events and `diagnose()` | Android `DiagnosticsSummary.kt` wording |
| e. Phone to phone | Android holds an outbox for the iPhone; test Android queue, iPhone pull | CLI `outbox`/`pull` as the reference flow |

Android numbers to beat on the iPhone: none measured yet in release mode; measure both in
Phase 4 with the same 400 MB file.

---

## 9. Rules carried forward

- Hyphen only. Never the long dash, in code, comments, docs or commits.
- No AI attribution anywhere: no co-author trailers, no "generated with" lines, no session links.
- Agents do not commit or push. The user commits and runs `git push origin main`.
- Parallel agents never edit any `Cargo.toml` or Gradle file. Dependencies are added up front in a serial step.
- Never bind or announce on 1714-1764, 53317 or 224.0.0.167.
- Manual runs use data and receive dirs on ext4 (`/tmp/pd-*`), never under the repo, and `HOME=/tmp/pd-home` or `--no-autostart` so the real user config is untouched.
- `docs/core-api.md` is frozen: a change needs a note in the phase handoff and an ffi version bump.
- Every phase ends by writing `docs/phases/phase-N-<name>.md` in this same shape.

# Phase 4 handoff: iOS app, share extension, outbox pull

Reader: a fresh session that must build Phase 5 (companion depth: remote input, presenter
mode, media polish) or finish the iOS gates on a Mac, without re-reading the iOS code. Read
`CLAUDE.md`, `PLAN.md` ("Build phases", "Anti-patterns"), `docs/protocol.md`,
`docs/threat-model.md`, `docs/core-api.md`, then `docs/phases/phase-3-android.md` sections 1,
5 and 7, and this file. `apps/ios/README.md` is the build and test manual for the Mac.

---

## 1. Status

| Item | Value |
|---|---|
| Base | `main` at `354408b` (Phase 3); this phase is one commit on top |
| Built on | This Pop!_OS machine, which has no Xcode. Everything below that needs a Mac is written and checked as far as Linux allows, and listed in section 7 as unverified |
| Added | `apps/ios/` (SwiftUI app `PlayDrop`, share extension `PlayDropShare`, Swift packages `PlayDropShared` and `PlayDropCore`, XcodeGen `project.yml`, `scripts/build-core.sh`, `scripts/check-linux.sh`, App Store copy) |
| Core changes | `crates/playdrop-ffi/Cargo.toml`: `crate-type` gains `staticlib` (iOS links a static library into an XCFramework). No core API change: `docs/core-api.md` stays frozen as at Phase 3 |
| Rust | 306 passed, 0 failed, 5 ignored (`cargo test --workspace --no-fail-fast`, unchanged from Phase 3; two load-sensitive tests, the daemon's `read_limited_times_out_on_a_stalled_source` and gate B, flaked once each while the Swift checks ran in parallel and pass alone); `cargo clippy --workspace --all-targets` zero warnings; `cargo fmt --all --check` clean |
| Rust on the iOS targets | `cargo clippy -p playdrop-ffi --target aarch64-apple-ios` and `--target aarch64-apple-ios-sim`: zero warnings (type check through stub `cc` and `ar`, see `scripts/check-linux.sh`; never linked here) |
| Swift on Linux | `apps/ios/scripts/check-linux.sh` exits 0: 145 Swift files parse (`swiftc -parse`); `PlayDropShared` builds in Swift 6 mode and its 59 XCTests pass on Linux; `scripts/typecheck-linux.sh` compiles the 50 non-SwiftUI app files, 5 of the 7 extension files and all 14 test files with `-strict-concurrency=complete` against the real bindings and `PlayDropShared` plus stub Apple modules (0 errors, 0 warnings), and `ui-symbols.py` resolves every cross-file symbol and label the 31 SwiftUI files use; 11 plists, catalogs and `project.yml` validate |
| CI | `.github/workflows/ios.yml`: free GitHub macOS runner builds the core for the simulator, generates the project, compiles app and extension unsigned and runs `PlayDropTests` on a simulator; plus a Linux job for the `PlayDropShared` tests. First run happens on the first push of this phase; read its log before anything else |
| Toolchain | Swift 6.4 (Linux toolchain for the checks; Xcode 16 or newer on the Mac), XcodeGen 2.4x, iOS 17.0 minimum, UniFFI 0.32.2 Swift bindings (module `PlayDropCore`), Rust targets `aarch64-apple-ios` and `aarch64-apple-ios-sim` |

### 1.1 Gate results (PLAN.md row 4)

Every gate needs a Mac with Xcode and an iPhone. None has been run. `apps/ios/README.md`
holds the checklist with the Linux-side commands for each one.

| Check | Result |
|---|---|
| A camera-roll dump to Linux works | Not run |
| A 2 GB video goes through the share extension handoff | Not run |
| Receives while open | Not run |
| Pulls queued files on open | Not run |
| Denied Local Network permission shows the fix screen | Not run |
| App Store copy is accurate | Copy written in `apps/ios/AppStore/copy.md`; review against the shipped behaviour once the gates pass |

---

## 2. Repository map (new in Phase 4)

```
apps/ios/
  README.md                             Mac build manual, entitlement request, gate checklist, Linux checks, known risks
  project.yml                           XcodeGen 2.46 spec: PlayDrop (app), PlayDropShare (extension), PlayDropTests
  Local.xcconfig.example                DEVELOPMENT_TEAM and CODE_SIGN_STYLE; copy to Local.xcconfig (gitignored)
  AppStore/copy.md                      store listing, keywords, privacy answers; honest about foreground-only receive
  scripts/build-core.sh                 Rust staticlibs for both iOS targets, Swift bindings, header repair, XCFramework
  scripts/check-linux.sh                parse, Shared tests, iOS clippy, resource validation, type check
  scripts/typecheck-linux.sh            swiftc -emit-module over the non-SwiftUI files with stub Apple modules
  scripts/linux-typecheck/              10 stub framework modules, 3 shims, ui-symbols.py
  scripts/stubs/                        stub-cc and stub-ar for cargo on the apple targets without an SDK
  Packages/PlayDropCore/                SwiftPM: binary PlayDropCoreFFI.xcframework + generated PlayDropCore.swift (both gitignored)
  Packages/PlayDropShared/              SwiftPM, Foundation only, Swift 6 mode, 20 sources, 8 test files (59 tests)
    Identifiers.swift                   bundle ids, App Group, scheme, Bonjour type, port, folder names
    OutgoingQueue.swift (+Writer, Batch, Item, Manifest, Listing, Error, BatchId, StreamingCopy)
                                        App Group queue: <root>/<id>/files/<name> + manifest.json, atomic rename commit, sweep
    SharedSettings.swift                SharedSettingsKeys, SharedPeerEntry, PeerDirectoryCodec
    Diagnostics.swift, DiagnosticsSummary.swift, LocalNetworkInference.swift
                                        DiagSnapshot mirror, Android's finding list, Local Network permission inference
    Addresses.swift, ByteFormat.swift, FileNames.swift, ClipboardEcho.swift, PairingUri.swift, PeerLabel.swift
  PlayDrop/
    App/PlayDropApp.swift               @main; scene phases to AppServices; onOpenURL to URLSchemeHandler only
    Core/CoreHost.swift                 the facade every screen uses (observable state plus withHost)
    Core/CoreHostImpl.swift             the one PlayDropHost, event pump, state refresh (4 Hz progress), window polling
    Core/AppServices.swift              start(), sceneDidBecomeActive(), sceneDidEnterBackground(), onboardingDidFinish()
    Core/HostConfigFactory.swift        data dir under Application Support (no backup), Documents/PlayDrop receive dir
    Core/KeychainSecretStore.swift      CryptoKit AES-256-GCM, key in the Keychain, AfterFirstUnlockThisDeviceOnly
    Core/SystemInterfaces.swift         getifaddrs, skips utun, pdp_ip, ipsec, llw, awdl
    Core/FileOpener.swift               SourceOpener over a URL, security scope held until release
    Core/AppSettings.swift, DeviceSignals.swift, Logging.swift, ErrorText.swift, EventBroadcaster.swift,
    Core/HostEventForwarder.swift, SerialLifecycle.swift, SendPreparation.swift, BackgroundAssertion.swift
    Outbox/OutboxPuller.swift           pullOutbox per online peer on foreground, peerOnline and outboxReady
    Clipboard/ClipboardSender.swift, ClipboardApplier.swift, ClipboardEchoState.swift
    Notifications/LocalNotifications.swift, FindRinger.swift
    Share/OutgoingDrain.swift           sends queued batches to the preferred or default peer, deletes on completion
    Share/PeerDirectoryMirror.swift, PhotoExport.swift, URLSchemeHandler.swift, UserFacingError.swift
    Intents/SendClipboardIntent.swift, DeviceEntity.swift, ClipboardIntentOutcome.swift, PlayDropShortcuts.swift
    UI/RootView.swift, RootSheet.swift  tabs Devices, Transfers, Settings; one root sheet (pairing, offers, batches)
    UI/Devices, AddDevice (QR show with SecureContainer, QR scan), Pairing, Transfers, Send, Settings
    UI/Settings/LocalNetworkFixItView.swift, DiagnosticsView.swift, DiagnosticsMapping.swift
    UI/Onboarding/OnboardingView.swift  three pages, notification permission, then onboardingDidFinish
    UI/Common/                          L10n, UiMessage and MessageCenter, CoreCall, UIFormat, FindRingerAlert, pickers
    Resources/Info.plist                NSLocalNetworkUsageDescription, NSBonjourServices, playdrop scheme, Files sharing, no UIBackgroundModes
    Resources/PlayDrop.entitlements     App Group, multicast networking, keychain access group
    Resources/PrivacyInfo.xcprivacy, Assets.xcassets (no icon image yet), Localizable.xcstrings
  PlayDropShare/
    ShareViewController.swift, ShareView.swift, ShareModel.swift, ShareItemLoader.swift, BatchSink.swift,
    AppOpener.swift, SharedSettingsReader.swift, Info.plist, PlayDropShare.entitlements, PrivacyInfo.xcprivacy
  PlayDropTests/                        14 XCTest files (Mac only): secret store, opener, interfaces, drain rules, UI logic
```

---

## 3. Public API

No change to `docs/core-api.md` or to `playdrop-ffi`'s surface. iOS binds the same
`PlayDropHost` as Android, with these host-side choices:

| Core trait or option | iOS implementation |
|---|---|
| `ReceiveTarget` | `directory` at `<Documents>/PlayDrop`; `UIFileSharingEnabled` and `LSSupportsOpeningDocumentsInPlace` make it visible in Files (iOS has a real folder, so no `ReceiveSink`) |
| `SecretStore` | `KeychainSecretStore`: AES-256-GCM through CryptoKit, key in the Keychain with `kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly` (SR-28); the core adds the `PDSEALED1` header |
| `NetworkInterfaces` | `SystemInterfaces` over `getifaddrs`, skipping cellular, VPN and AWDL interfaces |
| `SourceOpener` | `FileOpener`: `open(2)` on a URL, holding the security scope for document-picker items |
| `SendSource` | `path` for App Group and exported photo files (app-private), `stream` for security-scoped URLs |
| `KeepAliveProfile` | `mobile` |
| Presence | `active` on foreground, `asleep` right before the core stops on background |
| Discovery | the core's own mDNS and beacon, which on iOS need the `com.apple.developer.networking.multicast` entitlement (section 5) |

---

## 4. How it runs today

Desktop, CLI and Android: unchanged from Phase 3.

iOS (Mac only): see `apps/ios/README.md`. In short: `scripts/build-core.sh` (Rust for both
iOS targets, Swift bindings, XCFramework), `xcodegen generate`, set the team in
`Local.xcconfig`, run on a device. The simulator cannot exercise Local Network permission or
multicast reliably.

Linux: `apps/ios/scripts/check-linux.sh` parses every Swift file, builds and tests
`PlayDropShared`, and runs clippy for the two iOS Rust targets.

---

## 5. Decisions made in Phase 4

| Decision | Why |
|---|---|
| Phase built on Linux without a Mac; every Apple-only piece is written, checked as far as Linux allows and documented as unverified | The alternative was to stall the phase. `PlayDropShared` (Foundation only) is fully tested on Linux; the Rust core is clippy-clean for both iOS targets; the non-SwiftUI Swift is type-checked with strict concurrency against stub Apple modules; SwiftUI code is syntax- and symbol-checked only |
| The core starts only after onboarding is done | The iOS Local Network prompt fires on the first LAN socket; it must come after the page that explains it |
| `find` rings the iPhone only while the app is in front (system sound plus haptics, 60 s cap, Stop button) | Nothing can ring a suspended app; anti-pattern 18 wants the limit stated, and the onboarding copy says so |
| UniFFI's Swift header is repaired in `build-core.sh` rather than by renaming the `open` trait methods | Renaming would change the frozen core API and the Kotlin bindings for a bindgen bug |
| The app process hosts the core only while in the foreground; background means presence `asleep` and `stop()` | iOS suspends the process and marks its sockets defunct (PLAN.md "Platform ceilings"); anti-pattern 11 forbids claiming always-on receive |
| Desktops and Android hold the iPhone's outbox; the iPhone pulls on every foreground and on `outboxReady` | protocol.md 7.8; the puller keeps the resume state, so a killed app resumes the pull |
| Share extension never hosts the core; it writes an App Group batch (`manifest.json` plus files, committed by atomic rename) and asks the user to open the app | PLAN.md row 4 ("hands large items to the main app through the App Group"); two processes cannot share port 42187, the extension has about 120 MB of memory and no pairing UI |
| `playdrop://` opens the app and drains the queue; host, path and query are ignored | SR-30 |
| The core's discovery stays on and the app carries the multicast networking entitlement, requested from Apple | The alternative, a Swift Bonjour layer feeding the core, would need a new FFI method (a freeze break) and still could not send the beacon. Without the entitlement discovery fails visibly in `diagnose()`, and pairing by address or QR plus last-known dial still work |
| XcodeGen `project.yml` instead of a committed `.xcodeproj` | A text spec is reviewable and mergeable; agents can edit it without Xcode |
| Swift packages: `PlayDropShared` (Swift 6 mode, Foundation only, Linux-tested) and `PlayDropCore` (binary XCFramework plus generated bindings, both gitignored) | Keeps the testable logic out of the app target and the generated code out of git |
| App and extension targets compile in Swift 5 language mode with strict concurrency warnings | Nothing here can compile them; Swift 6 mode errors would be found only on the Mac. Flip to Swift 6 mode once the app builds there |
| `playdrop-ffi` gains `crate-type = "staticlib"` | iOS links a static library; `cargo ndk` still picks only the `.so` for Android (the `.a` is an extra build artefact there) |
| Received files land in `Documents/PlayDrop`, shown in the Files app | iOS has no shared Downloads; Files is where users look |
| QR screen hidden from capture through a secure text field layer | SR-9 asks for capture blocking; iOS has no `FLAG_SECURE`, and this is the known working route |
| Local Network permission is inferred (join failures, "No route to host" dials, a peer online this run) | iOS has no API to read the permission; anti-pattern 18 wants the failure named |

---

## 6. Open TODOs

| File | What | Phase |
|---|---|---|
| `apps/ios/scripts/build-core.sh` step 5b | Drop the header repair once UniFFI stops writing backticked C identifiers | on upgrade |
| `apps/ios/PlayDrop/Clipboard/ClipboardSender.swift` | A per-peer clipboard image send needs `send_clipboard_image(peer, ...)` in the core; today an image goes to every eligible peer | 5 (core API note) |
| `apps/ios/project.yml` | Flip the app and extension targets to Swift 6 language mode once they compile on a Mac | 4 follow-up |
| `apps/ios/PlayDrop/Resources/Assets.xcassets` | App icon image | 6 |
| `apps/ios/PlayDrop/Core/Logging.swift`, `crates/playdrop-ffi/src/logging.rs` | Core logs go to stderr on iOS; an `os_log` layer would show them in Console | 6 |
| `apps/ios/PlayDrop/Share/OutgoingDrain.swift` | Mid-send state is not saved: a batch interrupted by iOS killing the app is sent again whole | later |
| Phase 3 carry-overs | `input.*` frames (Phase 5), sender persistence (out of v1), Windows link and run, Nearby re-burst fix, idle battery measurement | 5, 6 |

---

## 7. Known gaps and risks

- **Nothing in `apps/ios` has been compiled by Xcode, linked, signed or run.** The checks here
  cover syntax everywhere, types and strict concurrency for everything outside SwiftUI, and
  symbol and label resolution inside SwiftUI. SwiftUI, PhotosUI, AVFoundation, QuickLook and
  the real behaviour of the Apple frameworks behind the stubs are unverified. The README lists
  the specific calls to confirm first (`PhotosPicker(preferredItemEncoding:)`,
  `FileRepresentation(importedContentType:importing:)`, `.quickLookPreview`, the `@Bindable`
  local in `SettingsView`, delegate isolation annotations).
- **Multicast entitlement:** Apple must grant `com.apple.developer.networking.multicast`
  before a signed build runs with it. Without it the core's mDNS and beacon fail, `diagnose()`
  shows the join failures, the fix-it screen names them, and pairing by address or QR plus
  last-known dial still work.
- **Local Network permission is inferred**, not read: the heuristics in
  `LocalNetworkInference` may say "likely denied" for a few seconds after start while mDNS is
  still registering.
- **SecureContainer** (QR hidden from capture) relies on the secure text field's private
  canvas layer; a future iOS could show the QR normally instead of failing (SR-9 then needs a
  new route).
- **AppOpener** uses the responder-chain `openURL:` workaround from the share extension; if
  iOS blocks it the user taps Done and opens the app by hand (the queue drains then).
- **`UIDevice.current.name`** is a generic "iPhone" without the user-assigned-device-name
  entitlement; onboarding should ask for a name (today the Settings screen allows editing).
- **Export compliance:** `ITSAppUsesNonExemptEncryption = false` is our reading for an app
  that uses TLS only; confirm before the first submission.
- **App Store Connect** needs a privacy policy URL and an icon; neither exists yet.
- **Core on iOS** was type-checked and clippy-checked for both targets but never linked:
  `ring`, `rustls` and `mdns-sd` all claim iOS support; link errors would show on the first
  `build-core.sh` run.
- **Test target** depends on the packages with `link: false`; if `import PlayDropShared`
  fails inside the tests, add the package as a linked dependency.
- **Android build time** grows slightly: `cargo ndk` now also builds the unused `.a` per ABI.

---

## 8. Phase 5 kickoff brief

PLAN.md "Build phases", row 5:

> | 5. Companion depth | Remote input: `SendInput` on Windows, RemoteDesktop portal plus libei on COSMIC and GNOME; presenter mode; GNOME clipboard button; media control polish | Trackpad latency under 30 ms on LAN on Windows; COSMIC remote input works or the feature hides itself with a reason; a Wi-Fi bounce reconnects without re-pairing; each feature detects missing OS support and hides itself |

Before Phase 5 starts, the iOS gates in section 1.1 must run on a Mac; treat that as the
first task of the next session that has one, and fold the results into this file.

**Step 0 (serial):** the `input.*` frames (protocol.md 5.12, SR-25) are the one core change:
add `Host::send_input` and the `Input` event, bump `playdrop-ffi`, note the freeze break in
the Phase 5 handoff, and regenerate the Kotlin and Swift bindings.

| Task | Builds | Phase 4 pieces reused |
|---|---|---|
| a. Windows input | `SendInput` injection behind the `input` grant, latency measured | `playdropd/src/battery.rs` shows the Windows-specific module shape |
| b. COSMIC and GNOME input | RemoteDesktop portal plus libei; hide with a reason when the portal is missing | `playdropd/src/clipboard` for the portal fallback pattern |
| c. Phone trackpad and presenter mode | Android and iOS screens sending `input.*`; iOS only while foreground | `apps/ios/PlayDrop/UI` and `apps/android/.../ui/device` |
| d. Media polish | Desktop MPRIS state from the daemon; phone media controls | `host/status.rs` already coalesces media frames |
| e. Nearby discovery fix | Fresh beacon burst when a window opens with no candidate after 10 s | Phase 3 handoff section 7 |

---

## 9. Rules carried forward

- Hyphen only. Never the long dash, in code, comments, docs or commits.
- No AI attribution anywhere: no co-author trailers, no "generated with" lines, no session links.
- Agents do not commit or push. The user commits and runs `git push origin main`.
- Parallel agents never edit any `Cargo.toml`, Gradle file, `Package.swift` or `project.yml`. Dependencies are added up front in a serial step.
- Never bind or announce on 1714-1764, 53317 or 224.0.0.167.
- Manual runs use data and receive dirs on ext4 (`/tmp/pd-*`), never under the repo, and `HOME=/tmp/pd-home` or `--no-autostart` so the real user config is untouched.
- `docs/core-api.md` is frozen: a change needs a note in the phase handoff and an ffi version bump.
- Every phase ends by writing `docs/phases/phase-N-<name>.md` in this same shape.

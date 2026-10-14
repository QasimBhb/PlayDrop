# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this repo is

PlayDrop: an open-source phone-to-computer companion for the local network. It combines KDE Connect's
always-on companion link (ping, clipboard, battery, media, later notifications and remote input) with
LocalSend's fast, account-free file drop, and adds resume and steady speed for thousands of small files.
Targets: Android, iOS, Windows, Linux (Pop!_OS COSMIC first, Ubuntu GNOME second). Speed is the top priority.

State: Phase 3 complete on 2026-10-09 (one commit on `main`). Rust workspace with
`playdrop-core` (identity, pin store, TLS, session plane, pairing, transfer engine, discovery,
HTTP/2 net layer, host runtime, blobs, outbox, sealed secrets), `playdrop-cli`, `playdrop-ipc`
(D-Bus and named pipe contract), `playdropd` (daemon: tray, notifications, clipboard sync,
autostart, diagnostics, battery and presence), `playdrop-ffi` (UniFFI surface, frozen in
`docs/core-api.md`), `uniffi-bindgen`, `apps/desktop` (Slint window) and `apps/android`
(Kotlin/Compose app hosting the core in a `connectedDevice` foreground service), `apps/ios`
(SwiftUI app plus share extension hosting the core while in the foreground; Phase 4 code
complete on 2026-10-09, compiled and unit-tested by GitHub's macOS CI, and built, signed
and installed on an iPhone from this Linux machine with xtool since 2026-10-12; four of
five device gates passed that day, the share-extension one needs an App Group). Phase 5
(remote input, presenter mode, media control) written on 2026-10-12: core `input.*`,
daemon RemoteDesktop portal and `SendInput` injectors, MPRIS and GSMTC media, phone
trackpad, keyboard, presenter and now-playing screens, `playdrop-ffi` 0.2.0; live gates
that need someone at the screen are listed in `docs/phases/phase-5-companion.md` section 7.
Phase 6 (ship gate) written on 2026-10-13: GitHub Releases only (no stores), signed
Android release key outside the repo, release and Windows and nightly fuzz workflows,
`.deb` and Flatpak built locally, `fuzz/` with seven cargo-fuzz targets, `bench/` scripts
and `docs/release/` (distribution, test matrix, throughput notebook, SR checklist), media
buttons in the window, "Can't find your device?" help on all three platforms, the Android
`DownloadsSink` rewrite (1 GiB at 94 to 99% of iperf3 on the A54; small files still floored
at one MediaProvider insert per file, shipped as is, revisit in Phase 7), Flatpak autostart through the
Background portal, KDE Connect coexistence test. 394 Rust tests plus 110 Android JUnit
tests plus 102 Linux-run `PlayDropShared` Swift tests, clippy clean. Licence
GPL-3.0-or-later. UI native per platform. Build fully agentic. Next: close the owed items in
`docs/phases/phase-6-ship-gate.md` sections 6 and 7, then Phase 7.

## Read first

`PLAN.md` is the single project document. Read it before doing anything. It holds the research
(with source links), the architecture, the locked decisions, the open Phase 0 questions, the
anti-pattern list, the build phases, and the list of unverified claims. Do not re-research anything
that PLAN.md already cites. Do not restate its content here.

**One phase per session.** Each phase is built in its own session, directly on `main` (solo
project, no feature branches, no pull requests), and ends with a handoff document at `docs/phases/phase-N-<name>.md` (status, module map, public API,
decisions made, open TODOs, kickoff brief for the next phase). Starting a new phase: read
`PLAN.md`, `docs/protocol.md`, `docs/threat-model.md`, then the latest `docs/phases/` file, then
only the code the kickoff brief points at. Do not re-read the whole codebase. Finishing a phase:
write the next handoff file before the final commit.

Working pattern that has held up: the main session lays down skeletons and pre-adds every
dependency, then spawns parallel Opus agents confined to disjoint directories with no `Cargo.toml`
edits, each with unit tests and a clippy-clean bar, then one integration agent wires them and runs
the phase gates. Agents never commit; the user says "commit".

`.cursor/plans/implementation-plan.md` belongs to a parallel Cursor session. Treat its facts as
unverified unless PLAN.md cites them; the "Conflicts and unverified claims" section of PLAN.md lists
the ones that failed verification. Do not edit or delete Cursor's files.

## Architecture in one paragraph

A Rust core (`playdrop-core`) owns identity, discovery, pairing, TLS, the session plane and the
transfer engine. On desktop a daemon (`playdropd`) hosts it and owns the listening socket; on Android
a `connectedDevice` foreground service hosts it; on iOS the foreground app hosts it. Every UI is a
thin client of the host and never owns the socket. One TCP port, TLS 1.3 mutual auth, HTTP/2, with a
long-lived session stream plus resumable chunked file transfers on the same connection. Device
identity is the hash of the device's public key, pinned at pairing. Full detail and rationale in
PLAN.md under "Architecture" and "Decisions".

## Hard rules specific to this project

- Never bind or announce on ports 1714-1764 (KDE Connect) or 53317 / 224.0.0.167 (LocalSend) unless an
  explicit compatibility mode is turned on.
- Reusing code from KDE Connect (GPL-2.0-or-later) or LocalSend (Apache-2.0) is allowed under our
  GPL-3.0-or-later licence, but every reused file keeps its original copyright header, gets an SPDX
  tag, and is listed in `THIRD_PARTY.md`.
- The user's two defining complaints: KDE Connect pairing on COSMIC is flaky, and LocalSend needs too
  many taps per send. Every UX decision serves one-tap send and zero-action clipboard sync.
- No UI work until a Rust CLI can send a 500 MB file, be killed, and resume with a matching hash.
- Freeze the core API before generating UniFFI bindings.
- The full anti-pattern list (21 items) is in PLAN.md. Check it before designing anything.

## Build order and where we are

Phases are in PLAN.md "Build phases". Phase 0 is done: `docs/product.md` (scope), `docs/protocol.md`
(normative wire spec, port 42187, wins on wire behaviour), `docs/threat-model.md` (SR-1 to SR-34,
each tagged with the phase that must satisfy it), `LICENSE`, `THIRD_PARTY.md`. Phase 1 is done:
gates A, B, D, E are tests in `crates/playdrop-core/tests/phase1_gates.rs`; gate C is the manual
`playdrop bench` run described under Commands. Loopback numbers (release): 500 MiB resume at
~405 MiB/s, 5,000 small files at 0.88x of one big file, HTTP/2 ~2,080 MiB/s vs raw framing
~2,845 MiB/s (h2 about 27% slower; kept for v1, `playdrop/2` reserved for raw bulk). Phase 2 is
done on Linux: `playdropd` over D-Bus (`org.playdrop.PlayDrop.Daemon1`, spec in `docs/ipc.md`),
COSMIC receives with the window closed, desktop copy reaches a peer in 2 to 5 ms, restart needs
no re-pair, packaging for deb, Flatpak and Inno Setup written. Windows type-checks on
`x86_64-pc-windows-gnu` but has never been linked or run. Phase 3 is done: the core API is frozen
in `docs/core-api.md` (SendSource, ReceiveSink, SecretStore, blobs, battery, presence, media,
outbox), `playdrop-ffi` binds it with UniFFI, and the Android app pairs, receives a 400 MiB file
while locked, reconnects 4 s after a Wi-Fi drop and stays reachable in Doze (measured on a Galaxy
A54; details and the remaining manual checks in `docs/phases/phase-3-android.md`). Phase 4 is
written: `apps/ios` holds the SwiftUI app (foreground host, outbox pull on open, Local Network
fix-it screen, "Send Clipboard" Shortcut), the share extension (App Group queue handoff,
`playdrop://` with no parameters), XcodeGen `project.yml`, `scripts/build-core.sh`
(XCFramework) and the App Store copy. This machine has no Xcode: the Swift was parse-checked and the Foundation-only
`PlayDropShared` package unit-tested on Linux, GitHub's macOS CI compiles the app and runs
`PlayDropTests`, and xtool (`apps/ios/Package.swift`, `xtool.yml`,
`scripts/xtool-dev.sh`) builds, signs and installs the app on the user's iPhone from
Linux with a free personal Apple team (no App Group, no multicast: pair by address or QR).
Four of the five Phase 4 device gates passed on 2026-10-12; the share-extension one is
blocked without an App Group. Phase 5 is written: the core applies `input` frames only
inside a session, with the grant, unlocked, keys capped at 60 per second, 10 minute idle
timeout (SR-25), and tells the controller why a session ended; `playdropd` injects through
the RemoteDesktop portal (COSMIC, GNOME) or `SendInput`, shows "Remote input from X" with a
kill switch in the tray and window, and feeds MPRIS players to peers with the `media` grant.
Phase 6 is written: see `docs/phases/phase-6-ship-gate.md` for the gate results (throughput
passes, small files on Android sit at the MediaProvider floor by decision, CI unproven until the first push) and the list of
what the user must do before tagging `v0.1.0`. Open TODOs are tagged with their SR in the
code (`grep -rn "TODO" crates/ apps/`).

## Commands

Run `export PATH="$HOME/.cargo/bin:$PATH"` first.

- Build: `cargo build --workspace` (release: `cargo build --release`)
- Test all: `cargo test --workspace` (gates print their numbers with `-- --nocapture`; use `--release` for real throughput)
- One module or test: `cargo test -p playdrop-core transfer::` or `cargo test -p playdrop-core --test phase1_gates gate_a`
- Lint and format: `cargo clippy --workspace --all-targets` (zero warnings), `cargo fmt --all`
- Debug logs in tests: `PD_LOG=1 cargo test ...`; in the CLI: `RUST_LOG=playdrop_core=debug`
- System packages for the Slint window on Linux: `libfontconfig1-dev libxkbcommon-dev libgl1-mesa-dev`
- Daemon and window on this machine (data dirs must be on ext4, never under the repo; keep the real
  user config untouched): `HOME=/tmp/pd-home target/release/playdropd --data-dir /tmp/pd-a --receive-dir /tmp/pd-a-in --port 42187 --name alpha --no-tray --no-autostart`
  then `target/release/playdrop-desktop` (`--page <tab>` opens a tab; `PLAYDROP_SNAPSHOT=<png>` renders the window to a file and exits); drive the daemon from a shell with
  `busctl --user call org.playdrop.PlayDrop.Daemon1 /org/playdrop/PlayDrop/Daemon1 org.playdrop.PlayDrop.Daemon1 Request s '{"op":"status"}'`
- Live tests against a running daemon: `PD_PEER=<peer> cargo test -p playdrop-ipc --test live_daemon -- --ignored`, `cargo test -p playdropd -- --ignored clipboard`
- Two hosts for a manual transfer, one per terminal:
  `cargo run -p playdrop-cli -- host --data-dir /tmp/pd-a --receive-dir /tmp/pd-a-in --port 42187 --name alpha`
  `cargo run -p playdrop-cli -- host --data-dir /tmp/pd-b --receive-dir /tmp/pd-b-in --port 42188 --name bravo`
  then type `pair` in bravo, `pair 127.0.0.1:42188` in alpha, `y` in both, `peers`, and `send <short-id> <path>` (`help` lists all stdin commands)
- Benchmark (release): `target/release/playdrop bench --role server --addr 127.0.0.1:42200 --mode h2` in one terminal, `target/release/playdrop bench --role client --addr 127.0.0.1:42200 --mode h2 --size 1GiB --runs 3` in another; repeat with `--mode raw`; compare with `iperf3 -s` and `iperf3 -c 127.0.0.1 -t 5`
- Remote input and media (Phase 5): `cargo test -p playdrop-core --test phase5_core` (loopback, prints the latency gate with `-- --nocapture`); `cargo test -p playdropd -- --ignored input_portal_live` (opens the COSMIC consent dialog: someone must approve it) and `-- --ignored media_mpris_live --nocapture` (prints local players); in `playdrop host`: `input <peer> start|move dx dy|click|key Enter|text hi|stop`, `media <peer> <player> pause`, `inputs`, `stop-input`
- Release and bench (Phase 6): `bench/make-dataset.sh`, `bench/loopback.sh`, `bench/iperf3-phone.sh`, `bench/desktop-to-phone.sh --runs 3` (daemon alpha running, phone online; `bench/README.md`); fuzz: `cargo +nightly fuzz build` then `fuzz/README.md` (run the long gate from `/tmp/pd-fuzz-src`, a symlink without spaces); packaging: `packaging/deb/build-deb.sh`, `packaging/flatpak/README.md`; Android release APK: `./gradlew :app:assembleRelease` (real key from `apps/android/keystore.properties`) or `-Pplaydrop.signWithDebugKey=true` for the test phone; never measure speed with a debug APK
- LAN discovery test (needs a real interface with multicast): `cargo test -p playdrop-core --test phase1_gates -- --ignored`
- Android (from `apps/android`, Gradle 9.6, AGP 9.4.1, SDK platform 37.0, NDK 27.1, `cargo-ndk`): `./gradlew :app:compileDebugKotlin` (Kotlin check, regenerates the UniFFI bindings), `./gradlew :app:assembleDebug` or `assembleRelease` (Rust for arm64-v8a and x86_64 per `playdrop.rustAbis` in `gradle.properties`), `./gradlew :app:testDebugUnitTest`, `adb install -r app/build/outputs/apk/debug/app-debug.apk`, `adb logcat -s playdrop:*`. Bindings alone: `cargo build -p playdrop-ffi && cargo run -p uniffi-bindgen -- generate --library target/debug/libplaydrop_ffi.so --language kotlin --no-format --out-dir <dir>`
- iOS on Linux (from `apps/ios`, after the one-time xtool setup in its README): `scripts/xtool-dev.sh` builds the Rust core, the app and the share extension, signs them and installs on the USB iPhone (`--core` after a change under `crates/`); `xtool launch org.playdrop.ios`
- iOS on a Mac (Xcode 16+, XcodeGen, rustup; from `apps/ios`): `scripts/build-core.sh` (Rust for `aarch64-apple-ios` and `aarch64-apple-ios-sim`, Swift bindings, `PlayDropCoreFFI.xcframework`), `cp Local.xcconfig.example Local.xcconfig` and set the team, `xcodegen generate`, open `PlayDrop.xcodeproj`, run on a device. On Linux: `apps/ios/scripts/check-linux.sh` (needs a Swift toolchain in `$SWIFT_TOOLCHAIN`; parses every Swift file, runs `swift test` in `Packages/PlayDropShared`, clippy for both iOS Rust targets through stub `cc` and `ar`, validates plists, and type-checks the non-SwiftUI Swift with strict concurrency against stub Apple modules via `scripts/typecheck-linux.sh`)
- Windows type check without MinGW: point `CC_x86_64_pc_windows_gnu` at a script that writes empty object files and set `AR=ar`, then `cargo check --workspace --exclude playdrop-ffi --exclude uniffi-bindgen --target x86_64-pc-windows-gnu --all-targets`

## Writing and git rules

- Hyphen only. Never an em-dash, anywhere.
- No AI attribution in commits, PRs, or comments.
- Commit messages in plain language: what changed and why.
- Work on `main` only. No feature branches, no pull requests: the user is the only developer.
- Never push. After a commit, hand the user `git push origin main` and a one-line summary of what
  was committed. No PR title or description; this overrides the global PR rule for this repo.
- Env values go into every checkout and worktree plus `.env.example`; confirm gitignore first.
- Do not delete research material, plans or docs without asking. Research folders were wiped once
  by another tool and had to be recovered.

# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this repo is

PlayDrop: an open-source phone-to-computer companion for the local network. It combines KDE Connect's
always-on companion link (ping, clipboard, battery, media, later notifications and remote input) with
LocalSend's fast, account-free file drop, and adds resume and steady speed for thousands of small files.
Targets: Android, iOS, Windows, Linux (Pop!_OS COSMIC first, Ubuntu GNOME second). Speed is the top priority.

State: Phase 1 core complete on 2026-10-08. Rust workspace with `playdrop-core` (identity, pin store,
TLS, session plane, pairing, transfer engine, discovery, HTTP/2 net layer, host runtime) and
`playdrop-cli`. 176 tests, clippy clean, all five Phase 1 gates pass. Licence GPL-3.0-or-later.
UI native per platform. Build fully agentic. Next: Phase 2 desktop daemon.

## Read first

`PLAN.md` is the single project document. Read it before doing anything. It holds the research
(with source links), the architecture, the locked decisions, the open Phase 0 questions, the
anti-pattern list, the build phases, and the list of unverified claims. Do not re-research anything
that PLAN.md already cites. Do not restate its content here.

**One phase per session.** Each phase is built in its own session, on its own branch, and ends
with a handoff document at `docs/phases/phase-N-<name>.md` (status, module map, public API,
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
`playdrop bench` run described under Commands. Loopback numbers (release): 500 MiB
resume at ~405 MiB/s, 5,000 small files at 0.88x of one big file, HTTP/2 ~2,080 MiB/s vs raw
framing ~2,845 MiB/s (h2 about 27% slower; kept for v1, `playdrop/2` reserved for raw bulk if
phone CPU measurements in Phase 3 demand it). Open TODOs are tagged with their SR in the code
(`grep -rn "TODO" crates/`). Handoff for the next session: `docs/phases/phase-1-core.md`. Next is
Phase 2: `playdropd` daemon, tray, notifications, Slint window, Windows installer. Every PR must
say which SRs it satisfies.

## Commands

Run `export PATH="$HOME/.cargo/bin:$PATH"` first.

- Build: `cargo build --workspace` (release: `cargo build --release`)
- Test all: `cargo test --workspace` (gates print their numbers with `-- --nocapture`; use `--release` for real throughput)
- One module or test: `cargo test -p playdrop-core transfer::` or `cargo test -p playdrop-core --test phase1_gates gate_a`
- Lint and format: `cargo clippy --workspace --all-targets` (zero warnings), `cargo fmt --all`
- Debug logs in tests: `PD_LOG=1 cargo test ...`; in the CLI: `RUST_LOG=playdrop_core=debug`
- Two hosts for a manual transfer, one per terminal:
  `cargo run -p playdrop-cli -- host --data-dir /tmp/pd-a --receive-dir /tmp/pd-a-in --port 42187 --name alpha`
  `cargo run -p playdrop-cli -- host --data-dir /tmp/pd-b --receive-dir /tmp/pd-b-in --port 42188 --name bravo`
  then type `pair` in bravo, `pair 127.0.0.1:42188` in alpha, `y` in both, `peers`, and `send <short-id> <path>` (`help` lists all stdin commands)
- Benchmark (release): `target/release/playdrop bench --role server --addr 127.0.0.1:42200 --mode h2` in one terminal, `target/release/playdrop bench --role client --addr 127.0.0.1:42200 --mode h2 --size 1GiB --runs 3` in another; repeat with `--mode raw`; compare with `iperf3 -s` and `iperf3 -c 127.0.0.1 -t 5`
- LAN discovery test (needs a real interface with multicast): `cargo test -p playdrop-core --test phase1_gates -- --ignored`

## Writing and git rules

- Hyphen only. Never an em-dash, anywhere.
- No AI attribution in commits, PRs, or comments.
- Commit messages in plain language: what changed and why.
- Never push. Hand the user the `git push` command for the current branch, plus a PR title and a
  short bullet description.
- Env values go into every checkout and worktree plus `.env.example`; confirm gitignore first.
- Do not delete research material, plans or docs without asking. Research folders were wiped once
  by another tool and had to be recovered.

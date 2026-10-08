# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this repo is

PlayDrop: an open-source phone-to-computer companion for the local network. It combines KDE Connect's
always-on companion link (ping, clipboard, battery, media, later notifications and remote input) with
LocalSend's fast, account-free file drop, and adds resume and steady speed for thousands of small files.
Targets: Android, iOS, Windows, Linux (Pop!_OS COSMIC first, Ubuntu GNOME second). Speed is the top priority.

State: research and planning done, Phase 0 mostly closed on 2026-10-08 (see "User requirements" in
PLAN.md). No code yet. Licence is GPL-3.0-or-later. UI is native per platform. Build is fully agentic.

## Read first

`PLAN.md` is the single project document. Read it before doing anything. It holds the research
(with source links), the architecture, the locked decisions, the open Phase 0 questions, the
anti-pattern list, the build phases, and the list of unverified claims. Do not re-research anything
that PLAN.md already cites. Do not restate its content here.

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

Phases are in PLAN.md "Build phases". Phase 0 remaining items: pick the port and multicast group,
write `docs/protocol.md` (SAS, frame format, versioning) and `docs/threat-model.md`, add `LICENSE`
and `THIRD_PARTY.md`. Then Phase 1: Cargo workspace, `playdrop-core`, `playdrop-cli`, resumable
500 MB transfer between two processes, HTTP/2 vs raw framing benchmark in week one.

## Commands

None yet. When the Cargo workspace exists, record build, test, single-test and bench commands here.

## Writing and git rules

- Hyphen only. Never an em-dash, anywhere.
- No AI attribution in commits, PRs, or comments.
- Commit messages in plain language: what changed and why.
- Never push. Hand the user the `git push` command for the current branch, plus a PR title and a
  short bullet description.
- Env values go into every checkout and worktree plus `.env.example`; confirm gitignore first.
- Do not delete research material, plans or docs without asking. Research folders were wiped once
  by another tool and had to be recovered.

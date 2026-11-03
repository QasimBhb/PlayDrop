# Reach across networks with iroh: spike design (2026-11-03)

Purpose: the design the Phase 12 spike builds, and what it must show before anyone turns it on.
Background and the verdict to build it are in `docs/design/later-items.md` item 1. Wire details are
normative in `docs/protocol.md` section 12 and the requirements in `docs/threat-model.md` SR-35 to
SR-40.

## Scope

In: desktop hosts (`playdrop host`, `playdropd`) behind a cargo feature, proven on one machine with
network namespaces and a locally run relay.

Out, on purpose: Android and iOS (no `playdrop-ffi` change until the spike earns one), any UI beyond
a diagnostics line, pairing across networks, and any default relay.

## Three locks, all off by default

1. **Cargo feature `iroh`** on `playdrop-core`, forwarded by `playdropd` and `playdrop-cli`. Release
   artifacts do not enable it, so they contain none of this code and none of its dependencies.
2. **A relay the user names**, `--reach-relay <url>`. No URL means no endpoint is created and nothing
   is contacted. *Choice:* n0's public relays have no SLA, are rate limited and would learn who talks
   to whom, and "no cloud" is part of the pitch, so using them has to be somebody's explicit decision.
   iroh's `Minimal` preset is used and its address lookup left empty, and its portmapper feature is
   left out, so a host never asks the router to open a port.
3. **A per-device `remote` grant**, default off, on both sides. Without it nothing about reach is
   told to that peer, and its dials are refused.

## How a connection is made

- **Keys.** The endpoint's secret key is derived from the device key with BLAKE3 `derive_key`
  (context `playdrop 2026 reach endpoint key v1`). *Choice:* a derived key, not the device key itself,
  so one key is not used in two protocols, and because nothing has to trust the Endpoint ID (below).
- **Learning a peer's endpoint.** A new session frame, `reach`, sent over an authenticated session to
  a peer with the `remote` grant, carries the Endpoint ID and relay URL. They are stored with the pin.
  So reach works only after the two devices have met on a LAN or QR path while the grant was on.
- **Carrying the session.** One QUIC bidirectional stream carries today's TLS 1.3 mutual-auth and
  HTTP/2 exactly as a TCP socket would. The pin check (SR-11), grants, limits and every frame are the
  same code. The Endpoint ID only decides whether to spend effort: **the inner TLS decides trust**, so
  a lying relay or a forged ID gets no further than a failed handshake.
- **When.** Only after every LAN candidate of a dial has failed. *Choice:* it is the slow path, and
  LAN is the product.
- **Addresses.** A reach connection has no socket address, so it carries a synthetic one in the
  discard prefix (`100::/64`, RFC 6666) built from a hash of the Endpoint ID. It is never stored as
  last-known, never dialled, and gives the per-address limit of SR-15 a per-peer meaning.
- **Getting back to the LAN.** On a verified advert or a network change, a host on a reach connection
  runs the usual path check, except that the reach connection counts as 0 MiB/s, so any LAN candidate
  that answers wins without measuring. The usual rules hold: never during a transfer, and the session
  moves with no offline.

## Honest labels

`playdrop peers` and `playdrop diagnose` say when a peer is on the relay or a hole-punched direct path
(anti-patterns 13 and 18). Relay URLs and Endpoint IDs stay out of info logs (SR-32).

## What the spike must show

| Gate | Check | Where |
|---|---|---|
| R1 | With only the relay transport: a file arrives with a matching hash, a killed send resumes, the clipboard crosses | `tests/phase12_reach.rs` |
| R2 | An unknown endpoint, an unpinned one, and a pinned one with the grant off are refused, and a forged Endpoint ID fails inner TLS | `tests/phase12_reach.rs` |
| R3 | Feature off: no `iroh` in the dependency tree; feature on and no relay named: no endpoint exists | test and CI |
| R4 | A LAN path appearing moves the session to it with no offline; a host with a working LAN path never dials reach | `tests/phase12_reach.rs` |
| R5 | Two network namespaces behind NAT with no route except through a relay: a send works, and a packet counter shows nothing went to any host but the relay and the peer. Records relayed and direct speed | `bench/reach-netns.sh` |

## What the spike cannot show

A real carrier network, a real guest Wi-Fi, a phone, or the choice of who runs the relay. Those need
a person, a second network or a decision, and they are listed as owed in the phase handoff.

## Costs known going in

- More crates when the feature is on (iroh alone resolves to 370, some shared with ours; the phase
  handoff records the real increase), and they are in `Cargo.lock` and the Flatpak source list either
  way, because a lock file records optional dependencies.
- One more layer of encryption on this path: QUIC outside, TLS inside. Accepted, since it keeps trust
  in one place and this is the slow path anyway.

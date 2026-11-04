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
   left out, so a host never asks the router to open a port. Relay only by default: no IP socket at
   all. A direct path needs `--reach-direct` too, because a hole-punching host sends UDP to whatever
   address the other end names, a stranger included (T45).
3. **A per-device `remote` grant**, default off, on both sides. Without it nothing about reach is
   told to that peer, and its dials are refused.

## How a connection is made

- **Keys.** The endpoint's secret key is derived from the device key with BLAKE3 `derive_key`
  (context `playdrop 2026 reach endpoint key v1`). *Choice:* a derived key, not the device key itself,
  so one key is not used in two protocols, and because nothing has to trust the Endpoint ID (below).
- **Learning a peer's endpoint.** A new session frame, `reach`, sent over an authenticated session to
  a peer with the `remote` grant, carries the Endpoint ID and relay URL. They are stored with the pin.
  So reach works only after the two devices have met on a LAN or QR path while the grant was on.
  They are stored in `reach.json` beside the pin file, not in it: `Pin` and `Grants` refuse unknown
  fields, so any new field would stop a release build from loading `pins.json`, and `playdrop-ffi`
  builds `Grants` with a struct literal.
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
(anti-patterns 13 and 18). Relay URLs and Endpoint IDs stay out of info logs (SR-32). iroh itself
names the relay at info level and tags every line with the Endpoint ID, so a host's log setup turns
its targets off unless the filter names `iroh`.

## What the spike must show

| Gate | Check | Where |
|---|---|---|
| R1 | With only the relay transport: a file arrives with a matching hash, a killed send resumes, the clipboard crosses | `tests/phase12_reach.rs` |
| R2 | An unknown endpoint, an unpinned one, and a pinned one with the grant off are refused, and a forged Endpoint ID fails inner TLS | `tests/phase12_reach.rs` |
| R3 | Feature off: no `iroh` in the dependency tree; feature on and no relay named: no endpoint exists | test and CI |
| R4 | A LAN path appearing moves the session to it with no offline; a host whose own LAN dial works does not dial reach. A one-way LAN does take the relay for that host's sends while the session stays on the LAN, as HTTP/2 lets it send only on a connection it dialled, and the label says so | `tests/phase12_reach.rs` |
| R5 | Two network namespaces behind NAT with no route except through a relay: a send works, and a packet counter shows nothing went to any host but the relay and the peer. Records relayed and direct speed | `bench/reach-netns.sh` |

## What it measured (2026-11-03, `bench/reach-netns.sh`, 1 GiB, one machine)

| Path | MiB/s | Sent by the sender |
|---|---|---|
| Relay, https, private CA (the default) | 182 | 1099 MiB to the relay, 0 to anyone else |
| Hole-punched direct, `--reach-direct` | 356 | 1052 MiB to the peer, 104 packets to the relay |
| Relay, `--dev` (plain http) | 95 | 1124 MiB to the relay, 0 to anyone else |

Relay runs fall into two groups, about 180 to 228 MiB/s and about 95, on either relay mode (94 once on
https, 95 once on `--dev`, among a dozen runs). The cause was not found, so one relay figure alone
says little. The relay's own log shows it dropping packets when its queue is full.

These are veth pairs on one machine with no real latency, so the numbers show the cost of the code
and the crypto, not what a real network gives. Every stage: the file's hash matched, and the counter
for any other destination stayed 0 (a test packet first proves it counts).

Learned from it:

- **The sender's 16 MiB buffer stays.** A 4 MiB one was tried for the revoke frame: relay speed was the
  same within noise (4 MiB: 189, 189; 16 MiB: 205, 181, 185 MiB/s), and the revoke test passed 20 rounds
  of 20 with 16 MiB, so it brought nothing.
- **A relay drops packets under load, and a large QUIC window turns that into a dead connection.**
  An 8 MiB stream window hit "too many gaps in stream buffer" within a second; 2 MiB cannot.
- **A NAT that leaves its own input open defeats hole punching at normal speed.** The first punch
  packet leaves a conntrack entry, the host's own packet collides with it, and the NAT gives that
  packet another port. iroh then stays on the relay, which still works. Dropping unsolicited input,
  as a router's firewall does, fixed it 3 runs out of 3. Real routers built the first way will sit on
  the relay.
- **Reach dials must not hold the dial lock.** With a stale silent LAN address and the peer away,
  dial tasks grew by about 8 a minute behind the lock and `connect()` was still blocked at 120 s. A
  single-flight dial outside the lock ended that.
- **A cold dial can take about 5 s** when an old LAN address silently drops packets, because reach
  waits for every LAN candidate to fail.
- `--dev` mode is plain http with no QUIC address discovery, as that needs TLS. Hole punching needs
  discovery, so the direct stage runs against TLS: the script makes a private CA with an IP address
  SAN and passes it with `--reach-relay-ca`.

## Required before reach leaves spike status

- **Proof of possession of the endpoint key in the `reach` frame.** Today nothing proves a peer holds
  the key behind the Endpoint ID it names, so the first to name an ID keeps it (T46): a paired peer
  can block reach to another device from this host, and costs that device a failed handshake now and
  then. Inner TLS still decides trust, so this is a denial of reach, not a way in.

## What the spike cannot show

A real carrier network, a real guest Wi-Fi, a phone, or the choice of who runs the relay. Those need
a person, a second network or a decision, and they are listed as owed in the phase handoff.

## Costs known going in

- Measured: `playdropd` goes from 276 to 457 crates with the feature on, and none with it off. That
  is true of the crate count only: the shared lock file moves default builds from rustls 0.23.45 to
  0.23.46.
  `Cargo.lock` goes from 762 to 906 entries and the Flatpak source list grows to match, either way,
  because a lock file records optional dependencies.
- One more layer of encryption on this path: QUIC outside, TLS inside. Accepted, since it keeps trust
  in one place and this is the slow path anyway.

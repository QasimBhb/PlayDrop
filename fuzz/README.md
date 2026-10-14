# PlayDrop fuzz targets

cargo-fuzz targets for every decoder that reads bytes from the network before (or without)
authentication. They serve threat-model.md **SR-33** ("cargo-fuzz targets for beacon, TXT,
manifest, bundle and frame decoders run nightly") and threat **T39** (parser bugs before
authentication). The Phase 6 ship gate needs 8 CPU-hours per target with no crash.

This directory is its own Cargo workspace, so the main build never needs nightly or
libFuzzer. The core's parsers are covered by the workspace-wide `unsafe_code = "forbid"`
lint. cargo-fuzz builds with `--cfg fuzzing`, which turns on the small
`playdrop_core::fuzz_api` module (today only the mDNS TXT entry point, since `mdns-sd`
does not let outside code build a resolved service).

## Targets

| Target | Input | What it drives | SR |
|---|---|---|---|
| `beacon` | one UDP datagram | `Advert::from_beacon`, then the full receive path: rate limiter, 512-byte cap, own-nonce drop, on-link check, tag verification, "Add device" list. A re-tagged copy of each parsed advert checks the verified path and the tag cache. Parsed adverts must round trip and have display-safe names. | SR-1, SR-2, SR-3, SR-8 |
| `mdns_txt` | 2-byte SRV port, then raw TXT RDATA | `mdns-sd`'s TXT decoder plus `Advert::from_txt`, the path every resolved service takes. Parsed adverts must round trip through TXT and stay under 400 bytes. | SR-2, SR-4 |
| `manifest` | a `POST /v1/transfers` body | serde into `Manifest`, `Manifest::validate`, then `BundleLayout` for every valid manifest. Checks that the path, entry and size rules really hold and that the layout matches `bundle.size`. | SR-17, SR-19, SR-20 |
| `bundle` | structured (`arbitrary`): files, resume offset, read sizes, corruptions or raw bytes | The receiver's bundle path in memory: chunk records through `ChunkDecoder` (BLAKE3 check), verified bytes into `BundleReader`. A genuine archive must be accepted and give back exactly the member bytes; a damaged one may be refused but never panic or emit data outside a member. Nothing touches the filesystem. | SR-19, SR-22 |
| `frame` | 1 byte picking read sizes, then raw session-stream bytes | Both decoders, `FrameDecoder` and `InboundDecoder`, fed whole and in pieces, plus the `FrameReader` async adapter. All must agree; accepted frames respect the 64 KiB cap and the seq rule, and re-encode with `OutboundEncoder` to the same frame. | SR-15, SR-16 |
| `session_json` | frame JSON payloads separated by byte `0x1E` | Every frame type straight from JSON (validation, `hello` checks, capability and grant tables, round trip), the shared `accept_payload` path, and four pairing state machines (SAS and QR, both roles). Every frame a state machine sends must encode, and nothing comes out after an attempt ends. | SR-6, SR-8, SR-9, SR-12, SR-16 |
| `qr_uri` | scanned QR text | `QrPayload::parse`. Accepted codes carry a real Device ID, fingerprint, 1 to 16 addresses and a secret, and survive `to_uri` unchanged. | SR-9 |

Seeds live in `seeds/<target>/` and are committed. They are valid samples from
`docs/protocol.md` and the core's unit tests; `make-seeds.sh` rewrites them. libFuzzer's
growing corpus goes to `corpus/` and crash inputs to `artifacts/`, both gitignored.

## Running

Needs a nightly toolchain and cargo-fuzz (`cargo install cargo-fuzz --version 0.13.2`).
Run from the repo root; cargo-fuzz finds this directory.

```sh
export PATH="$HOME/.cargo/bin:$PATH"
cargo +nightly fuzz list
cargo +nightly fuzz build                      # all targets
mkdir -p fuzz/corpus/frame
cargo +nightly fuzz run frame fuzz/corpus/frame fuzz/seeds/frame -- \
    -max_total_time=120 -rss_limit_mb=2048
```

The first corpus directory receives new inputs; the seed directory is only read. Smoke-run
speeds on this machine (one core each, other work running, 120 s):

| Target | exec/s |
|---|---|
| beacon | ~10,000 |
| mdns_txt | ~18,000 |
| manifest | ~14,700 |
| bundle | ~4,000 |
| frame | ~3,000 |
| session_json | ~1,700 |
| qr_uri | ~40,000 |

## Ship gate: 8 CPU-hours per target

Plan for 8 cores. One target at a time with `-fork=8` for one hour is 8 CPU-hours, so the
whole gate takes about 7 hours of wall time:

```sh
for t in beacon mdns_txt manifest bundle frame session_json qr_uri; do
  mkdir -p fuzz/corpus/$t
  cargo +nightly fuzz run $t fuzz/corpus/$t fuzz/seeds/$t -- \
      -fork=8 -max_total_time=3600 -rss_limit_mb=2048 -timeout=10 \
      -print_final_stats=1 2>&1 | tee fuzz/gate-$t.log
done
```

`-fork=8` runs 8 child processes and stops at the first crash (`-ignore_crashes` is off).
Fork mode hands the artifact path to its children without quoting, so a checkout under a
path with a space (this machine: `New Volume`) needs `-artifact_prefix=/tmp/pd-fuzz-artifacts/<target>/`
added after `--`, pointing at a path without spaces; the same goes for `cmin` and `tmin`.
A target passes when its log ends with `DONE` and the artifact directory holds no `crash-*`
file. A `timeout-*` file counts only if it is slow on its own: replay it with the target
binary and time it. Fork children on a loaded machine (a Gradle build, a full `cargo test`)
can stall past the 10 s limit on an input that replays in milliseconds; the 2026-10-13 gate
run produced six such files for `beacon`, every one replaying in 40 ms.
Record the date, commit, total executions and final coverage line of each log in the ship
gate fuzz report.

## Nightly CI

`.github/workflows/fuzz.yml` runs each target for about ten minutes every night (and on
demand, with a custom duration), keeps the corpus in the Actions cache from night to night,
and uploads `fuzz/artifacts/<target>/` when a target crashes.

## When a target crashes

1. Reproduce: `cargo +nightly fuzz run <target> fuzz/artifacts/<target>/crash-<hash>`
   (add `RUST_BACKTRACE=1` for the stack).
2. Minimise: `cargo +nightly fuzz tmin <target> fuzz/artifacts/<target>/crash-<hash>`;
   the smallest input is written next to it as `minimized-from-<hash>`.
3. For structured targets (`bundle`), print the decoded input with
   `cargo +nightly fuzz fmt <target> <file>`.
4. Fix the bug in the core with the smallest correct change, and add a unit test next to the
   decoder that feeds the minimised input (or the value it decodes to), so the bug stays
   fixed without nightly.
5. Shrink a corpus that has grown large: `cargo +nightly fuzz cmin <target>`.

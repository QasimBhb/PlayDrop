# Phase 1 handoff: Rust core and CLI

Reader: a fresh session that must build Phase 2 (the desktop daemon) on top of `playdrop-core` without re-reading its 23,000 lines. Read `CLAUDE.md` and `PLAN.md` ("Build phases", "Anti-patterns") first; this file covers only what the code does today. `docs/protocol.md` is normative on wire behaviour; `docs/threat-model.md` section 4 holds SR-1 to SR-34.

---

## 1. Status

| Item | Value |
|---|---|
| Commit | `c8378f9` "Phase 1: Rust core and command line with resumable transfers" (2026-10-08) |
| Crates | `playdrop-core` (library), `playdrop-cli` (binary `playdrop`) |
| Tests | 176 passed, 0 failed, 3 ignored (`cargo test --workspace`): 168 core unit, 3 `host_smoke`, 4 `phase1_gates`, 1 CLI. Ignored: two real-multicast discovery tests and the LAN pairing gate |
| Lint | `cargo clippy --workspace --all-targets` zero warnings (pedantic on, `unsafe_code = "forbid"`) |
| Toolchain | Rust 1.99, `export PATH="$HOME/.cargo/bin:$PATH"` |

**Gates** (PLAN.md Phase 1 "done when"):

| Gate | Where | Result |
|---|---|---|
| A: 500 MiB file, receiver killed mid-transfer, resumes, hash matches, no re-pair | `tests/phase1_gates.rs` `gate_a_kill_and_resume_500mib` | Pass. Release ~405 MiB/s on loopback (CLAUDE.md); a debug run here: kill at 356,610,756 bytes, restart 0.74 s, 394.9 MiB/s overall, 1 reconnect |
| B: 5,000 small files hold throughput | `gate_b_many_small_files_hold_throughput` | Pass. Release 0.88x of one big file (CLAUDE.md); a debug run here: 407.7 vs 322.6 MiB/s, ratio 0.79. Floor 0.7 release, 0.5 debug |
| C: HTTP/2 vs raw framing, iperf3 recorded | Manual: `playdrop bench` (see CLAUDE.md Commands). Not a test | h2 ~2,080 MiB/s, raw ~2,845 MiB/s on loopback (h2 ~27% slower, kept for v1) |
| D: daemon restart needs no re-pair | `gate_d_restart_reconnects_without_pairing` | Pass, reconnected 27 ms after restart |
| E: no 1714-1764, 53317, 224.0.0.167, no `kdeconnect` on the wire | `gate_e_no_kde_ports_and_no_kdeconnect_on_the_wire` | Pass |
| LAN discovery then pair | `lan_discovery_add_device_then_pair` (`--ignored`) | Needs a real multicast interface; not run in CI |

**Explicitly not done in Phase 1**

- No `playdropd` binary. PLAN.md Phase 1 names `playdropd` and `playdrop send <peer> <files>`; what exists is `playdrop host` (a foreground host with a stdin REPL) and `playdrop bench`.
- Outbox (protocol.md 7.8), blobs (5.15), media (5.9) and input (5.12) are not implemented.
- Sender-side resume persistence (7.6): a restarted sender re-offers.
- Windows: compiles in principle, never built or run. DPAPI, `SO_EXCLUSIVEADDRUSE`, free space, file IDs and `Zone.Identifier` are missing.
- iperf3 on a real Wi-Fi link: unmeasured.
- Fuzz targets (SR-33): not written.

---

## 2. Repository map

```
Cargo.toml                       workspace; lints: clippy pedantic, unsafe forbidden, missing_docs warn
crates/playdrop-core/src/
  lib.rs                         module list, re-exports `Error`, `Result`
  constants.rs                   protocol.md 1: port 42187, group 239.255.42.187, ALPN, chunk sizes, FORBIDDEN_PORTS
  error.rs                       `Error` enum (Io, Tls, NotPaired, Forbidden, PortHeld, NoSpace, ...) mapped to 9 error codes
  identity/                      protocol.md 2; SR-10, SR-28. Ed25519 key, self-signed cert, DeviceId, Short ID
    secure_fs.rs                 0700/0600 checks, atomic private writes (SR-28); no-op checks on non-Unix
  pin_store/                     6.4, 8.2; SR-13, SR-14, SR-28. pins.json: Pin, Grants, announce keys, last-known
  tls/                           3.1, 4; SR-11, SR-12. rustls configs, pin-only verifiers, exporter, cipher order
  net/                           3.1-3.4, 5.1, 7.1, 10; SR-11, SR-13, SR-15, SR-31. h2 settings, body helpers
    server.rs                    exclusive bind, connection limits, TLS + h2 accept, Router trait
    client.rs                    staggered 250 ms dial (3.2), h2 client handshake
    connection.rs                one authenticated connection: peer ID, exporter, keep-alive, reset counter
  session/                       4, 5, 6; SR-6 to SR-9, SR-13 to SR-16. Transport-agnostic
    frame.rs                     u32 length + JSON codec, 64 KiB cap, seq replay rule
    types.rs                     every frame struct, Capability, GrantKind, ErrorCode, clipboard hash
    hello.rs                     hello validation, name sanitising (SR-8), TrustCheck
    pairing.rs                   SAS and QR state machines, AddDeviceWindow, PairRequestLimiter
    limits.rs                    Limits, HandshakeGate, ConnectionCounter, ResetRateCounter (SR-15)
  transfer/                      7; SR-17 to SR-23. Network-agnostic engine
    manifest.rs                  manifest build and validation (SR-17, SR-20), SourceInfo (size, mtime, inode)
    chunk.rs                     chunk records, streaming BLAKE3, root and prefix hashes
    sender.rs                    TransferSender, ResumePlan (SR-21)
    receiver.rs                  TransferReceiver: .part staging, durable offsets, retries (SR-22), finalize
    bundle.rs                    deterministic ustar writer, own-loop reader (7.7, SR-19)
    fsx.rs                       beneath-only walks, create_new, " (n)" collisions (SR-18, SR-23)
  discovery/                     8; SR-1 to SR-5
    tag.rs                       announce-key HMAC tag (8.2)
    advert.rs                    TXT/beacon encode and parse, sanitize_name
    limits.rs, cache.rs          rate limits, DialLimiter (5 s), LRU cache 256/120 s, add-device list 32
    engine.rs                    socket-free decision logic shared by both carriers
    beacon.rs, mdns.rs           the two carriers (mdns-sd 0.21)
    service.rs                   `Discovery` facade, concurrent tasks
  host/                          the runtime a daemon embeds; glues everything above
    mod.rs                       Host, HostConfig, PeerSummary, AddDeviceCandidate, KeepAliveProfile
    events.rs                    HostEvent, Secret<T>, DiscoveryPath, PairingPromptKind, FileRoot
    registry.rs                  connections per peer, session placement (3.3)
    session.rs                   session stream, frame handlers, grants check, unpair
    pairing.rs                   drives session::pairing over a session stream (6.1-6.3)
    send.rs / receive.rs         transfer plane over h2 (7.1-7.7)
    router.rs                    request routing (7.1); outbox list stub, blobs 404
    connect.rs                   last-known dial loop, discovery wiring, 10 s diagnostic
    pins.rs                      RwLock<PinStore> shared with the TLS verifier, async writers
    platform.rs                  VolumeSpace (statvfs), SystemInterfaces (if-addrs), local_addresses
crates/playdrop-core/tests/      host_smoke.rs (pair, send, ping, grants, unpair, QR, SR-13), phase1_gates.rs, common/
crates/playdrop-cli/src/         main.rs (clap), repl.rs (`host` REPL), bench.rs (h2 vs raw)
```

---

## 3. Public API a host embeds (`playdrop_core::host`)

This is the contract Phase 2 builds `playdropd` against. Everything is async on tokio (multi-thread runtime).

### 3.1 Start and stop

```rust
pub async fn start(config: HostConfig) -> Result<(Self, mpsc::Receiver<HostEvent>)>
pub async fn shutdown(self)
```

- `start` loads or creates the identity and `pins.json`, binds exclusively (`Error::PortHeld { port }` if taken), starts the server, reloads unfinished incoming transfers, starts discovery and the last-known dial loop, then emits `HostEvent::Listening`.
- `shutdown` takes `self` by value: a daemon holding `Arc<Host>` must `Arc::try_unwrap` it (the CLI does). Dropping a `Host` without `shutdown` leaves its tasks running.
- The event channel holds 4,096 events and **drops events when full**. It has one receiver; the daemon must drain it continuously and fan out to UI clients itself.

### 3.2 `HostConfig`

```rust
pub struct HostConfig {
    pub data_dir: PathBuf,                      // identity, pins, transfer index; 0700 (SR-28)
    pub receive_dir: PathBuf,                   // received files; created 0700 (SR-23)
    pub port: u16,                              // 42187; 0 = any free port, tests only
    pub device_name: String,                    // sanitised (SR-8)
    pub device_type: DeviceType,                // Desktop, Laptop, Phone, Tablet, Tv
    pub keep_alive_profile: KeepAliveProfile,   // Desktop (60 s) or Mobile (300 s)
    pub interfaces: Box<dyn InterfaceSource>,   // IPv4 interface list for discovery
    pub bind_ip: IpAddr,                        // default `::` dual-stack
    pub discovery: bool,                        // mDNS + beacon on
}
pub fn new(data_dir: impl Into<PathBuf>, receive_dir: impl Into<PathBuf>,
           device_name: impl Into<String>) -> Self   // desktop defaults, SystemInterfaces
```

`receive_dir`, `device_name` and `port` are fixed for the host's lifetime; changing them means restart.

### 3.3 `Host` methods

| Signature | What it does |
|---|---|
| `fn device_id(&self) -> DeviceId` | This device |
| `fn short_id(&self) -> String` | `k3qd-7f2m-xa9p-41bc` form (SR-8) |
| `fn local_addr(&self) -> SocketAddr` | Bound address |
| `fn peers(&self) -> Vec<PeerSummary>` | Paired peers: `device_id`, `short_id`, `label`, `online`, `grants`, `last_known`, `paired_at` (Unix s) |
| `fn resolve_peer(&self, query: &str) -> Option<DeviceId>` | By hex ID, Short ID prefix (hyphens optional) or exact label; `None` if ambiguous |
| `fn is_online(&self, peer: &DeviceId) -> bool` | Session up |
| `async fn connect(&self, peer: &DeviceId) -> Result<()>` | Dial last-known addresses now |
| `fn grants(&self, peer: &DeviceId) -> Option<Grants>` | Read grants |
| `async fn set_grants(&self, peer: &DeviceId, grants: Grants) -> Result<()>` | Local UI only (SR-14); persisted |
| `async fn set_label(&self, peer: &DeviceId, label: String) -> Result<()>` | Local label (SR-8) |
| `async fn unpair(&self, peer: &DeviceId) -> Result<()>` | Sends `unpair`, closes within 1 s, deletes pin, grants, tokens, transfers, rotates announce key |
| `fn open_add_device_window(&self) -> Result<()>` | 120 s window (SR-6); advertises name; accepts unpinned certs |
| `fn close_add_device_window(&self)` | Also drops any QR offer |
| `fn add_device_window_open(&self) -> bool` | Window state |
| `fn add_device_list(&self) -> Vec<AddDeviceCandidate>` | `name`, `device_type`, `addr` of devices in their own window |
| `async fn pair_with(&self, addr: SocketAddr) -> Result<()>` | SAS pairing as initiator; opens local window if needed |
| `fn pairing_respond(&self, accept: bool)` | Answer the current `PairingPrompt` |
| `fn show_qr(&self) -> Result<String>` | Opens window, returns one-use QR URI valid 120 s. Never log it |
| `async fn scan_qr(&self, uri: &str) -> Result<()>` | Pair as QR scanner |
| `async fn send(&self, peer: &DeviceId, paths: Vec<PathBuf>) -> Result<TransferHandle>` | Files and folders; 4 file PUTs plus bundle; resumes after drops for up to 10 min |
| `async fn respond_offer(&self, id: &str, accept: bool) -> Result<()>` | Answer a `TransferOffer` |
| `fn ping(&self, peer: &DeviceId, message: Option<&str>) -> Result<()>` | `Error::NotFound` if no session |
| `fn send_clipboard(&self, peer: &DeviceId, text: &str, sensitive: bool) -> Result<bool>` | Needs `clipboardOut`; `Ok(false)` when it would echo; `Error::TooLarge` over 32 KiB |

### 3.4 `TransferHandle` (send side only)

```rust
pub fn id(&self) -> &str
pub fn total_bytes(&self) -> u64
pub fn bytes_sent(&self) -> u64          // re-sent bytes count again
pub fn cancel(&self)                     // receiver deletes its .part files
pub async fn wait(&mut self) -> Result<TransferSummary>
```

`TransferSummary { id, peer, bytes, elapsed_ms, roots: Vec<FileRoot>, bundle_root, reconnects }`.

### 3.5 `HostEvent` (`#[non_exhaustive]`, serde tag `event`, snake_case)

Peers are hex Device IDs. `Secret<T>` serialises the value but prints `(redacted)` in `Debug` (SR-32): never log it.

| Variant | Fields | What the UI does |
|---|---|---|
| `Listening` | `addr`, `device_id`, `short_id` | Show own identity in settings and diagnostics |
| `PeerOnline` | `peer`, `short_id`, `name`, `addr` | Mark device online; `name` is display only |
| `PeerOffline` | `peer`, `short_id` | Mark offline (or "asleep" if last `Presence` was `asleep`) |
| `PairingPrompt` | `peer`, `short_id`, `name`, `prompt: PairingPromptKind` | `Sas { sas }`: show 6 digits, Short ID, name; no default-focused button (SR-8); answer `pairing_respond`. `ScannerConfirm`: "Add name (Short ID)?" |
| `Paired` | `peer`, `short_id`, `name` | Desktop notification "new device paired" (SR-8); refresh list |
| `PairingFailed` | `reason` | Show reason, offer retry |
| `RemovedByPeer` | `peer`, `short_id` | "This device no longer has you paired; pair again" |
| `Unpaired` | `peer`, `short_id` | Remove from list |
| `AddDeviceSeen` | `name`, `device_type`, `addr` | Add to "Add device" list; dial only on user pick |
| `TransferOffer` | `id`, `peer`, `short_id`, `files`, `total_bytes`, `note` | Only when `files` grant is off: prompt, answer `respond_offer` |
| `TransferProgress` | `id`, `peer`, `direction`, `bytes`, `total`, `bytes_per_sec` | Progress bar; every ~500 ms |
| `TransferComplete` | `id`, `peer`, `direction`, `bytes`, `elapsed_ms`, `roots`, `bundle_root` | Notification "Open" / "Show in folder" (see gap 1) |
| `TransferFailed` | `id`, `peer`, `direction`, `reason` | Notification with reason (SR-22 wording) |
| `Clipboard` | `peer`, `text: Secret<Option<String>>`, `sensitive` | Write to system clipboard; if `sensitive`, disable history/cloud (SR-24). `None` = image blob, unfetchable today |
| `Ping` | `peer`, `message` | Notification |
| `Find` | `peer`, `start` | Ring / stop |
| `Battery` | `peer`, `level`, `charging`, `low` | Device list and tray |
| `Presence` | `peer`, `state` | `active`, `idle`, `locked`, `asleep` |
| `Unsupported` | `peer`, `frame_type` | Media, input, `outbox_ready`: log at debug, ignore |
| `DiscoveryDiagnostic` | `peer`, `short_id`, `path: DiscoveryPath`, `failure` | Diagnostics screen: `Mdns`, `Beacon` or `LastKnown` and why. Emitted once, 10 s after start, per unreached pinned peer |

### 3.6 Gaps for Phase 2

None of these exist. Close the first six before the daemon tasks start (they change the core API, so one agent does them, serially).

1. **Received file paths.** Receive-side `TransferComplete` has empty `roots` and no destination paths, though `transfer::receiver` computes each `dest` in `finalize`. "Open" and "Show in folder" need them (PLAN.md UX). Add e.g. `saved: Vec<PathBuf>` and the folder.
2. **Cancel and list transfers by ID.** No `Host::cancel_transfer(id)` for an incoming transfer and no `Host::transfers()` snapshot for a UI that connects after events were emitted.
3. **Shutdown by reference.** `shutdown(self)` forces `Arc::try_unwrap`; a daemon with IPC tasks needs `shutdown(&self)`.
4. **Lock state for SR-6.** `open_add_device_window` cannot refuse while the desktop is locked (TODO in `host/pairing.rs`). Add a host-supplied lock-state source (logind `LockedHint`, `WTS_SESSION_LOCK`).
5. **Clipboard push fixes.** `send_clipboard` stores `last_sent` before the size and session checks, so a failed push suppresses a retry of the same text. No image clipboard; text over 32 KiB fails until blobs exist. A daemon broadcast helper (`push_clipboard` to every peer with `clipboardOut`) would keep echo logic in one place.
6. **Diagnostics on demand.** Only the one-shot 10 s report exists. Phase 2 needs `Host::diagnose()` on demand, a self-check that tells "port 42187 blocked by firewall" apart from "multicast blocked", and a passthrough to `Discovery::network_changed` when the OS reports a network change.
7. Send `find`, `battery`, `presence` and `capabilities`: advertised in `caps.out` but no `Host` method sends them.
8. `PeerSummary` has no `last_seen` (SR-8 asks the device list to show it).
9. Runtime setters for `receive_dir` and `device_name`.
10. `TransferOffer` carries no file names for the prompt.
11. Windows platform items in section 7 (SR-20, SR-28, SR-31, SR-21 file ID, SR-23).

---

## 4. Key types in other modules

| Type | Path | One line |
|---|---|---|
| `DeviceIdentity` | `identity` | Keypair plus cert; `load_or_create(dir)`, `device_id()`, `short_id()`, `fingerprint()`, `certificate_der()` |
| `DeviceId` | `identity` | 32-byte SPKI hash; `to_hex()`, `short_id()`, `FromStr`, serde as hex; `Debug` is truncated |
| `identity::default_dir()` | `identity` | `$XDG_DATA_HOME/playdrop` on Linux via `directories`, platform local data dir elsewhere |
| `CERT_FILE`, `KEY_FILE` | `identity` | `identity.crt`, `identity.key` |
| `PinStore` | `pin_store` | One JSON file, in-memory, not thread-safe; `open`, `save`, `insert`, `remove` (rotates local announce key), `set_grants`, `set_label`, `push_last_known` |
| `Pin` | `pin_store` | `device_id`, `spki_hash`, `cert_der`, `label`, `grants`, `announce_key`, `last_known` (max 8), `paired_at` |
| `Grants` | `pin_store` | `files`, `clipboard_in`, `clipboard_out`, `input`, `media`, `find`; `Default` = all on except `input`; serde camelCase |
| `PIN_FILE` | `pin_store` | `pins.json` |
| `InterfaceSource` | `discovery` | Trait: `fn ipv4_interfaces(&self) -> Vec<Ipv4Interface>`. `SystemInterfaces` (if-addrs) and `StaticInterfaces` (settable) provided |
| `Ipv4Interface` | `discovery` | `index`, `addr`, `netmask`; on-link check |
| `FreeSpace` | `transfer` | Trait: `fn space(&self, path: &Path) -> io::Result<SpaceInfo>`; `host::platform::VolumeSpace` is fixed in `Inner` (not configurable via `HostConfig`) |
| `SpaceInfo` | `transfer` | `available`, `total`; reserve = max(2 GiB, 5%) |
| `Frame::Clipboard(Clipboard)` | `session::types` | `kind`, `mime`, `hash`, `text`, `blob`, `sensitive`; `clipboard_text_hash(text)` normalises CRLF |
| `Frame::Ping(Ping)` / `Pong` | `session::types` | `Ping::new(Option<&str>)` cuts to 256 bytes |
| `Frame::Battery(Battery)` | `session::types` | `level`, `charging`, `low` |
| `Frame::Presence(Presence)` | `session::types` | `state: PresenceState`, `reason`, `expect_back_ms` |
| `DeviceType` | `session::types` | `Desktop`, `Laptop`, `Phone`, `Tablet`, `Tv` |
| `Error` | crate root | `PortHeld { port }`, `InsecurePermissions`, `NotPaired`, `Forbidden`, `TooLarge`, `NoSpace`, ... |

---

## 5. How it runs today

Two hosts on one machine, one per terminal:

```
cargo run -p playdrop-cli -- host --data-dir /tmp/pd-a --receive-dir /tmp/pd-a-in --port 42187 --name alpha
cargo run -p playdrop-cli -- host --data-dir /tmp/pd-b --receive-dir /tmp/pd-b-in --port 42188 --name bravo
```

Type `pair` in bravo, `pair 127.0.0.1:42188` in alpha, `y` in both, `peers`, then `send <short-id> <path>`.

`host` flags: `--data-dir`, `--receive-dir` (default `~/Downloads/PlayDrop`), `--port`, `--name` (default `/etc/hostname`), `--mobile`, `--json` (events as JSON lines), `--bind`, `--no-discovery`.

| stdin command | Effect |
|---|---|
| `peers` | List paired devices |
| `pair` | Open "Add device" for 120 s |
| `pair <ip:port>` | SAS pairing with that device |
| `pair --qr` / `pair --scan <uri>` | QR pairing, display or scan side |
| `list` | Devices seen in "Add device" |
| `y` / `n` | Answer the pairing prompt |
| `send <peer> <path>...` | Send files or folders |
| `accept <id>` / `decline <id>` | Answer an offer |
| `grants <peer> [+g\|-g]...` | Show or change `files`, `clipboardIn`, `clipboardOut`, `input`, `media`, `find` |
| `ping <peer> [message]`, `clip <peer> <text>` | Ping, push clipboard |
| `unpair <peer>`, `help`, `quit` | |

**Data dir layout** (all 0700 dirs, 0600 files on Unix; start fails on looser modes):

```
<data_dir>/identity.crt  identity.key  pins.json
<data_dir>/incoming/<transfer id>.json      owner Device ID of each unfinished incoming transfer
<receive_dir>/.playdrop-partial/<id>/       transfer.json, <fid>.part, <fid>.json, <fid>.hashes, bundle.json, bundle.hashes
```

**Logs:** CLI uses `RUST_LOG` (default `playdrop_core=warn`; `RUST_LOG=playdrop_core=debug` for detail). Tests use `PD_LOG=1`. Device IDs are logged truncated to 8 characters (SR-32).

---

## 6. Decisions made during implementation

Items marked 11.1 are already in protocol.md section 11.1.

| Decision | Why |
|---|---|
| HTTP/2 kept for bulk despite ~27% loopback gap to raw framing (protocol.md 10 says over 10% moves bulk to `playdrop/2`) | Loopback is CPU-bound and unrepresentative; `playdrop/2` stays reserved until Phase 3 phone CPU numbers decide (CLAUDE.md) |
| Two connections per pair; only the dialler sends requests (11.1) | `h2` lets only the client open streams |
| Session stream on the lower-ID dial; a second session gets `409`, its connection stays for requests (11.1) | Deterministic placement without closing a request-carrying connection |
| Ending a session stream does not close its connection (11.1) | The connection may still carry transfers |
| No `duplicate_connection` GOAWAY debug data (11.1) | `h2` cannot attach it |
| Equal keep-alive intervals: the dialler pings (11.1) | Tie-break 3.4 leaves open |
| Staging folder `<receive_dir>/.playdrop-partial/<id>/`; manifests may not start with that name | Same volume as the destination, so finalize is a rename |
| Durable record per stream: JSON state plus append-only 32-byte hash file, synced every 16 MiB or 1 s | Cheap appends; restart truncates `.part` to the last durable verified offset |
| `.part` sized with `set_len` (sparse), not `fallocate` | Portable first; TODO for real reservation |
| Sender does not persist resume state (11.1) | Scope; a restarted sender re-offers |
| mDNS cannot expose packet source IP, so resolved addresses are filtered on-link instead | `mdns-sd` 0.21 limitation; TLS pin check plus 5 s dial limit cap a forged pointer |
| Discovery `Verified` uses the peer's stored port, not the SRV/beacon port | Payload never chooses the dial target (SR-1) |
| `PinStore::remove` rotates the local announce key in the same step | Rotation on unpair cannot be forgotten (SR-5) |
| `tls::build_client_config_unpaired` exists for "Add device" and QR dialling | Key unknown before pairing; never used for pinned peers |
| Cipher order by CPU AES support; servers honour client order | protocol.md 10 |
| Socket buffers left to kernel autotuning; only `TCP_NODELAY` set | Setting `SO_RCVBUF` on Linux disables autotuning |
| Gate B floor 0.5 in debug builds, 0.7 in release | Debug builds distort small-file cost |
| Last-known dial retried every 5 s for every unreached pin | Equals the SR-1 dial limit; finds a restarted peer within ~5 s without multicast |
| Sender keeps reconnecting for 10 min after a drop; offers expire after 10 min | Covers Wi-Fi switches and daemon restarts |
| Mobile keep-alive profile also sets `BeaconSchedule::DiscoveryWindowsOnly` | No permanent multicast on phones (anti-pattern 10) |
| `PUT` on non-accepted offer: pending `409`, declined `403`, expired/cancelled `410`; no `expired` frame (11.1) | Spec gap |
| `RemovedByPeer` reported, pin kept (11.1) | Not stored yet |
| `Secret<T>` wrapper on SAS and clipboard in events | SR-32 by construction |
| SR-18 uses `symlink_metadata` checks plus `create_new`, not `openat2` | `std` only for now; race window remains (TODO) |
| `InterfaceSource` and `FreeSpace` are traits | No `unsafe` in core; hosts supply platform data (PLAN.md: no platform paths in core) |
| SAS uses BLAKE3 over sorted IDs and full SPKIs (section 11) | Fixed before the threat model; equal strength |

---

## 7. Open TODOs

Every `TODO` in `crates/` (12 total). `grep -rn "TODO(SR-" crates/` finds only 8; also grep plain `TODO`.

| File:line | Cites | What | Phase |
|---|---|---|---|
| `playdrop-core/src/host/pairing.rs:56` | SR-6 | Refuse "Add device" while desktop locked | 2 |
| `playdrop-core/src/host/platform.rs:41` | SR-20 | `GetDiskFreeSpaceExW` on Windows; today reports 1 PiB free | 2 |
| `playdrop-core/src/net/server.rs:104` | SR-31 | `SO_EXCLUSIVEADDRUSE` on Windows via `windows-sys` | 2 |
| `playdrop-core/src/host/mod.rs:634` | SR-24 | Clipboard text over 32 KiB as a blob | 2 (desktop), 3 |
| `playdrop-core/src/host/router.rs:103` | protocol.md 5.15 | Blobs endpoint | 2/3 |
| `playdrop-core/src/host/session.rs:528` | 5.4, SR-8 | Persist "removed by peer" in the pin | 2 (UI shows it) |
| `playdrop-core/src/transfer/fsx.rs:11` | SR-18 | `openat2(RESOLVE_BENEATH \| RESOLVE_NO_SYMLINKS)` | SR-18 is P1: overdue, do in 2 |
| `playdrop-core/src/transfer/receiver.rs:547` | SR-20, 10 | `fallocate` real blocks on Linux | 2 |
| `playdrop-core/src/net/mod.rs:92` | SR-15, 10 | Raise socket buffers only where kernel max is lower | 3 (measure on phones) |
| `playdrop-core/src/host/send.rs:50` | SR-21, 7.6 | Persist sender state across sender restart | 3 (phones get killed) |
| `playdrop-core/src/host/session.rs:475` | Phase 3/5 | Act on media, input, `outbox_ready` | 3 (media, outbox), 5 (input) |
| `playdrop-core/src/host/router.rs:93` | 7.8 | Outbox endpoints | 3/4 |

Untagged Windows gaps found in code: `secure_fs.rs` skips mode checks on non-Unix (SR-28 Windows DPAPI, P2); `manifest.rs` `file_id` returns `None` on non-Unix (SR-21 file ID); no `Zone.Identifier` writing (SR-23, P2).

---

## 8. Known gaps and risks

- Outbox: `GET /v1/outbox` returns `{"items": []}`; every other outbox path is `404`. Blobs always `404`.
- Media, input and `outbox_ready` frames raise `HostEvent::Unsupported`; `caps.in` does not advertise them.
- An incoming clipboard image yields `Clipboard { text: None }` that cannot be fetched.
- Incoming clipboard `hash` is stored for echo prevention without checking it against the text.
- "Removed by peer" is not persisted; after restart the prompt is lost until the next hello.
- No integration test drives over 100 stream resets per second through a live connection (only the `ResetRateCounter` unit test), and none for the 2 s liveness ping that decides a duplicate connection (`DUPLICATE_PING_DEADLINE`, `host/router.rs`).
- No fuzz targets (SR-33) and no SR-32 canary log grep yet.
- Windows: never compiled in CI or run. Paths, permissions, free space, exclusive bind, file IDs untested.
- The project lives on a non-Unix filesystem (`/media/calliope/New Volume`); `secure_fs` refuses its loose modes. Data and receive dirs for manual runs must be on ext4, e.g. `~/.local/share/playdrop` or `/tmp`.
- iperf3 was compared on loopback only, which is not a real-link baseline. The "50% of iperf3" v1 floor is still unmeasured on Wi-Fi.
- The event channel drops events when full; a stalled UI loses progress and completion events silently.
- The 10 s diagnostic runs once per start; a peer that drops later produces no diagnostic.
- Mobile hosts would retry last-known dial every 5 s for every pin; revisit for battery in Phase 3.

---

## 9. Phase 2 kickoff brief

PLAN.md "Build phases", row 2:

> | 2. Desktop daemon | `playdropd` on Linux and Windows; tray; notifications; autostart; receive folder; drag and drop; diagnostics screen naming the failed discovery path; IPC for the UI; Slint window; Windows installer + firewall rule; automatic clipboard push (COSMIC data-control, Windows listener); PC to PC transfers | Pop!_OS 24.04 COSMIC receives with the window closed; Windows 11 receives while minimised; Ubuntu GNOME works through the window and a notification; copying on the desktop pushes to the phone within 1 s; a closed ufw/firewalld/Windows port shows "port X blocked", not an empty list |

**Step 0 (serial, one agent, before the rest):** close gaps 1 to 6 in section 3.6 plus the SR-18 `openat2` TODO, add every new dependency to the relevant `Cargo.toml` (`ksni`, `notify-rust`, `wl-clipboard-rs`, `zbus`, `slint`, `windows-sys`/`windows`), and create empty `crates/playdropd/` and `apps/desktop/` skeletons in the workspace. Parallel agents then never touch `Cargo.toml`.

| Task | Builds | Phase 1 APIs used | SRs |
|---|---|---|---|
| a. Daemon | `crates/playdropd/`: wraps `Host::start`/`shutdown`, single instance, drains `HostEvent`s. Linux: XDG autostart `.desktop`, D-Bus activation service file, optional systemd user unit. Windows: Run key. Tray via `ksni` (SNI) and `Shell_NotifyIcon`. Notifications via `notify-rust` (freedesktop) and Windows toasts: paired, received (Open / Show in folder), failed, ping, find | `HostConfig::new`, `Host::start`, `shutdown`, `HostEvent`, `identity::default_dir`, `Error::PortHeld` | SR-8 (pair notification), SR-23 (received files 0600, Zone.Identifier, Open on executables opens folder), SR-28 (Windows DPAPI, `%LOCALAPPDATA%\PlayDrop`), SR-31 (exclusive bind, "port held" message) |
| b. Local IPC | D-Bus interface on Linux (`zbus`), named pipe on Windows; exposes every `Host` method in 3.3 and streams `HostEvent` as JSON; multiple UI clients | All of 3.3, `HostEvent` serde | SR-26 (caller UID check, reject other Flatpak apps, files as fds), SR-27 (pipe per user SID, first-instance, reject remote, DACL), SR-32 (never log `Secret`) |
| c. Clipboard watcher | COSMIC: `ext-data-control-v1` via `wl-clipboard-rs`. Windows: `AddClipboardFormatListener`. Push to every online peer with `clipboardOut`; apply incoming `HostEvent::Clipboard` with history and cloud upload off when `sensitive`. GNOME: no watcher, in-window button | `send_clipboard` (or the new broadcast), `HostEvent::Clipboard`, `peers`, `grants` | SR-24 (skip `x-kde-passwordManagerHint=secret`, Windows exclusion formats; manual send of such a clip asks and sets `sensitive`), SR-14, protocol.md 5.7 echo rule. Target: desktop copy reaches phone within 1 s |
| d. Slint window | `apps/desktop/`: device list (label, Short ID, online, battery, paired date, last seen), "Add device" with list, SAS prompt (no default button), QR display (`WDA_EXCLUDEFROMCAPTURE` on Windows), transfer progress with cancel, drag and drop to send, grants toggles, unpair, receive folder setting, diagnostics screen naming the failed path. Never owns the socket: talks only over task b | Through IPC: `add_device_list`, `pair_with`, `pairing_respond`, `show_qr`, `send`, `set_grants`, `set_label`, `unpair`, new `diagnose`, `DiscoveryDiagnostic` | SR-6 (window refused while locked), SR-8 (stripped names, Short ID beside name), SR-9 (QR capture-protected, one tap before pin), SR-14 (grants only from local UI) |
| e. Packaging | `packaging/deb/`, `packaging/flatpak/` manifest with `--share=network`, `--socket=wayland`, `--device=dri`, `--talk-name=org.kde.StatusNotifierWatcher`, `--talk-name=org.freedesktop.Notifications`, portals for files, never `--socket=session-bus`. `packaging/windows/` classic installer adding a program-scoped firewall rule for TCP and UDP 42187 | None directly | SR-31 (program-scoped rule), anti-patterns 12 and 16 |
| f. Phase 2 tests | Scripts or tests for each "done when": COSMIC receives with window closed; Windows 11 receives minimised; GNOME through window plus notification; clipboard within 1 s; closed ufw/firewalld/Windows port shows "port 42187 blocked". Plus IPC refusal tests for SR-26/SR-27 and the SR-24 KeePassXC/Bitwarden test | Daemon binary, IPC, `DiscoveryDiagnostic` / `diagnose` | SR-24, SR-26, SR-27, SR-31 tests as written in threat-model.md |

Tasks a and b go first in parallel; c, d, e follow once the IPC surface is fixed; f runs last against real machines. Every PR names the SRs it satisfies.

---

## 10. Rules carried forward

- Hyphen only. Never the long dash, in code, comments, docs or commits.
- No AI attribution anywhere: no co-author trailers, no "generated with" lines, no session links.
- Agents do not commit or push. The user commits and runs `git push`; agents hand over the command, a PR title and a short bullet description.
- Parallel agents never edit any `Cargo.toml`. Dependencies are added up front in a serial step.
- Never bind or announce on 1714-1764, 53317 or 224.0.0.167.
- Every phase ends by writing `docs/phases/phase-N-<name>.md` in this same shape: status, repository map, public API, key types, how it runs, decisions, TODOs, gaps, next-phase brief, rules.

# playdrop-core public API, frozen for v1 bindings

Status: **frozen on 2026-10-09** at the start of Phase 3 (PLAN.md anti-pattern 20: freeze
before generating UniFFI bindings). `crates/playdrop-ffi` binds exactly this surface. A
change here is a change to every host (daemon, Android service, iOS app) and needs a note in
the current phase handoff and a version bump of `playdrop-ffi`.

Everything below lives in `playdrop_core::host` unless a path says otherwise. Phase 1 and 2
handoffs describe the behaviour; this file lists the shape. Items marked **new** are added by
the Phase 3 freeze step and did not exist at the end of Phase 2.

Rules that hold for the whole surface:

- No filesystem path reaches the core from a mobile host except its private data and staging
  directories (anti-pattern 20). Files to send arrive as `SendSource`; received files leave
  through `ReceiveSink`.
- Every type that crosses to a UI is `Clone + Debug + Serialize + Deserialize`; `Secret<T>`
  redacts `Debug` (SR-32).
- Peers are named by `DeviceId` in Rust and by hex Device ID strings in events.
- Methods marked `async` complete when the action is done, not when it is started, unless
  the doc says they return a handle.

---

## 1. Configuration

```rust
pub struct HostConfig {
    pub data_dir: PathBuf,                       // identity, pins, transfer and outbox index; 0700
    pub receive: ReceiveTarget,                  // new: where received files go (section 2)
    pub port: u16,                               // 42187; 0 = any free port, tests only
    pub device_name: String,                     // sanitised (SR-8)
    pub device_type: DeviceType,                 // Desktop, Laptop, Phone, Tablet, Tv
    pub keep_alive_profile: KeepAliveProfile,    // Desktop (60 s) or Mobile (300 s, beacon only in windows)
    pub interfaces: Box<dyn InterfaceSource>,    // IPv4 interfaces for discovery
    pub bind_ip: IpAddr,                         // default `::`
    pub discovery: bool,                         // mDNS + beacon
    pub lock_state: Box<dyn LockState>,          // SR-6; default AlwaysUnlocked
    pub free_space: Arc<dyn FreeSpace>,          // new: default platform::VolumeSpace (statvfs)
    pub secret_store: Option<Arc<dyn SecretStore>>, // new: seals identity.key and pins.json at rest (SR-28)
}

impl HostConfig {
    pub fn new(data_dir, receive_dir, device_name) -> Self;          // desktop defaults, ReceiveTarget::Directory
    pub fn with_sink(data_dir, staging_dir, sink: Arc<dyn ReceiveSink>, device_name) -> Self;  // new: mobile defaults
}
```

`with_sink` sets `device_type: Phone`, `keep_alive_profile: Mobile`, discovery on, and
`ReceiveTarget::Staged { staging_dir, sink }`.

## 2. Receiving without paths (new)

```rust
pub enum ReceiveTarget {
    /// Desktop: stage under `<dir>/.playdrop-partial/<id>/`, finalize by rename into `<dir>`.
    Directory(PathBuf),
    /// Mobile: stage and verify under `staging_dir` (a private directory the host owns),
    /// then hand each verified file to `sink`.
    Staged { staging_dir: PathBuf, sink: Arc<dyn ReceiveSink> },
}

pub trait ReceiveSink: Send + Sync + fmt::Debug {
    /// Called once per verified file (and once per empty file), after the root hash check,
    /// from a blocking thread. `file` is the verified staging file, open read-only at
    /// offset 0. The sink copies or moves it to its final place and returns where it went
    /// (an Android `content://` URI, a path, or any string the UI can open). The core
    /// deletes the staging file afterwards whatever the sink did.
    fn deliver(&self, item: DeliverItem, file: std::fs::File) -> Result<String, SinkError>;
    /// Called once per empty directory in the manifest (`dirs`).
    fn make_dir(&self, path: Vec<String>) -> Result<(), SinkError>;
}

pub struct DeliverItem {
    pub transfer_id: String,
    pub peer: String,            // hex Device ID
    pub path: Vec<String>,       // manifest components, SR-17 validated
    pub size: u64,
    pub mtime_ms: i64,
    pub mime: Option<String>,
}

pub struct SinkError { pub message: String }   // the transfer fails with this text (SR-22 wording)
```

Free space in `Staged` mode is checked on `staging_dir` (same volume on Android).

`TransferComplete` gains `delivered: Vec<DeliveredFile { path: String, location: String }>`
in both modes (location = final path in `Directory` mode, the sink's string in `Staged`
mode). `saved` and `folder` keep their Phase 2 meaning and stay empty in `Staged` mode.

## 2a. Secrets at rest (new)

```rust
pub trait SecretStore: Send + Sync + fmt::Debug {
    /// Encrypts bytes before they are written to the data dir.
    fn seal(&self, plain: Vec<u8>) -> Result<Vec<u8>, SecretStoreError>;
    /// Decrypts bytes read from the data dir.
    fn open(&self, sealed: Vec<u8>) -> Result<Vec<u8>, SecretStoreError>;
}
pub struct SecretStoreError { pub message: String }
```

With a store configured, `identity.key` and `pins.json` are written as
`b"PDSEALED1" || sealed` and read back through `open`; files without the header are read
as plain text once and rewritten sealed on the next save (migration), and a sealed file on a
host without a store fails start with `Error::Identity`. Android supplies an AES-256-GCM
Keystore key (StrongBox when present, no user authentication so screen-off receive works,
SR-28); desktops keep 0600 files and pass `None`.

## 3. Sending without paths (new)

```rust
pub enum SendSource {
    Path(PathBuf),                               // file or folder, walked as before
    Stream(StreamSource),                        // one file behind a reopenable handle
}

pub struct StreamSource {
    pub name: String,                            // one path component (SR-17); the manifest path is [name]
    pub size: u64,
    pub mtime_ms: i64,                           // 0 = unknown; then mtime is not part of the SR-21 check
    pub mime: Option<String>,
    pub opener: Arc<dyn SourceOpener>,
}

pub trait SourceOpener: Send + Sync + fmt::Debug {
    /// Opens the source read-only at offset 0. Called at offer time (metadata), once per
    /// `PUT` (seek to the resume offset) and once per bundle member. Must return a
    /// seekable regular file; a pipe fails the transfer at the first resume.
    fn open(&self) -> std::io::Result<std::fs::File>;
}
```

`Host::send(peer, Vec<PathBuf>)` stays as a wrapper over `send_sources`. The resume check
(SR-21) for a stream compares `fstat` size always, mtime when the recorded value is not 0,
and `(dev, ino)` when the platform exposes them.

## 4. `Host` methods

Unchanged from Phase 2 unless marked.

| Signature | Notes |
|---|---|
| `async fn start(config: HostConfig) -> Result<(Host, mpsc::Receiver<HostEvent>)>` | |
| `async fn shutdown(&self)` | idempotent |
| `fn device_id(&self) -> DeviceId`, `fn short_id(&self) -> String`, `fn local_addr(&self) -> SocketAddr` | |
| `fn peers(&self) -> Vec<PeerSummary>` | `PeerSummary` gains `removed_by_peer: bool` (**new**, persisted in the pin) |
| `fn resolve_peer(&self, query: &str) -> Option<DeviceId>` | |
| `fn is_online(&self, peer) -> bool`, `async fn connect(&self, peer) -> Result<()>` | |
| `fn grants(&self, peer) -> Option<Grants>`, `async fn set_grants(&self, peer, Grants) -> Result<()>`, `async fn set_label(&self, peer, String) -> Result<()>` | |
| `async fn unpair(&self, peer) -> Result<()>` | |
| `fn open_add_device_window(&self) -> Result<()>`, `fn close_add_device_window(&self)`, `fn add_device_window_open(&self) -> bool` | |
| `fn add_device_list(&self) -> Vec<AddDeviceCandidate>` | one entry per address; the UI may collapse by name |
| `async fn pair_with(&self, addr: SocketAddr) -> Result<()>` | returns once the SAS exchange has started; the result arrives as events |
| `fn pairing_respond(&self, accept: bool)` | |
| `fn show_qr(&self) -> Result<String>`, `async fn scan_qr(&self, uri: &str) -> Result<()>` | |
| `async fn send(&self, peer, Vec<PathBuf>) -> Result<TransferHandle>` | wrapper |
| `async fn send_sources(&self, peer, Vec<SendSource>, note: Option<String>) -> Result<TransferHandle>` | **new** |
| `async fn cancel_transfer(&self, id: &str) -> Result<()>`, `fn transfers(&self) -> Vec<TransferSnapshot>`, `async fn respond_offer(&self, id, accept) -> Result<()>` | |
| `fn ping(&self, peer, message: Option<&str>) -> Result<()>` | |
| `fn find(&self, peer, start: bool) -> Result<()>` | **new**; `Error::NotFound` without a session |
| `fn send_clipboard(&self, peer, text, sensitive) -> Result<bool>` | text over 32 KiB and up to 1 MiB now goes as a blob (**changed**) |
| `fn push_clipboard(&self, text, sensitive) -> Result<ClipboardPush>` | same blob rule |
| `fn push_clipboard_image(&self, bytes: Vec<u8>, mime: &str, sensitive: bool) -> Result<ClipboardPush>` | **new**; blob, `clipboard.image` capability required on the peer; at most 16 MiB |
| `fn set_battery(&self, level: u8, charging: bool, low: bool)` | **new**; remembered, pushed to every online peer now and at each session start, at most once per 60 s unless `charging` or `low` changed (5.8) |
| `fn set_presence(&self, state: PresenceState, reason: Option<String>, expect_back_ms: Option<u64>)` | **new**; remembered and pushed to every online peer (5.10) |
| `fn send_media_command(&self, peer, player: &str, action: MediaAction, value: Option<u64>) -> Result<()>` | **new**; needs the peer's `media.control` capability |
| `fn push_media_state(&self, state: MediaState)` | **new**; coalesced to 2 frames per second per player (5.9) |
| `async fn queue_outbox(&self, peer, Vec<SendSource>, note: Option<String>) -> Result<String>` | **new**; returns the item ID; sends `outbox_ready` if the peer is online (7.8) |
| `fn outbox(&self) -> Vec<OutboxItem>` | **new**; items this host holds |
| `async fn remove_outbox(&self, id: &str) -> Result<()>` | **new** |
| `async fn pull_outbox(&self, peer) -> Result<Vec<String>>` | **new**; pulls every item the peer holds for us, returns the transfer IDs started; progress and completion arrive as transfer events |
| `fn diagnose(&self) -> Diagnostics`, `fn network_changed(&self)` | |

`TransferHandle` is unchanged (`id`, `total_bytes`, `bytes_sent`, `cancel`, `wait`).

New plain types:

```rust
pub struct MediaState { pub player: String, pub name: Option<String>, pub playing: bool,
    pub title: Option<String>, pub artist: Option<String>, pub album: Option<String>,
    pub duration_ms: Option<u64>, pub position_ms: Option<u64>, pub position_at_ms: Option<u64>,
    pub volume: Option<u8>, pub can: Vec<MediaAction> }
pub struct OutboxItem { pub id: String, pub peer: String, pub file_count: usize,
    pub total_bytes: u64, pub note: Option<String>, pub created_at_ms: u64 }
```

`session::types::{MediaAction, PresenceState, DeviceType}` and `pin_store::Grants` are part
of the frozen surface as they are.

## 5. `HostEvent` (serde tag `event`, snake_case, `#[non_exhaustive]`)

Phase 2 variants are unchanged except where noted. New variants:

| Variant | Fields |
|---|---|
| `TransferComplete` | gains `delivered: Vec<DeliveredFile>` (section 2) |
| `ClipboardImage` | `peer`, `mime`, `data: Secret<Base64Bytes>`, `sensitive` (**new**; `Base64Bytes` serialises as a base64 string) |
| `MediaState` | `peer`, `state: MediaState` (**new**) |
| `MediaCommand` | `peer`, `player`, `action: MediaAction`, `value: Option<u64>` (**new**; only with the peer's `media` grant) |
| `OutboxReady` | `peer`, `count`, `bytes`, `ids` (**new**; replaces `Unsupported` for `outbox_ready`) |
| `AddDeviceSeen` | now emitted once per address per "Add device" window, again only if name or type changed (**changed**) |
| `Clipboard` | `text` is now always `Some` for text clips, long ones fetched from the blob first; image clips arrive as `ClipboardImage` (**changed**) |
| `RemovedByPeer` | now also persisted: `PeerSummary.removed_by_peer` is true until the next successful pairing or unpair (**changed**) |

`Unsupported` remains for `input` only.

## 6. Capabilities advertised by v1

`in` and `out`: `ping`, `find`, `clipboard`, `clipboard.image`, `battery`, `media.state`,
`media.control`, `presence`, `transfer`, `transfer.bundle`, `outbox`, `blobs`. `input.*` is
Phase 5. The `capabilities` frame is parsed but never sent in v1: per-peer toggles are grants.

## 7. Traits a host may supply

| Trait | Default | Mobile |
|---|---|---|
| `discovery::InterfaceSource` | `platform::SystemInterfaces` (getifaddrs) | Android may pass its own list (`NetworkInterface.getNetworkInterfaces()`) |
| `host::LockState` | `AlwaysUnlocked` | unused on phones (pairing needs the app open) |
| `transfer::FreeSpace` | `platform::VolumeSpace` | default works on the staging volume |
| `host::ReceiveSink` | none in `Directory` mode | required |
| `host::SourceOpener` | none for `Path` sources | one per shared `content://` item |
| `host::SecretStore` | none (0600 files, SR-28 desktop rule) | Android Keystore AES-256-GCM wrapper |

All are synchronous and `Send + Sync`; none is async (UniFFI issue #2576 is avoided).

## 8. Deliberately not in v1

- Sender-side resume across a sender restart (protocol.md 7.6 "sender persistence"). Share
  intent URI grants die with the process, so a restarted phone could not reopen them anyway;
  a restarted sender re-offers.
- `Host::set_capabilities`. Grants already give per-peer toggles.

## 9. Phase 5 additions (freeze break, `playdrop-ffi` 0.2.0, 2026-10-12)

Remote input (protocol.md 5.12, SR-25) and the pieces a UI needs to hide a feature the
peer cannot do (PLAN.md anti-pattern 18). Everything else above is unchanged.

```rust
// HostConfig
pub accepts_input: bool,     // default false; advertise `input.*` in caps.in (a desktop with a backend)

// PeerSummary
pub capabilities: Vec<String>,   // the peer's caps.in while online, empty offline

// Host
fn send_input(&self, peer, Input) -> Result<()>;     // NotFound without a session; Forbidden when the
                                                      // peer lacks the matching input.* capability; moves
                                                      // and scrolls coalesced to one frame per 8 ms
fn input_sessions(&self) -> Vec<DeviceId>;           // peers with an open input session on this host
fn stop_input_sessions(&self) -> Vec<DeviceId>;      // SR-25 kill switch; peers are told
```

`session::types::Input` (with `PointerInput`, `KeyInput`, `SessionOp`, `PointerButton`,
`ButtonAction`, `ScrollUnit`, `KeyAction`, `Modifier`, `PresenterAction`) joins the frozen
surface as it is. The FFI flattens it to `InputEvent` (`Session`, `PointerMove`,
`PointerButton`, `Scroll`, `Key`, `Text`, `Presenter`).

New `HostEvent` variants:

| Variant | Fields |
|---|---|
| `Input` | `peer`, `input: Input`. Only on a host started with `accepts_input`, only from a peer with the `input` grant, inside a session it opened, while the host is unlocked. Keys over 60 per second are dropped. `Session { Stop }` is always the last event of a session: peer stop, connection drop, 10 idle minutes, lock, or `stop_input_sessions`. The host's backend releases every key and button on it |
| `PeerRefused` | `peer`, `frame_type: Option<String>`, `code`, `message`. The peer answered one of our frames with an `error` (usually `forbidden`: no grant on its side). The core remembers the types of the last 256 frames it sent per connection to fill `frame_type` |

| `InputSessionEnded` | `peer`, `reason`. The target of a session we control ended it (lock, 10 idle minutes, kill switch): the controlling UI leaves its input screen and shows the reason |

`Unsupported` is no longer emitted (kept for older IPC clients).

Capabilities (section 6): `caps.out` now always lists `input.pointer`, `input.key` and
`input.presenter`; `caps.in` lists them only with `accepts_input`.

Nearby discovery: while "Add device" is open and no candidate has been seen, the host sends
a fresh beacon burst every 10 s (Phase 3 handoff section 7).

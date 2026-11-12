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
    /// Called once per verified file (and once per empty file), from a blocking thread:
    /// for a streamed file after the root hash check; for a bundle member as soon as the
    /// member's bytes are chunk-verified and durable, before the bundle root arrives (so
    /// while the bundle still streams, from the core's own delivery threads; Phase 8,
    /// section 11). Calls overlap: streamed files and bundle members each go out several
    /// at a time. After a process kill the files in flight (for a bundle at most four)
    /// may be delivered a second time, and a sink that keeps every copy then shows them twice
    /// (Android: `a.txt` and `a (1).txt` in Downloads). `file` is the verified staging file, open read-only at offset 0. The sink copies or moves it to its final place and returns where it went
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
`media.control`, `presence`, `transfer`, `transfer.bundle`, `outbox`, `blobs`, `notifications`
(section 15). `input.*` is
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

## 10. Phase 7 note (content change, `playdrop-ffi` 0.2.1, 2026-10-14)

No type changed. `Diagnostics.device_id` and `PeerDiagnostic.peer` now carry the first 8 hex
characters of the Device ID instead of all 64, the same truncation every log line uses
(SR-32: the diagnostics report is meant to be pasted into bug reports). `short_id` and
`label` are unchanged and are what every screen matches on; the SR-32 canary test
(`crates/playdrop-core/tests/sr32_canary.rs`) checks the report for full IDs. A receiver
also logs one line at DEBUG when it places a verified file (file names are allowed at
DEBUG and below only).

## 11. Phase 8 note (`playdrop-ffi` 0.3.0, 2026-10-15)

Additive type change and two behaviour changes.

- `HostEvent::TransferProgress` and `TransferSnapshot` gain `files_done` (unsigned, files
  finished so far; `#[serde(default)]`, so an older host reads as 0). Send side: a streamed
  file counts once its `PUT` came back `verified` or the receiver skipped it; every bundled
  file counts once the bundle `PUT` came back `verified`; a zero-length file counts only when
  the transfer completes. After a reconnect the count is rebuilt from the receiver's
  `GET /v1/transfers/{id}`; within one connection it never goes down. A successful send
  always ends with one reading where `bytes == total` and `files_done == file_count`, just
  before `TransferComplete`. Receive side: streamed files verified or skipped, bundle members
  placed (live, from the delivery thread), zero-length files once created. On
  `TransferSnapshot` it is the same count as the last progress event, and `file_count` once
  `complete`.
- A send is `offered` from the moment it starts until its first file or bundle `PUT` starts
  moving bytes, including while the receiver decides. It is `active` from then on and stays
  `active` across reconnects. No `TransferProgress` is emitted for a send while it is
  `offered`; a declined or expired offer goes to `cancelled` or `failed` with no progress
  event. A send with nothing to stream passes through `active` on its final reading, so the
  order is always offered, active, complete. A progress reading never changes the state.
- A finished snapshot stays in `transfers()` for at least 60 s (`KEEP_AT_LEAST`), then until
  it is older than 10 minutes or more than 64 newer finished ones exist (never more than
  1,024 in all).
- `ReceiveSink::deliver` is called for a bundle member before the bundle root arrives
  (section 2, protocol.md 11.1). The trait is unchanged.

## 12. Phase 9 note (`playdrop-ffi` 0.4.0, behaviour change and one additive field)

`HostConfig` gains `reconnect_for: Duration`, set by `new` and `with_sink` to the old fixed
10 minutes: how long a send (and an outbox pull) keeps trying to reach the other device
after a drop before it is reported failed. Hosts built through `new` or `with_sink` need no
change; tests shorten it. `TransferSnapshot` gains `resumable: bool` (`#[serde(default)]`,
so an older host reads as false): true on a `failed` send whose record is kept for resume,
false once it is cancelled, resumed or complete. This lifts the section 8 exclusion of
sender-side resume.

Every send keeps a record under `<data_dir>/outgoing/` (`<id>.json`, `<id>.state` and an
append-only `<id>.hashes`, all 0600; protocol.md 7.6 "sender persistence" and 11.1), deleted when the send completes, is cancelled or declined,
fails for good, or its source changes size, and after 24 h. A send that gives up because
the receiver is unreachable, or keeps dropping the upload, still ends `failed` with
`TransferFailed`, but keeps its record, sets `resumable`, and its `note` (and the event's
`reason`) says how it resumes: "receiver unreachable; resumes when <label> is back or when
these files are sent to it again" when the host restarts it by itself (path sources, an
unreachable receiver, at most three automatic resumes per record), otherwise "...; resumes
when these files are sent to <label> again". It resumes under the same transfer ID: by
itself as just said, by itself at the next host start when the host stopped mid-send
(path sources), and on every platform when `send_sources` is called with the same items
for the same peer within 24 h (the returned `TransferHandle::id()` is the earlier ID;
stream openers are attached afresh, which is how a phone resumes a share). If the receiver
had meanwhile ended that transfer (cancelled, declined, expired, failed), such a send
continues as a new transfer under a new transfer ID on the wire, but this device keeps
knowing it by the earlier ID: the handle, the events, `transfers()`, `TransferSummary.id`
and `cancel_transfer` all use it, also after a restart; a send the host restarted by
itself just ends then. A
resumed send is listed once in `transfers()` and goes `offered`, `active`, `complete`
again. A waiting send stays in `transfers()` (`failed`, `resumable`) for as long as its
record exists, also after a restart of this host;
`TransferHandle::bytes_sent` and the progress readings now start each connected round at
what the receiver holds and keeps, so they never pass the total and count no byte twice.
`cancel_transfer` on a waiting ID deletes its record, shows it `cancelled`, returns
`Ok(())`, and tells the receiver in the background: at once when it is online, else at its
next session (the cancel is kept on disk for up to 24 h); unpairing deletes a peer's
records and such pending cancels. A receiving host now answers for a transfer that ended (any end) for
24 h, also after a restart, so a sender that missed the end completes instead of sending
the files again. No event or status was added.

The `playdrop-ffi` mirror of `TransferSnapshot` carries `resumable` (bool) too, and the ffi
version is 0.4.0 (an additive type change, like `files_done` in section 11). Hosts that
build a `TransferSnapshot` by hand (test fixtures) must pass `resumable = false`.

## 13. Phase 10 note (Rust only, no `playdrop-ffi` change)

`HostConfig` gains `listener_setup: Option<Arc<dyn ListenerSetup>>` (default `None`; the
trait and `ListenSocket`, a re-export of `socket2::Socket`, are in `playdrop_core::host`): a
hook called on the listening socket before it is bound, through which a Windows daemon sets
`SO_EXCLUSIVEADDRUSE` (SR-31) because the core forbids `unsafe`. It is Rust only:
`playdrop-ffi` does not expose it and builds its `HostConfig` through
`HostConfig::new`/`with_sink`, so mobile hosts keep `None` and the ffi version does not
change.

## 14. 2026-11-10 note (Rust only, no `playdrop-ffi` change)

Three additive changes for the desktop daemon's IPC (`docs/ipc.md` section 1.7). The ffi
records are built field by field from the Rust types, so they ignore the new fields; the
UniFFI surface and the ffi version (0.4.0) stay as they are, and Android and iOS see none of
this yet.

- `PeerSummary` gained `battery: Option<PeerBattery>` (`level`, `charging`, `low`, the
  peer's last `battery` frame, protocol.md 5.8): set while the peer is online, cleared when
  it goes offline or is unpaired. The ffi `PeerSummary` does not carry it; a mobile UI keeps
  using `HostEvent::Battery`.
- `TransferSnapshot` gained `names: Vec<String>`: the transfer's top-level names in manifest
  order, each once, at most `TRANSFER_NAMES_MAX` (3). On the receive side they come from the
  peer's manifest after its SR-17 check: display text only. With it, `names_more: bool`
  (serde default false, not written when false) is true only when names were left out: the
  core reads one name past the cap to tell. The ffi `TransferSnapshot` carries neither.
- `Host::send_clipboard_image(peer, bytes, mime, sensitive) -> Result<ClipboardPush>`: the
  one-device counterpart of `push_clipboard_image`, with the errors of `send_clipboard`. Not
  bound by the ffi.

## 15. Phase 15 note: notification mirroring (`playdrop-ffi` 0.5.0, additive)

Protocol.md 5.16, SR-41 to SR-44. Every change is additive; nothing existing changed.

- `pin_store::Grants` gains `notifications_in: bool` (default `true`) and
  `notifications_out: bool` (default `false`, never turned on by pairing). Both are
  `#[serde(default)]`, so pin stores written before load with those values. `Grants` no
  longer refuses unknown fields, and each new field is left out of the JSON while at its
  default, so an older reader (a v0.9.0 applet, a downgraded daemon) fails only once the
  user changed one of them.
- `MirroredNotification { key: String, app: String, title: String, text: String, when_ms:
  u64 }` in `playdrop_core::host`, serde as is, `Debug` redacted (SR-43). Also
  `NOTIFICATIONS_POSTED_PER_MINUTE` (30).
- `Host` methods:
  - `send_notification(&self, peer: &DeviceId, n: MirroredNotification) -> Result<()>`:
    `posted`. Needs this host's `notifications_out` grant for the peer (`Forbidden`, nothing
    sent), `NotPaired`, `NotFound` without a session, `Forbidden` when the peer lacks the
    `notifications` capability, `BadRequest` for a key that is not 32 lowercase hex. Cuts
    `app`, `title`, `text` to 64 B, 256 B, 1 KiB at a character boundary.
  - `send_notification_removed(&self, peer: &DeviceId, key: String) -> Result<()>`: needs
    no grant (no content; it clears the peer's copies after `notifications_out` went off,
    SR-42), otherwise the same errors.
  - `send_notification_dismiss(&self, peer: &DeviceId, key: String) -> Result<()>`: desktop
    to phone; needs this host's `notifications_in` for the peer.
  - `notification_key(&self, raw: &str) -> String`: first 128 bits of BLAKE3 keyed by a
    per-process random 32-byte salt over `raw`, lowercase hex. Stable within a process.
- `HostEvent` gains `NotificationPosted { peer, notification: MirroredNotification }`,
  `NotificationRemoved { peer, key: String }` (both only with the receiver's
  `notifications_in`) and `NotificationDismiss { peer, key: String }` (only with the
  receiver's `notifications_out`). Over-limit frames are dropped with `error`
  `bad_request`, malformed keys and `posted` frames past 30 a minute per connection are
  dropped silently.
- Section 6: `notifications` is added to both capability lists.

`playdrop-ffi` 0.5.0 binds the same surface: the `MirroredNotification` record,
`send_notification(peer, notification)`, `send_notification_removed(peer, key)`,
`send_notification_dismiss(peer, key)`, `notification_key(raw)`, the three events, and the
two `Grants` fields with UniFFI defaults (`notificationsIn = true`, `notificationsOut =
false`), so existing Kotlin and Swift constructors keep compiling.

## 16. Phase 16 note (`playdrop-ffi` 0.6.0, additive, Swift pattern changes)

Protocol.md sections 4, 5.7, 5.12, 5.14 and 5.16; `docs/design/update-check.md`. Every
change is additive except that the `InputSessionEnded` and `TransferFailed` events gain a
field (see the end).

**Notification role** (protocol.md 4, 5.16).

- `HostConfig.notifications: NotificationRole` in `playdrop_core::host`, with `Off` (the
  default), `Source` (a phone: sends `posted` and `removed`, handles `dismiss`) and `Sink`
  (a desktop: handles `posted` and `removed`, sends `dismiss`). `Off` leaves
  `notifications` out of both capability lists and drops every `notification` frame; either
  role lists it in both. Each role's send methods return `Forbidden` for the other role's
  ops, and the other role's ops are dropped on arrival without an answer. `playdropd` is a
  sink, `playdrop host` is off. A host's own `caps.out` now decides what it sends (it was a
  fixed list).
- `NOTIFICATIONS_REMOVED_PER_MINUTE` (400) and `NOTIFICATIONS_DISMISS_PER_MINUTE` (60) join
  `NOTIFICATIONS_POSTED_PER_MINUTE` (30); frames past them on one connection are dropped.
- ffi: `HostConfig.notifications: NotificationRole?` with the default `None` (no role) and
  `NotificationRole { Source, Sink }`. Existing Kotlin and Swift constructors keep
  compiling; the Android app passes `Source`, the iPhone app leaves it out. (UniFFI 0.32
  proc-macros cannot give an enum field a default value, hence the optional.)

**Why an input session ended** (protocol.md 5.12).

- `InputEndReason` in `playdrop_core::host`: `GrantOff`, `Locked`, `NotAccepted`,
  `StoppedByUser`, `IdleTimeout`, `ConnectionLost`, `Other` (serde `snake_case`; an
  unknown value reads as `Other`).
- `HostEvent::InputSessionEnded` gains `code: InputEndReason` next to `reason: String`
  (`#[serde(default)]`, so an IPC event from an older daemon reads as `Other`). The
  controller gets exactly one per `start` it sent or that failed locally (`NotFound` gives
  `ConnectionLost`, `Forbidden` for a peer without `input.*` gives `NotAccepted`; both calls
  still return their error), unless it sent `stop` first: from the target's answer, from an
  older target's `forbidden` (`GrantOff`) or known message, when the connection ends, or
  when either side unpairs (`ConnectionLost`, raised before `Unpaired`). An answer whose
  `re` names a frame sent before the current `start` is ignored, so a late refusal of an
  earlier `start` cannot end a newer session.
- Against a target older than 0.11.0 the promise is weaker, because such a target answers
  only its first refusal per connection (one `forbidden` per frame type) and never answers
  while locked. After that `forbidden`, a later `start` on the same connection ends at once
  locally with `GrantOff` and is not sent (the call returns `Ok`), until the connection is
  opened again. A `start` to an older target that is locked gives no event at all: input is
  simply not applied there. There is no positive answer to a `start` on the wire, so no
  timer can tell "accepted" from "ignored".
- The target answers each refused `start` (at most 60 per connection per minute; more are
  dropped unanswered) and turns `Host::set_grants` with `input` off into the end of that
  peer's running session (it gets its final `stop` event). The grant is read again under
  the input lock when a `start` is handled, so a `start` racing the grant change cannot
  leave a session open. Unpairing a peer ends the session it holds here in the same way
  (final `stop`, then `Unpaired`).
- Kotlin and Swift: no change in calls; an app keeps waiting for the one event per `start`
  and treats `Ok` from `send_input` as "sent or ended", not as "accepted".
- ffi: `InputEndReason` enum and `HostEvent.InputSessionEnded { peer, reason, code }`.

**Clipboard to chosen peers** (protocol.md 5.7).

- `Host::push_clipboard_to(&self, peers: &[DeviceId], text: &str, sensitive: bool) ->
  Result<ClipboardPush>` and `Host::push_clipboard_image_to(&self, peers: &[DeviceId],
  bytes: Vec<u8>, mime: &str, sensitive: bool) -> Result<ClipboardPush>`: `push_clipboard`
  and `push_clipboard_image` limited to `peers`, with one echo check and one "last sent" for
  the whole push (calling `send_clipboard` per device skips all but the first as an echo),
  the same per-peer rules and counters, and an empty list sending nothing. The echo check of
  a chosen push is only against the last clip applied from a peer (received clips are never
  forwarded), not the last one sent: the same text sent to one device and then to another
  goes to both. It still becomes "last sent", so the automatic `push_clipboard` does not
  send it again. Size and MIME errors fail the whole call as before.
- `ClipboardPush` gains `skipped_not_paired: Vec<DeviceId>` (serde default): chosen peers
  that are not paired, reported per device; the rest of the push goes on.
- ffi: `push_clipboard_to(peers: [String], text, sensitive)` and
  `push_clipboard_image_to(peers: [String], data, mime, sensitive)` on `PlayDropHost`;
  `ClipboardPush.skipped_not_paired: [String]` (default empty) also lists entries that are
  not a Device ID.

**Why a transfer failed** (protocol.md 7.3, 9).

- `TransferFailure` in `playdrop_core::host`: `NoSpace` (`507 no_space`), `Declined`
  (`403 declined` or a declined decision), `TooLarge` (`413`), `RateLimited` (`429`),
  `CancelledByPeer`, `CancelledByYou`, `Integrity`, `ConnectionLost` (unreachable, or the
  upload kept dropping), `TimedOut` (offer expired, no data for 24 h), `Other` (serde
  `snake_case`; an unknown value reads as `Other`). `TransferFailure::sender_text()` is the
  plain sentence a sender shows; it reads after "Sending to <device> failed: " and carries
  no HTTP status or wire code ("the receiving device does not have enough free space").
  An offer accepted by hand that then finds no space reaches the sender as `NoSpace` too
  (the decision frame's `reason` `no_space`, protocol.md 5.13); a sender older than 0.11.0
  shows it as `Other`. On the receiver, a failed transfer with no message of its own is
  `Integrity` only when a file or the bundle failed its checks, else `Other`.
- `HostEvent::TransferFailed` gains `code: TransferFailure` (`#[serde(default)]`), and
  `TransferSnapshot` gains `failure: Option<TransferFailure>` (set on failed and cancelled
  transfers, `#[serde(default)]`). `reason` and `note` keep the words: on the send side the
  named failures now use `sender_text()` instead of texts such as "offer refused: 507
  Insufficient Storage no_space"; `Other` keeps the receiver's answer as it came.
- ffi: `TransferFailure` enum, `HostEvent.TransferFailed { id, peer, direction, reason,
  code }`, and `TransferSnapshot.failure: TransferFailure?` (default `None`).

**A transfer refused for lack of space, on the receiver** (protocol.md 6.4, SR-20).

- `HostEvent::ReceiveRefusedNoSpace { peer, offered_bytes, free_bytes, reserve_bytes }`
  when this host answers `507 no_space`: at the offer, at a local accept, or on a `PUT`
  that would cross the reserve (`max(2 GiB, 5% of the volume)`). `offered_bytes` is the
  transfer's total. At most one per peer per minute (unpair clears the peer's minute); it
  names no file (SR-32).
- ffi: `HostEvent.ReceiveRefusedNoSpace { peer, offeredBytes, freeBytes, reserveBytes }`.

**Update check** (`docs/design/update-check.md`, SR-45 to SR-48). Free functions over
`playdrop_core::update`, no network, clock or files:

- `update_sums_url()`, `update_sig_url()`, `update_user_agent()` -> `String`;
  `update_max_sums_len()`, `update_max_sig_len()`, `update_max_download_len()`,
  `update_request_timeout_ms()` -> `u64`; `update_max_redirects()` -> `u32`.
- `update_check_due(last_answer_ms: Option<u64>, last_failure_ms: Option<u64>, now_ms: u64)
  -> bool`; `update_redirect_allowed(url: String) -> bool`.
- `update_offer(sums: Vec<u8>, sig: Vec<u8>, current_version: String, kind:
  UpdateInstallKind) -> Result<Option<UpdateOffer>, UpdateCheckError>`. `current_version`
  is the app's own `major.minor.patch`; anything else is `BadCurrentVersion`.
- `update_offer_with_test_key(sums, sig, current_version, kind, test_key_hex: String)`:
  trusts one more key, for debug builds against a stand-in server only. Release app code
  must never call it.
- `update_asset_matches(asset: UpdateAsset, sha256: Vec<u8>) -> bool`: the SHA-256 the app
  computed over a download against the signed one.
- `UpdateInstallKind { Deb, Windows, Android, Flatpak, Ios, Other }`, `UpdateOffer {
  version, page_url, asset: Option<UpdateAsset> }`, `UpdateAsset { name, url, sha256_hex }`,
  and the flat error `UpdateCheckError { TooLarge, BadSignatureEncoding, NotSigned,
  Malformed, NoVersion, BadCurrentVersion, BadTestKey }` whose messages are fit to show.

**Swift source change.** Adding `code` to the `inputSessionEnded` and `transferFailed`
cases changes their associated values, so positional patterns gain a `_`:
`case let .inputSessionEnded(p, reason, _)` in
`apps/ios/PlayDrop/UI/Input/RemoteInputModel.swift`, and `.transferFailed(id, peer,
direction, reason, _)` in `PlayDrop/Core/CoreHostImpl.swift`,
`PlayDrop/Notifications/LocalNotifications.swift` and `PlayDrop/Share/OutgoingDrain.swift`.
Kotlin reads the fields by name and keeps compiling.

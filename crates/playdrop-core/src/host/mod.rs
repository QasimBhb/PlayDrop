//! The runtime a daemon or mobile service embeds: identity, pins, discovery, the HTTP/2
//! server, live connections and sessions, pairing, and both sides of the transfer plane.
//!
//! A host owns the listening socket; UIs are thin clients that call the [`Host`] API and
//! read [`HostEvent`]s (PLAN.md "Architecture"). Wire behaviour follows `docs/protocol.md`;
//! where this module deviates, the "Implementation notes" subsection of protocol.md says so.
//!
//! Module map:
//! - [`registry`]: live connections per peer and where the session stream lives (3.3).
//! - [`session`]: the session stream, frame handlers and grants (5, SR-13, SR-14, SR-16).
//! - [`pairing`]: driving `session::PairingSession` for both roles and methods (6).
//! - [`receive`] and [`send`]: the transfer plane (7).
//! - [`transfers`]: snapshots of live and recent transfers for late UIs.
//! - [`router`]: request routing for the server (7.1).
//! - [`connect`]: last-known dial, discovery wiring and the 10 s report (3.2, 8).
//! - [`diagnostics`]: the on-demand report for the daemon's diagnostics screen.
//! - [`platform`]: free space and interface adapters.
//! - [`blobs`]: clipboard blobs, held and fetched (5.15, SR-24).
//! - [`status`]: battery, presence and media pushed to peers (5.8 to 5.10).
//! - [`outbox`]: items held for a peer that pulls them, and pulling (7.8).
//! - [`lock`](mod@lock): the host-supplied desktop lock state (SR-6).

mod blobs;
mod connect;
pub mod diagnostics;
pub mod events;
mod input;
pub mod lock;
mod outbox;
mod pairing;
mod pins;
pub mod platform;
mod receive;
mod registry;
mod router;
mod send;
mod send_resume;
mod send_store;
mod session;
mod status;
mod transfers;

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use tokio::sync::{mpsc, watch};

pub use crate::identity::sealing::{SecretStore, SecretStoreError};
pub use crate::net::{ListenSocket, ListenerSetup};
pub use crate::transfer::{DeliverItem, ReceiveSink, SinkError, SourceOpener, StreamSource};
pub use diagnostics::{
    Diagnostics, DialFailure, InterfaceInfo, JoinFailure, MdnsStatus, MulticastStatus,
    PeerDiagnostic,
};
pub use events::{
    Base64Bytes, DeliveredFile, DiscoveryPath, FileRoot, HostEvent, OfferedFile, PairingPromptKind,
    Secret, TransferDirection, OFFER_FILES_MAX,
};
pub use lock::{AlwaysUnlocked, LockFlag, LockState};
pub use outbox::OutboxItem;
pub use send::{SendSource, TransferHandle, TransferSummary};
pub use status::MediaState;
pub use transfers::{TransferSnapshot, TransferStatus, KEEP_AT_LEAST, KEEP_FINISHED, KEEP_FOR};

use crate::discovery::{BeaconSchedule, DialLimiter, Discovery, DiscoveryConfig, InterfaceSource};
use crate::identity::{DeviceId, DeviceIdentity};
use crate::net::connection::{short, CloseReason, DESKTOP_KEEPALIVE, MOBILE_KEEPALIVE};
use crate::net::{Server, TlsAcceptor};
use crate::pin_store::{Grants, PinStore, PIN_FILE};
use crate::session::types::Input;
use crate::session::types::{
    Capability, Clipboard, ClipboardKind, DeviceType, Find, FindAction, Frame, MediaAction, Ping,
    PresenceState,
};
use crate::session::{AddDeviceWindow, Limits, PairRequestLimiter, QrOffer};
use crate::tls;
use crate::transfer::{FreeSpace, SinkBinding};
use crate::{Error, Result};

use pins::Pins;
use registry::Registry;

/// Keep-alive profile announced in `hello.keepAlive` (protocol.md 3.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeepAliveProfile {
    /// PING every 60 s.
    #[default]
    Desktop,
    /// PING every 300 s; minutes, not seconds, on mobile (PLAN.md anti-pattern 10).
    Mobile,
}

impl KeepAliveProfile {
    /// The interval.
    pub fn interval(self) -> Duration {
        match self {
            KeepAliveProfile::Desktop => DESKTOP_KEEPALIVE,
            KeepAliveProfile::Mobile => MOBILE_KEEPALIVE,
        }
    }
}

/// Where received files go (`docs/core-api.md` section 2).
#[derive(Debug, Clone)]
pub enum ReceiveTarget {
    /// Desktop: stage under `<dir>/.playdrop-partial/<id>/`, finalize by rename into
    /// `<dir>`; created 0700 (SR-23).
    Directory(PathBuf),
    /// Mobile: stage and verify under `staging_dir` (a private directory the host owns),
    /// then hand each verified file to `sink`.
    Staged {
        /// Private staging directory; free space is measured here.
        staging_dir: PathBuf,
        /// Gets every verified file.
        sink: Arc<dyn ReceiveSink>,
    },
}

impl ReceiveTarget {
    /// The directory transfers are staged (and, for `Directory`, placed) beneath.
    pub fn root(&self) -> &Path {
        match self {
            Self::Directory(d) => d,
            Self::Staged { staging_dir, .. } => staging_dir,
        }
    }
}

/// How to start a [`Host`].
pub struct HostConfig {
    /// Identity, pins, transfer and outbox index; created 0700 (SR-28).
    pub data_dir: PathBuf,
    /// Where received files go.
    pub receive: ReceiveTarget,
    /// TCP port (protocol.md section 1: 42187 unless overridden). 0 picks a free port,
    /// for tests only: last-known dial needs a stable port.
    pub port: u16,
    /// Name shown to peers in `hello` and "Add device" adverts (sanitised, SR-8).
    pub device_name: String,
    /// Form factor for `hello.type`.
    pub device_type: DeviceType,
    /// Keep-alive interval (protocol.md 3.4).
    pub keep_alive_profile: KeepAliveProfile,
    /// The host's IPv4 interfaces for discovery (protocol.md 8.1).
    pub interfaces: Box<dyn InterfaceSource>,
    /// Address to bind. The default `::` is dual-stack; tests bind 127.0.0.1.
    pub bind_ip: IpAddr,
    /// Run mDNS and the beacon (protocol.md 8). Off in loopback tests, which rely on
    /// last-known dial.
    pub discovery: bool,
    /// Whether the desktop is locked; pairing is refused while it is (SR-6). Defaults to
    /// [`AlwaysUnlocked`]; a daemon supplies logind `LockedHint` or `WTS_SESSION_LOCK`.
    pub lock_state: Box<dyn LockState>,
    /// Free space of the receive volume (SR-20). Defaults to [`platform::VolumeSpace`].
    pub free_space: Arc<dyn FreeSpace>,
    /// Seals `identity.key` and `pins.json` at rest (`docs/core-api.md` section 2a).
    /// `None` keeps the 0600 files of the desktop.
    pub secret_store: Option<Arc<dyn SecretStore>>,
    /// Adjusts the listening socket before it is bound (SR-31). A Windows daemon sets
    /// `SO_EXCLUSIVEADDRUSE` through it; `None` (the default) binds with the core's own
    /// options only. Rust only, not exposed through `playdrop-ffi`.
    pub listener_setup: Option<Arc<dyn ListenerSetup>>,
    /// Advertise `input.pointer`, `input.key` and `input.presenter` in `caps.in`
    /// (protocol.md 5.12): the host has a backend that applies remote input (a desktop
    /// daemon with `SendInput` or the `RemoteDesktop` portal). Off by default; phones never
    /// accept input. Any host may send input (`caps.out`) regardless.
    pub accepts_input: bool,
    /// How long a send (and an outbox pull) keeps trying to reach the other device after
    /// a drop before it is reported failed (protocol.md 7.6). Default 10 minutes. A send
    /// that gives up keeps its record and resumes when the device is back
    /// (`docs/core-api.md` section 12); tests shorten the window.
    pub reconnect_for: Duration,
}

impl HostConfig {
    /// A desktop configuration with the protocol port, dual-stack bind, discovery on and
    /// the system interface list.
    pub fn new(
        data_dir: impl Into<PathBuf>,
        receive_dir: impl Into<PathBuf>,
        device_name: impl Into<String>,
    ) -> Self {
        Self {
            data_dir: data_dir.into(),
            receive: ReceiveTarget::Directory(receive_dir.into()),
            port: crate::constants::TCP_PORT,
            device_name: device_name.into(),
            device_type: DeviceType::Desktop,
            keep_alive_profile: KeepAliveProfile::Desktop,
            interfaces: Box::new(platform::SystemInterfaces),
            bind_ip: IpAddr::V6(Ipv6Addr::UNSPECIFIED),
            discovery: true,
            lock_state: Box::new(AlwaysUnlocked),
            free_space: Arc::new(platform::VolumeSpace),
            secret_store: None,
            listener_setup: None,
            accepts_input: false,
            reconnect_for: send::RECONNECT_FOR,
        }
    }

    /// A mobile configuration (`docs/core-api.md` section 1): device type phone, the
    /// mobile keep-alive, discovery on, and received files staged under `staging_dir` and
    /// handed to `sink`.
    pub fn with_sink(
        data_dir: impl Into<PathBuf>,
        staging_dir: impl Into<PathBuf>,
        sink: Arc<dyn ReceiveSink>,
        device_name: impl Into<String>,
    ) -> Self {
        let mut c = Self::new(data_dir, PathBuf::new(), device_name);
        c.receive = ReceiveTarget::Staged {
            staging_dir: staging_dir.into(),
            sink,
        };
        c.device_type = DeviceType::Phone;
        c.keep_alive_profile = KeepAliveProfile::Mobile;
        c
    }
}

impl std::fmt::Debug for HostConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostConfig")
            .field("data_dir", &self.data_dir)
            .field("receive", &self.receive)
            .field("secret_store", &self.secret_store.is_some())
            .field("listener_setup", &self.listener_setup)
            .field("port", &self.port)
            .field("bind_ip", &self.bind_ip)
            .field("discovery", &self.discovery)
            .finish_non_exhaustive()
    }
}

/// A paired peer as the UI lists it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PeerSummary {
    /// Device ID.
    pub device_id: String,
    /// Short ID (SR-8).
    pub short_id: String,
    /// Local label.
    pub label: String,
    /// Whether a session is up.
    pub online: bool,
    /// Grants.
    pub grants: Grants,
    /// Last-known addresses, newest first.
    pub last_known: Vec<SocketAddr>,
    /// When pairing completed, Unix seconds.
    pub paired_at: u64,
    /// When this peer last sent a session hello or frame, Unix seconds; `None` if never
    /// (SR-8). Persisted in the pin at session start and end, so it survives a restart.
    #[serde(default)]
    pub last_seen: Option<u64>,
    /// The peer no longer has us pinned (protocol.md 5.4); true until the next
    /// successful pairing or unpair. Persisted in the pin.
    #[serde(default)]
    pub removed_by_peer: bool,
    /// What the peer accepts (`hello.caps.in`, protocol.md section 4) while online; empty
    /// offline. A UI shows a feature such as remote input only when the peer lists its
    /// capability here (PLAN.md anti-pattern 18: hide what cannot work, with the reason).
    #[serde(default)]
    pub capabilities: Vec<String>,
}

/// A device seen in "Add device" mode (protocol.md 8.3 rule 1): listed, never dialled
/// until the user picks it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AddDeviceCandidate {
    /// Sanitised advertised name.
    pub name: String,
    /// Device type token.
    pub device_type: String,
    /// Address to pass to [`Host::pair_with`].
    pub addr: SocketAddr,
}

/// What [`Host::push_clipboard`] did with one clipboard change.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ClipboardPush {
    /// Peers the clip was sent to.
    pub sent_to: Vec<DeviceId>,
    /// True if nothing was sent because the text equals the last clip applied from a peer
    /// or the last one sent (protocol.md 5.7 echo rule).
    pub skipped_echo: bool,
    /// Peers with the `clipboardOut` grant but no session right now.
    pub skipped_offline: usize,
    /// Paired peers without the `clipboardOut` grant.
    pub skipped_no_grant: usize,
    /// Online peers that cannot take this clip: an image to a peer without
    /// `clipboard.image`, or long text to one without `blobs`.
    #[serde(default)]
    pub skipped_unsupported: usize,
}

/// Per-peer reachability notes for the 10 s report and [`Host::diagnose`].
#[derive(Debug, Default, Clone)]
pub(crate) struct Reach {
    mdns: bool,
    beacon: bool,
    /// The carrier that most recently supplied a discovered address.
    last_via: Option<DiscoveryPath>,
    discovered: Vec<SocketAddr>,
    last_dial_error: Option<String>,
    /// The most recent failed dial, kept until the next one fails.
    last_failure: Option<diagnostics::DialFailure>,
}

/// Shared state of a running host.
pub(crate) struct Inner {
    pub(crate) identity: Arc<DeviceIdentity>,
    pub(crate) local_id: DeviceId,
    pub(crate) local_addr: SocketAddr,
    pub(crate) bind_ip: IpAddr,
    pub(crate) data_dir: PathBuf,
    pub(crate) receive: ReceiveTarget,
    pub(crate) device_name: String,
    pub(crate) device_type: DeviceType,
    pub(crate) keepalive: Duration,
    pub(crate) discovery_on: bool,
    pub(crate) pins: Pins,
    pub(crate) registry: Registry,
    events: mpsc::Sender<HostEvent>,
    pub(crate) window_flag: Arc<AtomicBool>,
    pub(crate) window: Mutex<AddDeviceWindow>,
    pub(crate) pair_limiter: Mutex<PairRequestLimiter>,
    pub(crate) qr_offer: Mutex<Option<QrOffer>>,
    pub(crate) pairing: Mutex<Option<pairing::ActivePairing>>,
    pub(crate) incoming: Mutex<HashMap<String, receive::Incoming>>,
    /// Incoming transfers that ended, answered for 24 h (protocol.md 7.6).
    pub(crate) ended: Mutex<HashMap<String, receive::EndedEntry>>,
    /// Writes and deletes of incoming records, in order (protocol.md 7.6).
    pub(crate) owner_io: receive::OwnerIo,
    /// Running sends and the index of recorded ones (protocol.md 7.6).
    pub(crate) sends: Mutex<send_resume::Sends>,
    pub(crate) transfers: Mutex<transfers::TransferBook>,
    pub(crate) discovery: Mutex<Option<Discovery>>,
    pub(crate) dial_limiter: Mutex<DialLimiter>,
    pub(crate) dial_locks: Mutex<HashMap<DeviceId, Arc<tokio::sync::Mutex<()>>>>,
    /// Wakes the last-known dial loop early (OS network change).
    pub(crate) dial_nudge: tokio::sync::Notify,
    pub(crate) reach: Mutex<HashMap<DeviceId, Reach>>,
    pub(crate) clipboard_echo: Mutex<ClipboardEcho>,
    /// Unix seconds of the last hello or frame per paired peer, updated on every frame.
    /// The pin store holds the persisted copy (written at session start and end only, so
    /// a busy session does not rewrite `pins.json` per frame).
    pub(crate) last_seen: Mutex<HashMap<DeviceId, u64>>,
    pub(crate) free_space: Arc<dyn FreeSpace>,
    /// Clipboard blobs this host serves (5.15).
    pub(crate) blobs: Mutex<blobs::BlobStore>,
    /// Remembered battery and presence, and the media and battery rate state.
    pub(crate) status: Mutex<status::StatusState>,
    /// Whether this host applies remote input (`caps.in` carries `input.*`, 5.12).
    pub(crate) accepts_input: bool,
    /// Remote input sessions and pending motion, both directions (5.12, SR-25).
    pub(crate) input: Mutex<input::InputState>,
    /// Outbox items held for peers (7.8).
    pub(crate) outbox: Mutex<HashMap<String, Arc<outbox::Held>>>,
    /// Outbox items being pulled from peers right now.
    pub(crate) pulling: Mutex<HashSet<String>>,
    /// [`HostConfig::reconnect_for`].
    pub(crate) reconnect_for: Duration,
    /// "Add device" adverts already reported in this window, by address (name, type).
    pub(crate) add_device_seen: Mutex<HashMap<SocketAddr, (String, String)>>,
    pub(crate) interfaces: Arc<dyn InterfaceSource>,
    pub(crate) lock_state: Box<dyn LockState>,
    shutdown: watch::Sender<bool>,
    task_guard: Arc<()>,
    server_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

/// How a transfer ended, for [`Inner::transfer_ended`].
#[derive(Debug)]
pub(crate) enum TransferEnd {
    /// Every file verified or skipped.
    Complete {
        bytes: u64,
        elapsed_ms: u64,
        roots: Vec<FileRoot>,
        bundle_root: Option<String>,
        saved: Vec<PathBuf>,
        folder: Option<PathBuf>,
        delivered: Vec<DeliveredFile>,
    },
    /// Failed, with the reason shown to the user (SR-22 wording).
    Failed(String),
    /// A send that failed but keeps its record and resumes later (protocol.md 7.6); the
    /// reason says how. Listed with `resumable` set.
    FailedResumable(String),
    /// Cancelled or declined by either side, with the reason.
    Cancelled(String),
}

/// Hashes for clipboard echo prevention (protocol.md 5.7).
#[derive(Debug, Default)]
pub(crate) struct ClipboardEcho {
    pub(crate) last_applied: Option<String>,
    pub(crate) last_sent: Option<String>,
}

/// Locks a std mutex, recovering from poisoning: every critical section here leaves the
/// state consistent.
pub(crate) fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Refuses clipboard text over the 1 MiB sync limit (protocol.md 5.7, SR-24).
fn check_clipboard_size(text: &str) -> Result<()> {
    if text.len() > blobs::TEXT_BLOB_MAX {
        return Err(Error::TooLarge(
            "clipboard text over 1 MiB is not synced".into(),
        ));
    }
    Ok(())
}

/// An inline text `clipboard` frame (protocol.md 5.7).
fn clipboard_frame(text: &str, hash: String, sensitive: bool) -> Frame {
    Frame::Clipboard(Clipboard {
        kind: ClipboardKind::Text,
        mime: "text/plain;charset=utf-8".into(),
        hash,
        text: Some(text.to_owned()),
        blob: None,
        sensitive,
    })
}

/// Text for a clipboard frame: inline up to 32 KiB, else as a blob held for the peer
/// (protocol.md 5.7, 5.15).
#[derive(Debug, Clone)]
enum ClipBody {
    Inline(String),
    Blob(bytes::Bytes),
}

impl ClipBody {
    fn of(text: &str) -> Self {
        if text.len() <= crate::session::types::CLIPBOARD_INLINE_MAX {
            Self::Inline(text.to_owned())
        } else {
            Self::Blob(bytes::Bytes::from(blobs::normalize_newlines(text)))
        }
    }

    /// The frame for `sess`, holding the blob for its peer. `None` when the peer cannot
    /// fetch blobs.
    fn frame_for(
        &self,
        inner: &Inner,
        sess: &session::Session,
        hash: &str,
        sensitive: bool,
    ) -> Option<Frame> {
        match self {
            Self::Inline(t) => Some(clipboard_frame(t, hash.to_owned(), sensitive)),
            Self::Blob(bytes) => {
                if !sess.peer_accepts(&Capability::Blobs) {
                    return None;
                }
                let id = lock(&inner.blobs).put(
                    bytes.clone(),
                    "text/plain;charset=utf-8",
                    sess.peer(),
                    tokio::time::Instant::now(),
                )?;
                Some(Frame::Clipboard(blobs::blob_clipboard(
                    ClipboardKind::Text,
                    "text/plain;charset=utf-8",
                    id,
                    bytes.len(),
                    sensitive,
                )))
            }
        }
    }
}

/// Now as Unix seconds; 0 if the clock is before 1970.
pub(crate) fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

impl Inner {
    /// Records that a paired peer was heard from just now (memory only).
    pub(crate) fn touch_peer(&self, peer: DeviceId) {
        lock(&self.last_seen).insert(peer, unix_now());
    }

    /// Writes the in-memory last-seen time of `peer` to its pin, in the background.
    pub(crate) fn persist_last_seen(self: &Arc<Self>, peer: DeviceId) {
        let Some(at) = lock(&self.last_seen).get(&peer).copied() else {
            return;
        };
        let me = Arc::clone(self);
        self.spawn(async move {
            if let Err(e) = me.pins.update(move |s| s.set_last_seen(&peer, at)).await {
                tracing::debug!(error = %e, "last-seen time not stored");
            }
        });
    }

    /// True if a clip with this hash was the last one applied from a peer or the last one
    /// sent (protocol.md 5.7 echo rule).
    pub(crate) fn is_clipboard_echo(&self, hash: &str) -> bool {
        let echo = lock(&self.clipboard_echo);
        echo.last_applied.as_deref() == Some(hash) || echo.last_sent.as_deref() == Some(hash)
    }

    /// Queues an event for the UI. If the UI is not reading, events are dropped rather
    /// than queued without bound.
    pub(crate) fn emit(&self, ev: HostEvent) {
        if self.events.try_send(ev).is_err() {
            tracing::debug!("host event dropped: channel full or closed");
        }
    }

    /// Records transfer progress in the book and reports it to the UI.
    pub(crate) fn transfer_progress(
        &self,
        id: &str,
        peer: &DeviceId,
        direction: TransferDirection,
        reading: transfers::ProgressReading,
    ) {
        lock(&self.transfers).progress(id, direction, reading);
        self.emit(HostEvent::TransferProgress {
            id: id.to_owned(),
            peer: peer.to_hex(),
            direction,
            bytes: reading.bytes,
            total: reading.total,
            bytes_per_sec: reading.bytes_per_sec,
            files_done: reading.files_done,
        });
    }

    /// Ends a transfer: moves its snapshot to the finished list and emits
    /// [`HostEvent::TransferComplete`] or [`HostEvent::TransferFailed`]. Keeping both in one
    /// place means [`Host::transfers`] and the event stream never disagree.
    pub(crate) fn transfer_ended(
        &self,
        id: &str,
        peer: &DeviceId,
        direction: TransferDirection,
        end: TransferEnd,
    ) {
        let (status, note, saved, folder, delivered) = match &end {
            TransferEnd::Complete {
                saved,
                folder,
                delivered,
                ..
            } => (
                TransferStatus::Complete,
                None,
                saved.clone(),
                folder.clone(),
                delivered.clone(),
            ),
            TransferEnd::Failed(r) | TransferEnd::FailedResumable(r) => (
                TransferStatus::Failed,
                Some(r.clone()),
                Vec::new(),
                None,
                Vec::new(),
            ),
            TransferEnd::Cancelled(r) => (
                TransferStatus::Cancelled,
                Some(r.clone()),
                Vec::new(),
                None,
                Vec::new(),
            ),
        };
        {
            let mut book = lock(&self.transfers);
            book.finish_with_files(id, direction, status, note, saved, folder, delivered);
            if matches!(end, TransferEnd::FailedResumable(_)) {
                book.set_resumable(id, direction, true);
            }
        }
        let ev = match end {
            TransferEnd::Complete {
                bytes,
                elapsed_ms,
                roots,
                bundle_root,
                saved,
                folder,
                delivered,
            } => HostEvent::TransferComplete {
                id: id.to_owned(),
                peer: peer.to_hex(),
                direction,
                bytes,
                elapsed_ms,
                roots,
                bundle_root,
                saved,
                folder,
                delivered,
            },
            TransferEnd::Failed(reason)
            | TransferEnd::FailedResumable(reason)
            | TransferEnd::Cancelled(reason) => HostEvent::TransferFailed {
                id: id.to_owned(),
                peer: peer.to_hex(),
                direction,
                reason,
            },
        };
        self.emit(ev);
    }

    /// Spawns a task that ends at shutdown.
    pub(crate) fn spawn<F>(self: &Arc<Self>, fut: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let guard = Arc::clone(&self.task_guard);
        let mut stop = self.shutdown.subscribe();
        tokio::spawn(async move {
            tokio::select! {
                () = fut => {}
                () = crate::net::stopped(&mut stop) => {}
            }
            drop(guard);
        });
    }

    /// Wraps a request handler so it counts as a host task and is dropped at shutdown.
    pub(crate) fn guard_task<F>(&self, fut: F) -> impl Future<Output = ()> + Send + 'static
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let guard = Arc::clone(&self.task_guard);
        let mut stop = self.shutdown.subscribe();
        async move {
            tokio::select! {
                biased;
                () = crate::net::stopped(&mut stop) => {}
                () = fut => {}
            }
            drop(guard);
        }
    }

    pub(crate) fn shutdown_rx(&self) -> watch::Receiver<bool> {
        self.shutdown.subscribe()
    }

    pub(crate) fn is_shutting_down(&self) -> bool {
        *self.shutdown.borrow()
    }

    /// Our `hello.port`.
    pub(crate) fn port(&self) -> u16 {
        self.local_addr.port()
    }

    /// The local label of a pinned peer, else its Short ID.
    pub(crate) fn label(&self, id: &DeviceId) -> String {
        self.pins
            .get(id)
            .map_or_else(|| id.short_id(), |p| p.label.clone())
    }

    /// Closes the "Add device" window (success, user, or expiry) and forgets the adverts
    /// reported in it, so the next window reports each device again.
    pub(crate) fn close_window(&self) {
        lock(&self.window).close();
        self.window_flag.store(false, Ordering::SeqCst);
        lock(&self.add_device_seen).clear();
    }

    /// The directory transfers are staged beneath (`docs/core-api.md` section 2).
    pub(crate) fn receive_root(&self) -> &Path {
        self.receive.root()
    }

    /// The sink binding for a transfer from `owner` in `Staged` mode.
    pub(crate) fn sink_binding(&self, owner: &DeviceId) -> Option<SinkBinding> {
        match &self.receive {
            ReceiveTarget::Directory(_) => None,
            ReceiveTarget::Staged { sink, .. } => Some(SinkBinding {
                sink: Arc::clone(sink),
                peer: owner.to_hex(),
            }),
        }
    }
}

/// A running host. Call [`Host::shutdown`] to stop it; dropping it leaves its tasks running
/// until the runtime ends.
pub struct Host {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for Host {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Host")
            .field("device", &short(&self.inner.local_id))
            .field("addr", &self.inner.local_addr)
            .finish_non_exhaustive()
    }
}

impl Host {
    /// Starts a host: loads or creates the identity and pin store (SR-28), binds the port
    /// exclusively (SR-31, [`Error::PortHeld`] if taken), starts the server, reloads
    /// unfinished incoming transfers and restarts unfinished sends (protocol.md 7.6),
    /// starts discovery and the last-known dial loop. Returns the host and its event
    /// stream.
    #[allow(clippy::too_many_lines, reason = "start-up is one linear sequence")]
    pub async fn start(config: HostConfig) -> Result<(Self, mpsc::Receiver<HostEvent>)> {
        let data_dir = config.data_dir.clone();
        let identity = {
            let dir = data_dir.clone();
            let sealer = config.secret_store.clone();
            tokio::task::spawn_blocking(move || {
                DeviceIdentity::load_or_create_sealed(&dir, &sealer)
            })
            .await
            .map_err(|e| Error::Io(std::io::Error::other(e)))??
        };
        let store = {
            let path = data_dir.join(PIN_FILE);
            let sealer = config.secret_store.clone();
            tokio::task::spawn_blocking(move || PinStore::open_sealed(path, sealer))
                .await
                .map_err(|e| Error::Io(std::io::Error::other(e)))??
        };
        let identity = Arc::new(identity);
        let limits = Limits::default();
        let server = Server::bind(
            SocketAddr::new(config.bind_ip, config.port),
            limits.clone(),
            config.listener_setup.as_deref(),
        )?;
        let local_addr = server.local_addr();
        let pins = Pins::new(store);
        let window_flag = Arc::new(AtomicBool::new(false));
        let (events_tx, events_rx) = mpsc::channel(4096);
        let interfaces: Arc<dyn InterfaceSource> = Arc::from(config.interfaces);
        let local_id = *identity.device_id();
        let inner = Arc::new(Inner {
            local_id,
            local_addr,
            bind_ip: config.bind_ip,
            data_dir,
            receive: config.receive,
            device_name: crate::session::sanitize_name(&config.device_name),
            device_type: config.device_type,
            keepalive: config.keep_alive_profile.interval(),
            discovery_on: config.discovery,
            registry: Registry::new(local_id),
            pins,
            identity: Arc::clone(&identity),
            events: events_tx,
            window_flag: Arc::clone(&window_flag),
            window: Mutex::new(AddDeviceWindow::new()),
            pair_limiter: Mutex::new(PairRequestLimiter::new(limits.max_pair_requests_per_minute)),
            qr_offer: Mutex::new(None),
            pairing: Mutex::new(None),
            incoming: Mutex::new(HashMap::new()),
            ended: Mutex::new(HashMap::new()),
            owner_io: receive::OwnerIo::start(),
            sends: Mutex::new(send_resume::Sends::default()),
            transfers: Mutex::new(transfers::TransferBook::default()),
            discovery: Mutex::new(None),
            dial_limiter: Mutex::new(DialLimiter::new()),
            dial_locks: Mutex::new(HashMap::new()),
            dial_nudge: tokio::sync::Notify::new(),
            reach: Mutex::new(HashMap::new()),
            clipboard_echo: Mutex::new(ClipboardEcho::default()),
            last_seen: Mutex::new(HashMap::new()),
            free_space: config.free_space,
            blobs: Mutex::new(blobs::BlobStore::default()),
            status: Mutex::new(status::StatusState::default()),
            accepts_input: config.accepts_input,
            input: Mutex::new(input::InputState::default()),
            outbox: Mutex::new(HashMap::new()),
            pulling: Mutex::new(HashSet::new()),
            reconnect_for: config.reconnect_for,
            add_device_seen: Mutex::new(HashMap::new()),
            interfaces: Arc::clone(&interfaces),
            lock_state: config.lock_state,
            shutdown: watch::Sender::new(false),
            task_guard: Arc::new(()),
            server_task: Mutex::new(None),
        });

        let lookup: Arc<dyn tls::PinLookup> = inner.pins.lookup();
        let acceptor = TlsAcceptor {
            config: tls::build_server_config(
                &identity,
                Arc::clone(&lookup),
                Arc::clone(&window_flag),
            ),
            pins: lookup,
            window: window_flag,
        };
        let router = Arc::new(router::HostRouter {
            inner: Arc::clone(&inner),
        });
        // Unfinished transfers and outbox items are reloaded before the first connection
        // is accepted: a sender that reconnects the moment the port answers asks for its
        // transfer by ID, and a `404` for one still being reloaded would end its send.
        receive::load_persisted(&inner).await;
        outbox::load_persisted(&inner).await;
        // Sends that were running when this host stopped start again from their records,
        // and the rest are indexed so a peer that connects at once finds its waiting
        // sends (protocol.md 7.6).
        send_resume::load_persisted(&inner).await;

        let server_task = tokio::spawn(server.run(acceptor, router, inner.shutdown_rx()));
        *lock(&inner.server_task) = Some(server_task);

        if inner.discovery_on {
            let key = inner.pins.read().local_announce_key().key;
            let mut cfg = DiscoveryConfig::new(key, local_addr.port(), Arc::clone(&interfaces));
            if config.keep_alive_profile == KeepAliveProfile::Mobile {
                cfg.schedule = BeaconSchedule::DiscoveryWindowsOnly;
            }
            match Discovery::start(cfg, inner.pins.peer_keys(), Arc::clone(&inner.window_flag)) {
                Ok((d, rx)) => {
                    if let Err(e) =
                        d.set_add_device_fields(&inner.device_name, inner.device_type.as_str())
                    {
                        tracing::debug!(error = %e, "add-device fields not set");
                    }
                    *lock(&inner.discovery) = Some(d);
                    let i2 = Arc::clone(&inner);
                    inner.spawn(connect::discovery_events(i2, rx));
                }
                Err(e) => {
                    tracing::warn!(error = %e, "discovery could not start; last-known dial only");
                }
            }
        }

        inner.spawn(connect::dial_loop(Arc::clone(&inner)));
        inner.spawn(connect::diagnostics(Arc::clone(&inner)));
        inner.spawn(receive::maintenance(Arc::clone(&inner)));
        inner.spawn(outbox::maintenance(Arc::clone(&inner)));
        inner.spawn(send_resume::maintenance(Arc::clone(&inner)));
        inner.spawn(input::maintenance(Arc::clone(&inner)));

        inner.emit(HostEvent::Listening {
            addr: local_addr,
            device_id: local_id.to_hex(),
            short_id: local_id.short_id(),
        });
        tracing::info!(addr = %local_addr, device = %short(&local_id), "host started");
        Ok((Self { inner }, events_rx))
    }

    /// Stops everything: the server, every connection, discovery and all tasks. Unfinished
    /// transfers stay on disk and resume after the next start (protocol.md 7.6).
    ///
    /// Takes `&self`, so a daemon can share the host as `Arc<Host>` with its IPC tasks.
    /// Idempotent: only the first call does the work; later or concurrent calls return at
    /// once. After it returns, the other methods fail or do nothing; drop the host.
    pub async fn shutdown(&self) {
        let inner = &self.inner;
        if inner.shutdown.send_replace(true) {
            return;
        }
        // Background last-seen writes stop with the tasks; write the newest times now so
        // the device list shows them after a restart (SR-8).
        let seen: Vec<(DeviceId, u64)> = lock(&inner.last_seen)
            .iter()
            .map(|(id, at)| (*id, *at))
            .collect();
        if !seen.is_empty() {
            let stored = inner
                .pins
                .update(move |s| {
                    for (id, at) in &seen {
                        // A peer unpaired meanwhile has no pin to update.
                        let _ = s.set_last_seen(id, *at);
                    }
                    Ok(())
                })
                .await;
            if let Err(e) = stored {
                tracing::debug!(error = %e, "last-seen times not stored at shutdown");
            }
        }
        for c in inner.registry.all_connections() {
            c.close(CloseReason::Shutdown);
        }
        let server = lock(&inner.server_task).take();
        if let Some(task) = server {
            let _ = tokio::time::timeout(Duration::from_secs(3), task).await;
        }
        let disc = lock(&inner.discovery).take();
        if let Some(d) = disc {
            let _ = tokio::time::timeout(Duration::from_secs(3), d.stop()).await;
        }
        let deadline = Instant::now() + Duration::from_secs(3);
        while Arc::strong_count(&inner.task_guard) > 1 && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        for c in inner.registry.all_connections() {
            let _ = tokio::time::timeout(Duration::from_millis(500), c.closed()).await;
        }
        // Every task is gone: what they queued for the incoming records lands now, before
        // a host restarted on the same folders reads them.
        inner.owner_io.close().await;
        tracing::info!(device = %short(&inner.local_id), "host stopped");
    }

    /// This device's ID.
    pub fn device_id(&self) -> DeviceId {
        self.inner.local_id
    }

    /// This device's Short ID (SR-8).
    pub fn short_id(&self) -> String {
        self.inner.local_id.short_id()
    }

    /// The bound address.
    pub fn local_addr(&self) -> SocketAddr {
        self.inner.local_addr
    }

    /// Paired peers with their online state.
    pub fn peers(&self) -> Vec<PeerSummary> {
        let store = self.inner.pins.read();
        store
            .iter()
            .map(|p| PeerSummary {
                device_id: p.device_id.to_hex(),
                short_id: p.device_id.short_id(),
                label: p.label.clone(),
                online: self.inner.registry.session(&p.device_id).is_some(),
                grants: p.grants,
                last_known: p.last_known.clone(),
                paired_at: p.paired_at,
                last_seen: lock(&self.inner.last_seen)
                    .get(&p.device_id)
                    .copied()
                    .max(p.last_seen),
                removed_by_peer: p.removed_by_peer,
                capabilities: self
                    .inner
                    .registry
                    .session(&p.device_id)
                    .map(|s| s.peer_caps_in())
                    .unwrap_or_default(),
            })
            .collect()
    }

    /// Finds a paired peer by Device ID, Short ID (with or without hyphens, or any unique
    /// prefix of it) or exact local label.
    pub fn resolve_peer(&self, query: &str) -> Option<DeviceId> {
        let q = query.trim().to_ascii_lowercase();
        if let Ok(id) = q.parse::<DeviceId>() {
            return self.inner.pins.contains(&id).then_some(id);
        }
        let q_short: String = q.chars().filter(|c| *c != '-').collect();
        let store = self.inner.pins.read();
        let matches: Vec<DeviceId> = store
            .iter()
            .filter(|p| {
                let s: String = p
                    .device_id
                    .short_id()
                    .chars()
                    .filter(|c| *c != '-')
                    .collect();
                (!q_short.is_empty() && s.starts_with(&q_short))
                    || p.label.eq_ignore_ascii_case(query.trim())
                    || p.device_id.to_hex().starts_with(&q)
            })
            .map(|p| p.device_id)
            .collect();
        match matches.as_slice() {
            [one] => Some(*one),
            _ => None,
        }
    }

    /// The grants of a paired peer.
    pub fn grants(&self, peer: &DeviceId) -> Option<Grants> {
        self.inner.pins.get(peer).map(|p| p.grants)
    }

    /// Replaces a peer's grants. Only the local UI may call this (SR-14).
    pub async fn set_grants(&self, peer: &DeviceId, grants: Grants) -> Result<()> {
        let id = *peer;
        self.inner
            .pins
            .update(move |s| s.set_grants(&id, grants))
            .await
    }

    /// Renames a peer locally (SR-8: peer renames never change it).
    pub async fn set_label(&self, peer: &DeviceId, label: String) -> Result<()> {
        let id = *peer;
        self.inner
            .pins
            .update(move |s| s.set_label(&id, label))
            .await
    }

    /// Unpairs a peer (protocol.md 6.4): sends `unpair`, closes its connections within 1 s,
    /// deletes its pin, grants, tokens and transfers, and rotates the announce key.
    pub async fn unpair(&self, peer: &DeviceId) -> Result<()> {
        session::unpair(&self.inner, *peer, true).await
    }

    /// Opens the "Add device" window for 120 s (protocol.md 6.1, SR-6). While open, this
    /// host advertises `name` and accepts unpinned certificates in unpaired mode.
    /// [`Error::Locked`] while [`HostConfig::lock_state`] reports the desktop locked.
    pub fn open_add_device_window(&self) -> Result<()> {
        pairing::open_window(&self.inner)
    }

    /// Closes the "Add device" window.
    pub fn close_add_device_window(&self) {
        self.inner.close_window();
        *lock(&self.inner.qr_offer) = None;
    }

    /// True while the window is open.
    pub fn add_device_window_open(&self) -> bool {
        self.inner.window_flag.load(Ordering::SeqCst)
    }

    /// Devices seen in their own "Add device" window (protocol.md 8.3 rule 1).
    pub fn add_device_list(&self) -> Vec<AddDeviceCandidate> {
        let d = lock(&self.inner.discovery);
        d.as_ref()
            .map(|d| {
                d.add_device_list()
                    .into_iter()
                    .map(|(addr, e)| AddDeviceCandidate {
                        name: e.name,
                        device_type: e.device_type,
                        addr,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Starts SAS pairing as the initiator with the device at `addr`, picked by the user
    /// from the "Add device" list or typed in (protocol.md 6.1, 6.2). Opens the local
    /// window if needed. Progress arrives as [`HostEvent::PairingPrompt`], then
    /// [`HostEvent::Paired`] or [`HostEvent::PairingFailed`]. [`Error::Locked`] while the
    /// desktop is locked (SR-6).
    pub async fn pair_with(&self, addr: SocketAddr) -> Result<()> {
        pairing::pair_with(&self.inner, addr).await
    }

    /// The user's answer to the current [`HostEvent::PairingPrompt`]: accept ("Pair",
    /// "Add") or reject.
    pub fn pairing_respond(&self, accept: bool) {
        pairing::respond(&self.inner, accept);
    }

    /// Opens the window and returns a QR pairing URI valid for 120 s (protocol.md 6.3,
    /// SR-9). The URI holds a one-time secret: show it, never log it. [`Error::Locked`]
    /// while the desktop is locked (SR-6).
    pub fn show_qr(&self) -> Result<String> {
        pairing::show_qr(&self.inner)
    }

    /// Pairs by a scanned QR URI as the scanner (protocol.md 6.3). [`Error::Locked`] while
    /// the desktop is locked (SR-6).
    pub async fn scan_qr(&self, uri: &str) -> Result<()> {
        pairing::scan_qr(&self.inner, uri).await
    }

    /// Sends files and folders to a paired peer (protocol.md 7): builds the manifest, offers
    /// it, streams up to 4 files plus the bundle concurrently, and resumes after drops.
    pub async fn send(&self, peer: &DeviceId, paths: Vec<PathBuf>) -> Result<TransferHandle> {
        let sources = paths.into_iter().map(SendSource::Path).collect();
        self.send_sources(peer, sources, None).await
    }

    /// Sends paths and host-supplied streams with an optional note (`docs/core-api.md`
    /// section 3, protocol.md 7). A stream's opener is called on a blocking thread at offer
    /// time, once per `PUT` and once per bundle member; an opener error fails the offer (at
    /// offer time) or the stream (later) with its `io::Error` text.
    pub async fn send_sources(
        &self,
        peer: &DeviceId,
        sources: Vec<SendSource>,
        note: Option<String>,
    ) -> Result<TransferHandle> {
        send::start(&self.inner, *peer, sources, note).await
    }

    /// Cancels a transfer by ID, in either direction (protocol.md 7.5).
    ///
    /// - Outgoing: same as [`TransferHandle::cancel`]; the send stops mid-file, sends
    ///   `DELETE /v1/transfers/{id}` with reason `user`, and the receiver deletes its
    ///   `.part` files.
    /// - Incoming: deletes every `.part`, answers later `PUT`s with `410 cancelled`, and
    ///   pushes a `transfer` frame with state `cancelled`, reason `user` (5.13), so the
    ///   sender stops. A pending offer is withdrawn the same way.
    /// - A failed send that waits to resume (`docs/core-api.md` section 12): its record is
    ///   deleted, so it never resumes, and when the peer is online the receiver gets the
    ///   `DELETE` and frees its slot (best effort).
    ///
    /// [`Error::NotFound`] if no live or resumable transfer has this ID.
    pub async fn cancel_transfer(&self, id: &str) -> Result<()> {
        if send_resume::cancel(&self.inner, id).await {
            return Ok(());
        }
        receive::cancel_local(&self.inner, id).await
    }

    /// Live transfers in both directions plus recently finished ones (the last
    /// [`KEEP_FINISHED`], none older than [`KEEP_FOR`], and every one that ended less than
    /// [`KEEP_AT_LEAST`] ago), oldest first, so a UI that connects late can rebuild its
    /// view.
    pub fn transfers(&self) -> Vec<TransferSnapshot> {
        lock(&self.inner.transfers).list()
    }

    /// The user's answer to a [`HostEvent::TransferOffer`] (protocol.md 7.3, 5.13).
    pub async fn respond_offer(&self, id: &str, accept: bool) -> Result<()> {
        receive::respond_offer(&self.inner, id, accept).await
    }

    /// Sends a `ping` with an optional message (protocol.md 5.6).
    pub fn ping(&self, peer: &DeviceId, message: Option<&str>) -> Result<()> {
        let s = self.inner.registry.session(peer).ok_or(Error::NotFound)?;
        s.send(Frame::Ping(Ping::new(message)));
        Ok(())
    }

    /// Rings (`start`) or silences a peer (protocol.md 5.6). [`Error::NotFound`] without a
    /// session.
    pub fn find(&self, peer: &DeviceId, start: bool) -> Result<()> {
        let s = self.inner.registry.session(peer).ok_or(Error::NotFound)?;
        s.send(Frame::Find(Find {
            action: if start {
                FindAction::Start
            } else {
                FindAction::Stop
            },
        }));
        Ok(())
    }

    /// Pushes clipboard text to one peer (protocol.md 5.7): needs the `clipboardOut` grant,
    /// and is skipped when it would echo the last clip applied from a peer or the last one
    /// sent. Returns whether it was sent. Text over 32 KiB goes as a blob (5.15) the peer
    /// fetches within 5 minutes; over 1 MiB is [`Error::TooLarge`] (SR-24). A push that
    /// fails (too large, no session) is not remembered, so retrying the same text works.
    /// Most callers want [`Host::push_clipboard`], which sends to every eligible peer.
    pub fn send_clipboard(&self, peer: &DeviceId, text: &str, sensitive: bool) -> Result<bool> {
        let pin = self.inner.pins.get(peer).ok_or(Error::NotPaired)?;
        if !pin.grants.clipboard_out {
            return Err(Error::Forbidden("clipboardOut grant is off"));
        }
        check_clipboard_size(text)?;
        let hash = crate::session::types::clipboard_text_hash(text);
        if self.inner.is_clipboard_echo(&hash) {
            return Ok(false);
        }
        let s = self.inner.registry.session(peer).ok_or(Error::NotFound)?;
        let frame = ClipBody::of(text)
            .frame_for(&self.inner, &s, &hash, sensitive)
            .ok_or_else(|| Error::TooLarge("the device cannot fetch text over 32 KiB".into()))?;
        s.send(frame);
        lock(&self.inner.clipboard_echo).last_sent = Some(hash);
        Ok(true)
    }

    /// Pushes a local clipboard change to every online peer with the `clipboardOut` grant
    /// (protocol.md 5.7, SR-24), applying the echo rule once for all of them: text equal to
    /// the last clip applied from a peer or the last one sent goes nowhere. Text over
    /// 32 KiB goes as a blob (5.15); over 1 MiB returns [`Error::TooLarge`]. The text is
    /// remembered as sent only if at least one peer got it, so a clip copied while every
    /// peer was offline is pushed again on the next change notification.
    pub fn push_clipboard(&self, text: &str, sensitive: bool) -> Result<ClipboardPush> {
        check_clipboard_size(text)?;
        let hash = crate::session::types::clipboard_text_hash(text);
        let body = ClipBody::of(text);
        Ok(self.push_clip(&hash, |s| body.frame_for(&self.inner, s, &hash, sensitive)))
    }

    /// Pushes a clipboard image (at most 16 MiB) to every online peer with the
    /// `clipboardOut` grant and the `clipboard.image` capability, as a blob each fetches
    /// and verifies (protocol.md 5.7, 5.15). Same echo rule as text; peers without the
    /// capability are counted in [`ClipboardPush::skipped_unsupported`].
    pub fn push_clipboard_image(
        &self,
        bytes: Vec<u8>,
        mime: &str,
        sensitive: bool,
    ) -> Result<ClipboardPush> {
        if bytes.len() > blobs::IMAGE_BLOB_MAX {
            return Err(Error::TooLarge("clipboard image over 16 MiB".into()));
        }
        if !mime.starts_with("image/") || http::HeaderValue::from_str(mime).is_err() {
            return Err(Error::BadRequest("not an image MIME type".into()));
        }
        let hash = blobs::blob_id(&bytes);
        let bytes = bytes::Bytes::from(bytes);
        let inner = &self.inner;
        Ok(self.push_clip(&hash, |s| {
            if !(s.peer_accepts(&Capability::ClipboardImage) && s.peer_accepts(&Capability::Blobs))
            {
                return None;
            }
            let id = lock(&inner.blobs).put(
                bytes.clone(),
                mime,
                s.peer(),
                tokio::time::Instant::now(),
            )?;
            Some(Frame::Clipboard(blobs::blob_clipboard(
                ClipboardKind::Image,
                mime,
                id,
                bytes.len(),
                sensitive,
            )))
        }))
    }

    /// The fan-out shared by text and image pushes (protocol.md 5.7 echo rule).
    fn push_clip<F>(&self, hash: &str, mut frame_for: F) -> ClipboardPush
    where
        F: FnMut(&session::Session) -> Option<Frame>,
    {
        let mut out = ClipboardPush::default();
        if self.inner.is_clipboard_echo(hash) {
            out.skipped_echo = true;
            return out;
        }
        let targets: Vec<(DeviceId, bool)> = self
            .inner
            .pins
            .read()
            .iter()
            .map(|p| (p.device_id, p.grants.clipboard_out))
            .collect();
        for (id, granted) in targets {
            if !granted {
                out.skipped_no_grant += 1;
                continue;
            }
            match self.inner.registry.session(&id) {
                Some(s) => match frame_for(&s) {
                    Some(f) => {
                        s.send(f);
                        out.sent_to.push(id);
                    }
                    None => out.skipped_unsupported += 1,
                },
                None => out.skipped_offline += 1,
            }
        }
        if !out.sent_to.is_empty() {
            lock(&self.inner.clipboard_echo).last_sent = Some(hash.to_owned());
        }
        out
    }

    /// Remembers this device's battery and pushes it to every online peer now and at each
    /// session start, at most once per 60 s per peer unless `charging` or `low` changed
    /// (protocol.md 5.8).
    pub fn set_battery(&self, level: u8, charging: bool, low: bool) {
        status::set_battery(&self.inner, level, charging, low);
    }

    /// Remembers this device's presence and pushes it to every online peer now and at
    /// each session start (protocol.md 5.10).
    pub fn set_presence(
        &self,
        state: PresenceState,
        reason: Option<String>,
        expect_back_ms: Option<u64>,
    ) {
        status::set_presence(&self.inner, state, reason, expect_back_ms);
    }

    /// Asks a peer to control one of its players (protocol.md 5.9). Needs a session
    /// ([`Error::NotFound`]) and the peer's `media.control` capability
    /// ([`Error::Forbidden`]); the peer applies it only with its `media` grant for us.
    pub fn send_media_command(
        &self,
        peer: &DeviceId,
        player: &str,
        action: MediaAction,
        value: Option<u64>,
    ) -> Result<()> {
        status::send_media_command(&self.inner, peer, player, action, value)
    }

    /// Pushes a local player's state to every online peer with the `media` grant,
    /// coalesced to 2 frames per second per player (protocol.md 5.9).
    pub fn push_media_state(&self, state: MediaState) {
        status::push_media_state(&self.inner, state);
    }

    /// Sends one remote input frame to `peer` (protocol.md 5.12). Open with
    /// `Input::Session { op: Start }`, end with `Stop`; the target applies nothing outside
    /// a session. Needs a session ([`Error::NotFound`]) and the matching `input.*`
    /// capability on the peer ([`Error::Forbidden`]; see [`PeerSummary::capabilities`]).
    /// Pointer moves and scrolls are coalesced to one frame per 8 ms. A peer that refuses
    /// a frame (no `input` grant on its side) answers with an `error` that arrives as
    /// [`HostEvent::PeerRefused`].
    pub fn send_input(&self, peer: &DeviceId, input: Input) -> Result<()> {
        input::send_input(&self.inner, peer, input)
    }

    /// Peers that hold an open input session on this host (we are the target), for a
    /// "Remote input from <label>" indicator (SR-25).
    pub fn input_sessions(&self) -> Vec<DeviceId> {
        lock(&self.inner.input).open_sessions()
    }

    /// Ends every input session this host is the target of and tells the peers: the
    /// SR-25 kill switch. Each ended session arrives as a final
    /// [`HostEvent::Input`] with `session`/`stop` so the backend releases every key and
    /// button. Returns the peers that had one.
    pub fn stop_input_sessions(&self) -> Vec<DeviceId> {
        input::stop_all_incoming(&self.inner)
    }

    /// Queues files for `peer` to pull (protocol.md 7.8) and returns the item ID. Sends
    /// `outbox_ready` now if the peer is online and at every session start while the item
    /// waits. Items made only of paths survive a restart; items with streams do not.
    pub async fn queue_outbox(
        &self,
        peer: &DeviceId,
        sources: Vec<SendSource>,
        note: Option<String>,
    ) -> Result<String> {
        outbox::queue(&self.inner, *peer, sources, note).await
    }

    /// Items this host holds for peers, oldest first.
    pub fn outbox(&self) -> Vec<OutboxItem> {
        outbox::list(&self.inner)
    }

    /// Removes a held item. [`Error::NotFound`] if unknown.
    pub async fn remove_outbox(&self, id: &str) -> Result<()> {
        outbox::remove(&self.inner, id).await
    }

    /// Pulls every item `peer` holds for us (protocol.md 7.8) and returns the transfer IDs
    /// being pulled. Each is an incoming transfer: the `files` grant decides acceptance,
    /// and progress and completion arrive as receive-side transfer events.
    pub async fn pull_outbox(&self, peer: &DeviceId) -> Result<Vec<String>> {
        outbox::pull(&self.inner, *peer).await
    }

    /// A diagnostic snapshot on demand: own address and port, whether the listener is up,
    /// the interfaces, both discovery carriers (group joined, beacon heard in the last
    /// 60 s, mDNS registered, any mDNS peer seen) and every paired peer with its most recent
    /// dial failure.
    ///
    /// A firewall blocking inbound TCP cannot be seen from inside the host: it looks the
    /// same as nobody dialling. The daemon runs its own loopback-versus-LAN probe with
    /// `port`, `bind_ok` and `interfaces` (see the [`diagnostics`] module docs); multicast
    /// blocked reads as groups joined but no beacon heard while a peer is on the subnet.
    pub fn diagnose(&self) -> Diagnostics {
        diagnostics::build(&self.inner)
    }

    /// Tells the host the OS reported a network change (Wi-Fi switch, cable, VPN): re-reads
    /// interfaces, rejoins the beacon group, sends a fresh beacon burst and rotates the
    /// mDNS record (protocol.md 8.1, SR-4), and runs the last-known dial round now instead
    /// of at the next 5 s tick (still at most one dial per peer per 5 s, 8.3 rule 1).
    pub fn network_changed(&self) {
        if let Some(d) = lock(&self.inner.discovery).as_ref() {
            d.network_changed();
        }
        self.inner.dial_nudge.notify_one();
    }

    /// Dials a paired peer now if no session is up, using its last-known addresses.
    pub async fn connect(&self, peer: &DeviceId) -> Result<()> {
        connect::ensure_session(&self.inner, *peer).await
    }

    /// True if a session with `peer` is up.
    pub fn is_online(&self, peer: &DeviceId) -> bool {
        self.inner.registry.session(peer).is_some()
    }
}

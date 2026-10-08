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
//! - [`router`]: request routing for the server (7.1).
//! - [`connect`]: last-known dial, discovery wiring and diagnostics (3.2, 8).
//! - [`platform`]: free space and interface adapters.

mod connect;
pub mod events;
mod pairing;
mod pins;
pub mod platform;
mod receive;
mod registry;
mod router;
mod send;
mod session;

use std::collections::HashMap;
use std::future::Future;
use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use tokio::sync::{mpsc, watch};

pub use events::{
    DiscoveryPath, FileRoot, HostEvent, PairingPromptKind, Secret, TransferDirection,
};
pub use send::{TransferHandle, TransferSummary};

use crate::discovery::{BeaconSchedule, DialLimiter, Discovery, DiscoveryConfig, InterfaceSource};
use crate::identity::{DeviceId, DeviceIdentity};
use crate::net::connection::{short, CloseReason, DESKTOP_KEEPALIVE, MOBILE_KEEPALIVE};
use crate::net::{Server, TlsAcceptor};
use crate::pin_store::{Grants, PinStore, PIN_FILE};
use crate::session::types::{Clipboard, ClipboardKind, DeviceType, Frame, Ping};
use crate::session::{AddDeviceWindow, Limits, PairRequestLimiter, QrOffer};
use crate::tls;
use crate::transfer::FreeSpace;
use crate::{Error, Result};

use pins::Pins;
use registry::Registry;

/// Keep-alive profile announced in `hello.keepAlive` (protocol.md 3.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
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

/// How to start a [`Host`].
pub struct HostConfig {
    /// Identity, pins and transfer index; created 0700 (SR-28).
    pub data_dir: PathBuf,
    /// Where received files land; created 0700 (SR-23).
    pub receive_dir: PathBuf,
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
            receive_dir: receive_dir.into(),
            port: crate::constants::TCP_PORT,
            device_name: device_name.into(),
            device_type: DeviceType::Desktop,
            keep_alive_profile: KeepAliveProfile::Desktop,
            interfaces: Box::new(platform::SystemInterfaces),
            bind_ip: IpAddr::V6(Ipv6Addr::UNSPECIFIED),
            discovery: true,
        }
    }
}

impl std::fmt::Debug for HostConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostConfig")
            .field("data_dir", &self.data_dir)
            .field("receive_dir", &self.receive_dir)
            .field("port", &self.port)
            .field("bind_ip", &self.bind_ip)
            .field("discovery", &self.discovery)
            .finish_non_exhaustive()
    }
}

/// A paired peer as the UI lists it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
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
}

/// A device seen in "Add device" mode (protocol.md 8.3 rule 1): listed, never dialled
/// until the user picks it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct AddDeviceCandidate {
    /// Sanitised advertised name.
    pub name: String,
    /// Device type token.
    pub device_type: String,
    /// Address to pass to [`Host::pair_with`].
    pub addr: SocketAddr,
}

/// Per-peer reachability notes for the 10 s discovery diagnostic.
#[derive(Debug, Default, Clone)]
pub(crate) struct Reach {
    mdns: bool,
    beacon: bool,
    discovered: Vec<SocketAddr>,
    last_dial_error: Option<String>,
}

/// Shared state of a running host.
pub(crate) struct Inner {
    pub(crate) identity: Arc<DeviceIdentity>,
    pub(crate) local_id: DeviceId,
    pub(crate) local_addr: SocketAddr,
    pub(crate) bind_ip: IpAddr,
    pub(crate) data_dir: PathBuf,
    pub(crate) receive_dir: PathBuf,
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
    pub(crate) outgoing: Mutex<HashMap<String, send::Outgoing>>,
    pub(crate) discovery: Mutex<Option<Discovery>>,
    pub(crate) dial_limiter: Mutex<DialLimiter>,
    pub(crate) dial_locks: Mutex<HashMap<DeviceId, Arc<tokio::sync::Mutex<()>>>>,
    pub(crate) reach: Mutex<HashMap<DeviceId, Reach>>,
    pub(crate) clipboard_echo: Mutex<ClipboardEcho>,
    pub(crate) free_space: Arc<dyn FreeSpace>,
    pub(crate) interfaces: Arc<dyn InterfaceSource>,
    shutdown: watch::Sender<bool>,
    task_guard: Arc<()>,
    server_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
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

impl Inner {
    /// Queues an event for the UI. If the UI is not reading, events are dropped rather
    /// than queued without bound.
    pub(crate) fn emit(&self, ev: HostEvent) {
        if self.events.try_send(ev).is_err() {
            tracing::debug!("host event dropped: channel full or closed");
        }
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

    /// Closes the "Add device" window (success, user, or expiry).
    pub(crate) fn close_window(&self) {
        lock(&self.window).close();
        self.window_flag.store(false, Ordering::SeqCst);
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
    /// unfinished incoming transfers (protocol.md 7.6), starts discovery and the last-known
    /// dial loop. Returns the host and its event stream.
    #[allow(clippy::too_many_lines, reason = "start-up is one linear sequence")]
    pub async fn start(config: HostConfig) -> Result<(Self, mpsc::Receiver<HostEvent>)> {
        let data_dir = config.data_dir.clone();
        let identity = {
            let dir = data_dir.clone();
            tokio::task::spawn_blocking(move || DeviceIdentity::load_or_create(&dir))
                .await
                .map_err(|e| Error::Io(std::io::Error::other(e)))??
        };
        let store = {
            let path = data_dir.join(PIN_FILE);
            tokio::task::spawn_blocking(move || PinStore::open(path))
                .await
                .map_err(|e| Error::Io(std::io::Error::other(e)))??
        };
        let identity = Arc::new(identity);
        let limits = Limits::default();
        let server = Server::bind(SocketAddr::new(config.bind_ip, config.port), limits.clone())?;
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
            receive_dir: config.receive_dir,
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
            outgoing: Mutex::new(HashMap::new()),
            discovery: Mutex::new(None),
            dial_limiter: Mutex::new(DialLimiter::new()),
            dial_locks: Mutex::new(HashMap::new()),
            reach: Mutex::new(HashMap::new()),
            clipboard_echo: Mutex::new(ClipboardEcho::default()),
            free_space: Arc::new(platform::VolumeSpace),
            interfaces: Arc::clone(&interfaces),
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
        let server_task = tokio::spawn(server.run(acceptor, router, inner.shutdown_rx()));
        *lock(&inner.server_task) = Some(server_task);

        receive::load_persisted(&inner).await;

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
    pub async fn shutdown(self) {
        let inner = self.inner;
        inner.shutdown.send_replace(true);
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
    /// [`HostEvent::Paired`] or [`HostEvent::PairingFailed`].
    pub async fn pair_with(&self, addr: SocketAddr) -> Result<()> {
        pairing::pair_with(&self.inner, addr).await
    }

    /// The user's answer to the current [`HostEvent::PairingPrompt`]: accept ("Pair",
    /// "Add") or reject.
    pub fn pairing_respond(&self, accept: bool) {
        pairing::respond(&self.inner, accept);
    }

    /// Opens the window and returns a QR pairing URI valid for 120 s (protocol.md 6.3,
    /// SR-9). The URI holds a one-time secret: show it, never log it.
    pub fn show_qr(&self) -> Result<String> {
        pairing::show_qr(&self.inner)
    }

    /// Pairs by a scanned QR URI as the scanner (protocol.md 6.3).
    pub async fn scan_qr(&self, uri: &str) -> Result<()> {
        pairing::scan_qr(&self.inner, uri).await
    }

    /// Sends files and folders to a paired peer (protocol.md 7): builds the manifest, offers
    /// it, streams up to 4 files plus the bundle concurrently, and resumes after drops.
    pub async fn send(&self, peer: &DeviceId, paths: Vec<PathBuf>) -> Result<TransferHandle> {
        send::start(&self.inner, *peer, paths).await
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

    /// Pushes clipboard text to a peer (protocol.md 5.7): needs the `clipboardOut` grant,
    /// and is skipped when it would echo the last clip applied from a peer or the last one
    /// sent. Returns whether it was sent.
    pub fn send_clipboard(&self, peer: &DeviceId, text: &str, sensitive: bool) -> Result<bool> {
        let pin = self.inner.pins.get(peer).ok_or(Error::NotPaired)?;
        if !pin.grants.clipboard_out {
            return Err(Error::Forbidden("clipboardOut grant is off"));
        }
        let hash = crate::session::types::clipboard_text_hash(text);
        {
            let mut echo = lock(&self.inner.clipboard_echo);
            if echo.last_applied.as_deref() == Some(&hash)
                || echo.last_sent.as_deref() == Some(&hash)
            {
                return Ok(false);
            }
            echo.last_sent = Some(hash.clone());
        }
        // TODO(SR-24): text over 32 KiB goes as a blob (protocol.md 5.15), Phase 2/3.
        if text.len() > crate::session::types::CLIPBOARD_INLINE_MAX {
            return Err(Error::TooLarge(
                "clipboard text over 32 KiB needs blobs".into(),
            ));
        }
        let s = self.inner.registry.session(peer).ok_or(Error::NotFound)?;
        s.send(Frame::Clipboard(Clipboard {
            kind: ClipboardKind::Text,
            mime: "text/plain;charset=utf-8".into(),
            hash,
            text: Some(text.to_owned()),
            blob: None,
            sensitive,
        }));
        Ok(true)
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

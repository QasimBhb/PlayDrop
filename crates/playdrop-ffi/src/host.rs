//! The host object the app holds: owns the tokio runtime and the core `Host`, pumps events
//! to the foreign listener, and exposes every frozen `Host` method (docs/core-api.md
//! section 4) with FFI-friendly arguments.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use playdrop_core::host::{Host, HostEvent as CoreEvent};
use tokio::runtime::Runtime;
use tokio::sync::mpsc;

use crate::error::PlayDropError;
use crate::traits::HostListener;
use crate::types::{
    parse_addr, parse_peer, AddDeviceCandidate, ClipboardPush, Diagnostics, Grants, HostConfig,
    InputEvent, MediaAction, MediaState, OutboxItem, PeerSummary, PresenceState, SendSource,
    TransferSnapshot,
};

/// Worker threads for the core runtime. *Choice:* two is enough for one connection's TLS
/// and hashing plus the event pump; the transfer engine is I/O bound on a phone.
const WORKER_THREADS: usize = 2;

/// How long `shutdown` waits for the core.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

/// A running host. One per process; create it from the foreground service, keep it for
/// the service's life, call `shutdown` from `onDestroy`.
///
/// Every synchronous method enters the runtime first (`Runtime::enter`): the core spawns
/// tasks from methods such as `pairing_respond` and `set_battery`, and `tokio::spawn`
/// panics on a thread with no runtime context, which is what a JNA caller thread is.
#[derive(uniffi::Object)]
pub struct PlayDropHost {
    rt: Runtime,
    host: Arc<Host>,
}

impl PlayDropHost {
    /// Runs `f`'s future on the core runtime and awaits it from the caller's executor.
    fn run<F, T>(&self, fut: F) -> impl Future<Output = Result<T, PlayDropError>> + Send
    where
        F: Future<Output = Result<T, playdrop_core::Error>> + Send + 'static,
        T: Send + 'static,
    {
        let handle = self.rt.spawn(fut);
        async move {
            match handle.await {
                Ok(r) => r.map_err(PlayDropError::from),
                Err(e) => Err(PlayDropError::other(format!("core task failed: {e}"))),
            }
        }
    }
}

#[uniffi::export]
#[allow(
    clippy::needless_pass_by_value,
    reason = "exported methods take owned strings; UniFFI has no borrowed string argument"
)]
impl PlayDropHost {
    /// Starts the host: loads or creates the identity and pins under `config.data_dir`,
    /// binds the port exclusively (`PortHeld` if taken), starts discovery and reconnects
    /// to paired peers. Blocks for the start-up (well under a second); call it off the
    /// main thread. Events start flowing to `listener` before this returns.
    #[uniffi::constructor]
    pub fn start(
        config: HostConfig,
        listener: Arc<dyn HostListener>,
    ) -> Result<Arc<Self>, PlayDropError> {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(WORKER_THREADS)
            .thread_name("playdrop-core")
            .enable_all()
            .build()
            .map_err(|e| PlayDropError::other(format!("runtime: {e}")))?;
        let core_cfg = config.into_core()?;
        let (host, events) = rt.block_on(Host::start(core_cfg))?;
        rt.spawn_blocking(move || pump(events, &*listener));
        Ok(Arc::new(Self {
            rt,
            host: Arc::new(host),
        }))
    }

    /// Stops everything. Idempotent. Unfinished incoming transfers resume after the next
    /// start.
    pub async fn shutdown(&self) {
        let host = Arc::clone(&self.host);
        let handle = self.rt.spawn(async move {
            let _ = tokio::time::timeout(SHUTDOWN_TIMEOUT, host.shutdown()).await;
        });
        let _ = handle.await;
    }

    /// Our hex Device ID.
    pub fn device_id(&self) -> String {
        let _rt = self.rt.enter();
        self.host.device_id().to_hex()
    }

    /// Our Short ID (SR-8).
    pub fn short_id(&self) -> String {
        let _rt = self.rt.enter();
        self.host.short_id()
    }

    /// Bound address as `ip:port`.
    pub fn local_addr(&self) -> String {
        let _rt = self.rt.enter();
        self.host.local_addr().to_string()
    }

    /// Paired peers.
    pub fn peers(&self) -> Vec<PeerSummary> {
        let _rt = self.rt.enter();
        self.host.peers().into_iter().map(Into::into).collect()
    }

    /// Finds a paired peer by hex ID, Short ID (prefix, hyphens optional) or exact label.
    pub fn resolve_peer(&self, query: String) -> Option<String> {
        let _rt = self.rt.enter();
        self.host.resolve_peer(&query).map(|d| d.to_hex())
    }

    /// True if a session with the peer is up.
    pub fn is_online(&self, peer: String) -> bool {
        let _rt = self.rt.enter();
        parse_peer(&peer).is_ok_and(|p| self.host.is_online(&p))
    }

    /// Dials a paired peer now from its last-known addresses.
    pub async fn connect(&self, peer: String) -> Result<(), PlayDropError> {
        let peer = parse_peer(&peer)?;
        let host = Arc::clone(&self.host);
        self.run(async move { host.connect(&peer).await }).await
    }

    /// Grants of a paired peer.
    pub fn grants(&self, peer: String) -> Option<Grants> {
        let _rt = self.rt.enter();
        parse_peer(&peer)
            .ok()
            .and_then(|p| self.host.grants(&p))
            .map(Into::into)
    }

    /// Replaces a peer's grants. Only the local UI may call this (SR-14).
    pub async fn set_grants(&self, peer: String, grants: Grants) -> Result<(), PlayDropError> {
        let peer = parse_peer(&peer)?;
        let host = Arc::clone(&self.host);
        self.run(async move { host.set_grants(&peer, grants.into()).await })
            .await
    }

    /// Renames a peer locally.
    pub async fn set_label(&self, peer: String, label: String) -> Result<(), PlayDropError> {
        let peer = parse_peer(&peer)?;
        let host = Arc::clone(&self.host);
        self.run(async move { host.set_label(&peer, label).await })
            .await
    }

    /// Unpairs a peer (protocol.md 6.4).
    pub async fn unpair(&self, peer: String) -> Result<(), PlayDropError> {
        let peer = parse_peer(&peer)?;
        let host = Arc::clone(&self.host);
        self.run(async move { host.unpair(&peer).await }).await
    }

    /// Opens the "Add device" window for 120 s (SR-6).
    pub fn open_add_device_window(&self) -> Result<(), PlayDropError> {
        let _rt = self.rt.enter();
        Ok(self.host.open_add_device_window()?)
    }

    /// Closes the window and drops any QR offer.
    pub fn close_add_device_window(&self) {
        let _rt = self.rt.enter();
        self.host.close_add_device_window();
    }

    /// True while the window is open.
    pub fn add_device_window_open(&self) -> bool {
        let _rt = self.rt.enter();
        self.host.add_device_window_open()
    }

    /// Devices seen in their own window, one entry per address.
    pub fn add_device_list(&self) -> Vec<AddDeviceCandidate> {
        let _rt = self.rt.enter();
        self.host
            .add_device_list()
            .into_iter()
            .map(Into::into)
            .collect()
    }

    /// Starts SAS pairing with the device at `addr` (`ip:port`). Returns once the exchange
    /// has started; the prompt and the result arrive as events.
    pub async fn pair_with(&self, addr: String) -> Result<(), PlayDropError> {
        let addr = parse_addr(&addr)?;
        let host = Arc::clone(&self.host);
        self.run(async move { host.pair_with(addr).await }).await
    }

    /// Answers the current pairing prompt.
    pub fn pairing_respond(&self, accept: bool) {
        let _rt = self.rt.enter();
        self.host.pairing_respond(accept);
    }

    /// Opens the window and returns a one-time QR pairing URI valid for 120 s (SR-9).
    /// Render it with `FLAG_SECURE`; never log it.
    pub fn show_qr(&self) -> Result<String, PlayDropError> {
        let _rt = self.rt.enter();
        Ok(self.host.show_qr()?)
    }

    /// Pairs as the scanner of a QR URI.
    pub async fn scan_qr(&self, uri: String) -> Result<(), PlayDropError> {
        let host = Arc::clone(&self.host);
        self.run(async move { host.scan_qr(&uri).await }).await
    }

    /// Sends files to a paired peer. Returns the transfer ID; progress and completion
    /// arrive as events. The send keeps retrying for 10 minutes after a drop.
    pub async fn send(
        &self,
        peer: String,
        sources: Vec<SendSource>,
        note: Option<String>,
    ) -> Result<String, PlayDropError> {
        let peer = parse_peer(&peer)?;
        let host = Arc::clone(&self.host);
        let sources = sources.into_iter().map(Into::into).collect();
        self.run(async move {
            host.send_sources(&peer, sources, note)
                .await
                .map(|h| h.id().to_owned())
        })
        .await
    }

    /// Cancels a transfer in either direction.
    pub async fn cancel_transfer(&self, id: String) -> Result<(), PlayDropError> {
        let host = Arc::clone(&self.host);
        self.run(async move { host.cancel_transfer(&id).await })
            .await
    }

    /// Live and recently finished transfers, oldest first.
    pub fn transfers(&self) -> Vec<TransferSnapshot> {
        let _rt = self.rt.enter();
        self.host.transfers().into_iter().map(Into::into).collect()
    }

    /// Answers a `TransferOffer` event.
    pub async fn respond_offer(&self, id: String, accept: bool) -> Result<(), PlayDropError> {
        let host = Arc::clone(&self.host);
        self.run(async move { host.respond_offer(&id, accept).await })
            .await
    }

    /// Sends a ping with an optional message.
    pub fn ping(&self, peer: String, message: Option<String>) -> Result<(), PlayDropError> {
        let _rt = self.rt.enter();
        let peer = parse_peer(&peer)?;
        Ok(self.host.ping(&peer, message.as_deref())?)
    }

    /// Rings the peer (`start`) or stops it.
    pub fn find(&self, peer: String, start: bool) -> Result<(), PlayDropError> {
        let _rt = self.rt.enter();
        let peer = parse_peer(&peer)?;
        Ok(self.host.find(&peer, start)?)
    }

    /// Pushes clipboard text to one peer. `false` when the echo rule skipped it.
    pub fn send_clipboard(
        &self,
        peer: String,
        text: String,
        sensitive: bool,
    ) -> Result<bool, PlayDropError> {
        let _rt = self.rt.enter();
        let peer = parse_peer(&peer)?;
        Ok(self.host.send_clipboard(&peer, &text, sensitive)?)
    }

    /// Pushes clipboard text to every online peer with the `clipboardOut` grant.
    pub fn push_clipboard(
        &self,
        text: String,
        sensitive: bool,
    ) -> Result<ClipboardPush, PlayDropError> {
        let _rt = self.rt.enter();
        Ok(self.host.push_clipboard(&text, sensitive)?.into())
    }

    /// Pushes a clipboard image (at most 16 MiB) to every eligible peer.
    pub fn push_clipboard_image(
        &self,
        data: Vec<u8>,
        mime: String,
        sensitive: bool,
    ) -> Result<ClipboardPush, PlayDropError> {
        let _rt = self.rt.enter();
        Ok(self
            .host
            .push_clipboard_image(data, &mime, sensitive)?
            .into())
    }

    /// Reports our battery; pushed to peers now and at every session start (5.8).
    pub fn set_battery(&self, level: u8, charging: bool, low: bool) {
        let _rt = self.rt.enter();
        self.host.set_battery(level, charging, low);
    }

    /// Reports our presence; send `Asleep` before Doze so peers show "asleep" (5.10).
    pub fn set_presence(
        &self,
        state: PresenceState,
        reason: Option<String>,
        expect_back_ms: Option<u64>,
    ) {
        let _rt = self.rt.enter();
        self.host.set_presence(state.into(), reason, expect_back_ms);
    }

    /// Sends a media command to a peer's player (5.9).
    pub fn send_media_command(
        &self,
        peer: String,
        player: String,
        action: MediaAction,
        value: Option<u64>,
    ) -> Result<(), PlayDropError> {
        let _rt = self.rt.enter();
        let peer = parse_peer(&peer)?;
        Ok(self
            .host
            .send_media_command(&peer, &player, action.into(), value)?)
    }

    /// Pushes our media player state to peers, coalesced to 2 per second per player.
    pub fn push_media_state(&self, state: MediaState) {
        let _rt = self.rt.enter();
        self.host.push_media_state(state.into());
    }

    /// Sends one remote input frame to a peer (protocol.md 5.12). Open with
    /// `Session { start: true }` and close with `start: false`. Fails with `NotFound`
    /// without a session and `Forbidden` when the peer does not list the needed
    /// `input.*` capability (see `PeerSummary.capabilities`). Pointer moves and scrolls
    /// are coalesced to one frame per 8 ms, so send them as fast as the touch surface
    /// reports. A peer with remote input off for us answers with `PeerRefused`.
    pub fn send_input(&self, peer: String, input: InputEvent) -> Result<(), PlayDropError> {
        let _rt = self.rt.enter();
        let peer = parse_peer(&peer)?;
        Ok(self.host.send_input(&peer, input.into())?)
    }

    /// Peers holding an open input session on this host (hosts with `acceptsInput`).
    pub fn input_sessions(&self) -> Vec<String> {
        let _rt = self.rt.enter();
        self.host
            .input_sessions()
            .iter()
            .map(playdrop_core::identity::DeviceId::to_hex)
            .collect()
    }

    /// The kill switch (SR-25): ends every input session this host is the target of and
    /// tells the peers. Each ended session arrives as a final `Input` event with
    /// `Session { start: false }`. Returns the peers that had one.
    pub fn stop_input_sessions(&self) -> Vec<String> {
        let _rt = self.rt.enter();
        self.host
            .stop_input_sessions()
            .iter()
            .map(playdrop_core::identity::DeviceId::to_hex)
            .collect()
    }

    /// Queues files for a peer that cannot listen (an iPhone); returns the item ID.
    pub async fn queue_outbox(
        &self,
        peer: String,
        sources: Vec<SendSource>,
        note: Option<String>,
    ) -> Result<String, PlayDropError> {
        let peer = parse_peer(&peer)?;
        let host = Arc::clone(&self.host);
        let sources = sources.into_iter().map(Into::into).collect();
        self.run(async move { host.queue_outbox(&peer, sources, note).await })
            .await
    }

    /// Items this host holds.
    pub fn outbox(&self) -> Vec<OutboxItem> {
        let _rt = self.rt.enter();
        self.host.outbox().into_iter().map(Into::into).collect()
    }

    /// Removes a queued item.
    pub async fn remove_outbox(&self, id: String) -> Result<(), PlayDropError> {
        let host = Arc::clone(&self.host);
        self.run(async move { host.remove_outbox(&id).await }).await
    }

    /// Pulls every item the peer holds for us; returns the transfer IDs started.
    pub async fn pull_outbox(&self, peer: String) -> Result<Vec<String>, PlayDropError> {
        let peer = parse_peer(&peer)?;
        let host = Arc::clone(&self.host);
        self.run(async move { host.pull_outbox(&peer).await }).await
    }

    /// The on-demand diagnostic snapshot.
    pub fn diagnose(&self) -> Diagnostics {
        let _rt = self.rt.enter();
        self.host.diagnose().into()
    }

    /// The OS reported a network change: rejoin groups, re-advertise, dial now.
    pub fn network_changed(&self) {
        let _rt = self.rt.enter();
        self.host.network_changed();
    }
}

/// Delivers core events to the foreign listener in order, on one blocking thread, until
/// the core drops its sender (shutdown).
fn pump(mut events: mpsc::Receiver<CoreEvent>, listener: &dyn HostListener) {
    while let Some(ev) = events.blocking_recv() {
        if let Err(e) = listener.on_event(ev.into()) {
            tracing::warn!(error = %e, "host listener failed on an event");
        }
    }
    tracing::debug!("event pump ended");
}

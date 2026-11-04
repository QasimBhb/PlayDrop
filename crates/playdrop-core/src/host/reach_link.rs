//! The host's side of reach across networks (protocol.md 12, SR-35 to SR-40): the accept gate,
//! the dial after every LAN candidate failed, the `reach` frame, the `remote` grant and the
//! honest labels. The bytes run through the same TLS and HTTP/2 code as TCP; this file only
//! decides who gets that far.

use std::collections::{HashMap, HashSet};
#[cfg(feature = "iroh-test")]
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use iroh::endpoint::{Incoming, VarInt};
use iroh::{Endpoint, EndpointId, RelayUrl};
use tokio::sync::{mpsc, watch, OwnedSemaphorePermit, Semaphore};
use tokio::time::Instant;

use super::connect::adopt;
use super::session::Session;
use super::{lock, Inner};
use crate::identity::DeviceId;
use crate::net::client::establish_over;
use crate::net::connection::short;
use crate::net::{Alternatives, CloseReason, Connection, DialOptions, Direction};
use crate::pin_store::Pin;
use crate::reach::store::{ReachStore, SharedStore, REACH_FILE};
use crate::reach::transport::{self, ReachIncoming, ReachIo};
use crate::reach::wire::{self, ReachInfo};
use crate::reach::{self, ReachConfig, ReachPath, ReachPathKind, DIAL_TIMEOUT};
use crate::session::types::{Capability, Frame, ReachFrame};
use crate::session::{ErrorCode, Limits};
use crate::tls;
use crate::{Error, Result};

/// How long one accept may take from the first QUIC packet to the first stream. *Choice:* the
/// TLS handshake timeout of SR-15, for the same reason.
const ACCEPT_TIMEOUT: Duration = Duration::from_secs(10);

/// Most `reach` frames per peer per minute that may change what is stored. *Choice:* a frame
/// costs a disk write and an answer, and an honest peer sends one per grant or relay change.
const MAX_CHANGES_PER_MINUTE: u32 = 10;

/// How long a revoke leaves for the frame that takes the endpoint back to get out, before the
/// reach connections close. *Choice:* 1 s, the bound SR-14 sets for an unpair: on a slow relay
/// with a send running the frame waits behind what is queued, and the next dial is refused anyway.
const REVOKE_GRACE: Duration = Duration::from_secs(1);

/// The longest wait between reach dials of a peer whose gate keeps refusing this host. *Choice:*
/// one attempt an hour tells the relay almost nothing, and a grant turned back on while this host
/// is away still works within that hour; a meeting on a LAN resets it.
const MAX_REFUSED_WAIT: Duration = Duration::from_secs(3600);

/// The result of a reach dial under way: `None` while it runs, then what it adopted, if anything.
type Flight = Option<Option<Arc<Connection>>>;

/// What a host with a relay named keeps.
pub(crate) struct ReachLink {
    endpoint: Endpoint,
    relay: RelayUrl,
    retry_every: Duration,
    #[cfg(feature = "iroh-test")]
    never_dial: bool,
    /// [`REVOKE_GRACE`], or a test's own.
    revoke_grace: Duration,
    /// The reach dial under way per peer: at most one.
    flights: Mutex<HashMap<DeviceId, watch::Receiver<Flight>>>,
    /// How many dials in a row each peer's gate refused, for the wait before the next.
    refused_by: Mutex<HashMap<DeviceId, u32>>,
    /// The last reach dial per peer ([`ReachConfig::retry_every`]).
    last_dial: Mutex<HashMap<DeviceId, Instant>>,
    /// Reach dials started (R4).
    #[cfg(feature = "iroh-test")]
    dials: AtomicU64,
    /// Connections closed by the gate before any TLS (SR-36).
    #[cfg(feature = "iroh-test")]
    refused: AtomicU64,
    /// Connections that passed the gate and went on to the inner TLS.
    #[cfg(feature = "iroh-test")]
    admitted: AtomicU64,
}

impl std::fmt::Debug for ReachLink {
    /// Leaves out the relay and the endpoint (SR-40).
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReachLink").finish_non_exhaustive()
    }
}

impl ReachLink {
    /// This host's Endpoint ID.
    pub(crate) fn id(&self) -> EndpointId {
        self.endpoint.id()
    }

    /// How long after the last dial of `peer` the next may go.
    fn dial_wait(&self, peer: &DeviceId) -> Duration {
        let refused = lock(&self.refused_by).get(peer).copied().unwrap_or(0);
        self.retry_every
            .saturating_mul(2u32.saturating_pow(refused))
            .min(MAX_REFUSED_WAIT.max(self.retry_every))
    }
}

/// Stored changes per peer in the current minute, with or without a relay named: a frame costs a
/// disk write either way.
#[derive(Debug, Default)]
pub(crate) struct ChangeLimit {
    counts: Mutex<HashMap<DeviceId, (Instant, u32)>>,
    /// Peers whose last frame named an Endpoint ID already taken: they are told about in
    /// diagnostics, as this host then cannot reach them across networks.
    conflicts: Mutex<HashSet<DeviceId>>,
}

impl ChangeLimit {
    /// Counts one change for `peer`; false if this minute has had its share.
    fn allow(&self, peer: DeviceId) -> bool {
        let now = Instant::now();
        let mut changes = lock(&self.counts);
        let e = changes.entry(peer).or_insert((now, 0));
        if now.saturating_duration_since(e.0) >= Duration::from_secs(60) {
            *e = (now, 0);
        }
        e.1 += 1;
        e.1 <= MAX_CHANGES_PER_MINUTE
    }

    fn forget(&self, peer: &DeviceId) {
        lock(&self.counts).remove(peer);
        self.clear_conflict(peer);
    }

    fn note_conflict(&self, peer: DeviceId) {
        lock(&self.conflicts).insert(peer);
    }

    fn clear_conflict(&self, peer: &DeviceId) {
        lock(&self.conflicts).remove(peer);
    }
}

/// The reach state a host always has with the feature on: the grants exist with or without a
/// relay.
pub(crate) fn open_store(
    inner_data_dir: &std::path::Path,
    sealer: crate::identity::sealing::Sealer,
) -> SharedStore {
    SharedStore::new(ReachStore::open(inner_data_dir.join(REACH_FILE), sealer))
}

/// Binds the endpoint if a relay is named. No relay named means no endpoint and nothing
/// contacted (SR-35).
pub(crate) async fn start(
    identity: &crate::identity::DeviceIdentity,
    config: Option<&ReachConfig>,
) -> Result<Option<Arc<ReachLink>>> {
    let Some(config) = config else {
        return Ok(None);
    };
    let relay = transport::parse_named_relay(&config.relay)?;
    let endpoint = transport::bind(identity, config, relay.clone()).await?;
    Ok(Some(Arc::new(ReachLink {
        endpoint,
        relay,
        retry_every: config.retry_every,
        #[cfg(feature = "iroh-test")]
        never_dial: config.never_dial,
        #[cfg(feature = "iroh-test")]
        revoke_grace: config.revoke_grace.unwrap_or(REVOKE_GRACE),
        #[cfg(not(feature = "iroh-test"))]
        revoke_grace: REVOKE_GRACE,
        flights: Mutex::new(HashMap::new()),
        refused_by: Mutex::new(HashMap::new()),
        last_dial: Mutex::new(HashMap::new()),
        #[cfg(feature = "iroh-test")]
        dials: AtomicU64::new(0),
        #[cfg(feature = "iroh-test")]
        refused: AtomicU64::new(0),
        #[cfg(feature = "iroh-test")]
        admitted: AtomicU64::new(0),
    })))
}

/// The capability string as this build carries it.
pub(crate) fn capability() -> Capability {
    Capability::Unknown(reach::CAPABILITY.to_owned())
}

/// Accepts until shutdown. Each incoming connection goes through [`accept_one`] on its own task,
/// at most [`Limits::max_pending_handshakes`] at once; beyond that QUIC refuses it (SR-15,
/// SR-38).
pub(crate) async fn accept_loop(
    inner: Arc<Inner>,
    link: Arc<ReachLink>,
    tx: mpsc::Sender<ReachIncoming>,
) {
    let gate = Arc::new(Semaphore::new(Limits::default().max_pending_handshakes));
    while let Some(incoming) = link.endpoint.accept().await {
        let Ok(permit) = Arc::clone(&gate).try_acquire_owned() else {
            tracing::debug!("too many reach handshakes in progress; refused");
            incoming.refuse();
            continue;
        };
        let (i2, l2, t2) = (Arc::clone(&inner), Arc::clone(&link), tx.clone());
        inner.spawn(accept_one(i2, l2, incoming, t2, permit));
    }
}

/// Who may come in over `endpoint`: pinned devices that hold the grant and stored this ID in
/// this pairing. Empty means closed before TLS (SR-36).
fn allowed_devices(inner: &Inner, endpoint: &EndpointId) -> Vec<DeviceId> {
    let hex = wire::endpoint_hex(endpoint);
    let candidates = inner.reach_store.read(|s| s.granted_with_endpoint(&hex));
    candidates
        .into_iter()
        .filter(|(id, paired_at)| {
            inner
                .pins
                .get(id)
                .is_some_and(|p| p.paired_at == *paired_at)
        })
        .map(|(id, _)| id)
        .collect()
}

/// One incoming connection: the gate, then the one stream, then the server's TLS path.
async fn accept_one(
    inner: Arc<Inner>,
    #[cfg_attr(not(feature = "iroh-test"), allow(unused_variables))] link: Arc<ReachLink>,
    incoming: Incoming,
    tx: mpsc::Sender<ReachIncoming>,
    _permit: OwnedSemaphorePermit,
) {
    let Ok(accepting) = incoming.accept() else {
        return;
    };
    let conn = match tokio::time::timeout(ACCEPT_TIMEOUT, accepting).await {
        Ok(Ok(c)) => c,
        Ok(Err(e)) => {
            tracing::debug!(error = %e, "reach handshake failed");
            return;
        }
        Err(_) => {
            tracing::debug!("reach handshake timed out");
            return;
        }
    };
    // The endpoint offers one ALPN, so QUIC has already refused any other; this is the check
    // that stays true if that ever changes.
    if conn.alpn() != reach::ALPN {
        #[cfg(feature = "iroh-test")]
        link.refused.fetch_add(1, Ordering::Relaxed);
        transport::refuse(&conn);
        return;
    }
    let remote = conn.remote_id();
    if allowed_devices(&inner, &remote).is_empty() {
        tracing::debug!(endpoint = %remote.fmt_short(), "reach connection refused before TLS");
        #[cfg(feature = "iroh-test")]
        link.refused.fetch_add(1, Ordering::Relaxed);
        transport::refuse(&conn);
        return;
    }
    #[cfg(feature = "iroh-test")]
    link.admitted.fetch_add(1, Ordering::Relaxed);
    // An older connection from this endpoint stays until this one is proven: after the inner TLS
    // and its first request, `Registry::attach` replaces it by device (protocol.md 3.3). Closing
    // it here would let anyone holding this host's Endpoint ID, or a peer that names it in a
    // `reach` frame so that a third host dials it, cut a working session.
    let stream = match tokio::time::timeout(ACCEPT_TIMEOUT, conn.accept_bi()).await {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => {
            tracing::debug!(error = %e, "reach stream not opened");
            return;
        }
        Err(_) => {
            tracing::debug!("reach stream not opened in time");
            transport::refuse(&conn);
            return;
        }
    };
    // Now there is no more credit at all, even after the first stream ends (SR-38).
    conn.set_max_concurrent_bi_streams(VarInt::from_u32(0));
    let (send, recv) = stream;
    let delivered = tx
        .send(ReachIncoming {
            quic: conn.weak_handle(),
            io: ReachIo::new(send, recv, conn),
            remote: reach::synthetic_addr(&remote),
            admit: {
                let inner = Arc::clone(&inner);
                Arc::new(move |device: &DeviceId| allowed_devices(&inner, &remote).contains(device))
            },
        })
        .await;
    if delivered.is_err() {
        tracing::debug!("reach stream dropped: the server has stopped");
    }
}

/// Where to dial `pin` over reach, if everything allows it: a relay named here, the grant on,
/// a stored ID, and that the peer named the same relay. *Choice:* only the relay this user named
/// is ever contacted (SR-35); a paired peer must not be able to point this host at a URL of its
/// choosing, so two devices on different relays do not reach each other.
fn dial_target(inner: &Inner, link: &ReachLink, pin: &Pin) -> Option<(EndpointId, RelayUrl)> {
    let (endpoint, relay) = inner.reach_store.read(|s| {
        let e = s.entry(pin).filter(|e| e.remote)?;
        Some((e.endpoint.clone()?, e.relay.clone()?))
    })?;
    let endpoint = wire::parse_endpoint(&endpoint).ok()?;
    let relay = wire::parse_relay(&relay).ok()?;
    if relay != link.relay {
        tracing::debug!(peer = %short(&pin.device_id), "peer names another relay; not dialled");
        return None;
    }
    Some((endpoint, relay))
}

/// The dial of protocol.md 12: called only after every LAN candidate failed, without the dial
/// lock. One dial per peer runs at a time, on its own task, and every caller waits for its result
/// (at most [`DIAL_TIMEOUT`] and a little), so callers cannot pile up behind a slow dial and LAN
/// dials go on meanwhile. `None` when reach does not apply or the dial failed; the caller reports
/// the LAN failure.
pub(crate) async fn dial_after_lan(inner: &Arc<Inner>, pin: &Pin) -> Option<Arc<Connection>> {
    let link = inner.reach_link.as_ref()?;
    #[cfg(feature = "iroh-test")]
    if link.never_dial {
        return None;
    }
    let (endpoint, relay) = dial_target(inner, link, pin)?;
    let mut result = {
        let mut flights = lock(&link.flights);
        if let Some(running) = flights.get(&pin.device_id) {
            running.clone()
        } else {
            let now = Instant::now();
            let wait = link.dial_wait(&pin.device_id);
            let mut last = lock(&link.last_dial);
            if last
                .get(&pin.device_id)
                .is_some_and(|t| now.saturating_duration_since(*t) < wait)
            {
                return None;
            }
            last.insert(pin.device_id, now);
            drop(last);
            let (tx, rx) = watch::channel(None);
            flights.insert(pin.device_id, rx.clone());
            inner.spawn(flight(
                Arc::clone(inner),
                Arc::clone(link),
                pin.clone(),
                endpoint,
                relay,
                tx,
            ));
            rx
        }
    };
    let done = result.wait_for(Option::is_some).await.ok()?;
    done.clone().flatten()
}

/// One reach dial, then its adoption. *Choice:* not under the dial lock: with a silent LAN address
/// the next LAN dial holds that lock for 5 s, and a working reach connection would wait for it
/// (measured, online at 10 s instead of 5). [`adopt`] attaches it only if no outbound connection
/// exists, in one step, and a LAN path that comes up later replaces it, so a peer never ends up
/// with both.
async fn flight(
    inner: Arc<Inner>,
    link: Arc<ReachLink>,
    pin: Pin,
    endpoint: EndpointId,
    relay: RelayUrl,
    tx: watch::Sender<Flight>,
) {
    let peer = pin.device_id;
    let adopted = match dial_once(&inner, &link, &pin, endpoint, relay).await {
        Some(conn) => adopt(&inner, peer, &pin, conn, Alternatives::default())
            .await
            .ok(),
        None => None,
    };
    lock(&link.flights).remove(&peer);
    tx.send_replace(Some(adopted));
}

/// The handshakes of one reach dial, at most [`DIAL_TIMEOUT`].
async fn dial_once(
    inner: &Inner,
    link: &ReachLink,
    pin: &Pin,
    endpoint: EndpointId,
    relay: RelayUrl,
) -> Option<Arc<Connection>> {
    #[cfg(feature = "iroh-test")]
    link.dials.fetch_add(1, Ordering::Relaxed);
    let opts = DialOptions::new(
        tls::build_client_config(&inner.identity, pin.spki_hash),
        Some(pin.spki_hash),
    );
    let remote = reach::synthetic_addr(&endpoint);
    let mut quic_seen = None;
    let dialled = tokio::time::timeout(DIAL_TIMEOUT, async {
        let quic = transport::connect(&link.endpoint, endpoint, relay)
            .await
            .map_err(|e| e.to_string())?;
        quic_seen = Some(quic.clone());
        let io = transport::open_stream(&quic)
            .await
            .map_err(|e| e.to_string())?;
        establish_over(io, remote, quic.weak_handle(), &opts).await
    })
    .await;
    // Something answered at that ID and it was not this peer letting us in: its gate said no, or
    // the key behind the ID is not the device's, as with an ID a peer named that belongs to
    // someone else. Ask less and less often (protocol.md 12). A dial that reached nobody is not
    // counted: that is what any peer looks like while it is away.
    if !matches!(dialled, Ok(Ok(_))) && quic_seen.is_some() {
        let mut refused = lock(&link.refused_by);
        let n = refused.entry(pin.device_id).or_insert(0);
        *n = n.saturating_add(1);
        let by_gate = quic_seen.as_ref().is_some_and(transport::was_refused);
        tracing::debug!(peer = %short(&pin.device_id), refusals = *n, by_gate, "reach dial answered but not let in");
    }
    match dialled {
        Ok(Ok(conn)) => Some(conn),
        Ok(Err(e)) => {
            tracing::debug!(peer = %short(&pin.device_id), error = %e, "reach dial failed");
            None
        }
        Err(_) => {
            tracing::debug!(peer = %short(&pin.device_id), "reach dial timed out");
            None
        }
    }
}

/// Lets the next reach dial go at once (the OS reported a network change).
pub(crate) fn network_changed(inner: &Arc<Inner>) {
    let Some(link) = inner.reach_link.clone() else {
        return;
    };
    lock(&link.last_dial).clear();
    inner.spawn(async move {
        link.endpoint.network_change().await;
    });
}

/// A session was placed: tell a peer we granted, or take back what we told one we no longer
/// grant (protocol.md 12). Nothing is said to a peer without the grant that was never told.
pub(crate) fn on_session_placed(inner: &Arc<Inner>, sess: &Arc<Session>) {
    let peer = sess.peer();
    let Some(pin) = inner.pins.get(&peer) else {
        return;
    };
    let (granted, told) = inner
        .reach_store
        .read(|s| (s.granted(&pin), s.entry(&pin).is_some_and(|e| e.told)));
    if let Some(link) = inner.reach_link.as_ref() {
        // We hear from the peer again, whichever way: whatever its gate said is old.
        lock(&link.refused_by).remove(&peer);
    }
    if granted {
        tell(inner, sess, &pin);
    } else if told {
        // Repeated after a revoke, whose frame can be lost with the connection, and marked as
        // done only if it went out.
        if take_back(sess) {
            note_told(inner, &pin, false);
        }
    }
    // A session on a LAN path: a reach connection we dialled has nothing left to carry
    // (protocol.md 12). Requests still on it finish while it drains.
    if !reach::is_reach_addr(sess.conn.remote_addr()) {
        for c in inner.registry.connections_of(&peer) {
            if reach::is_reach_addr(c.remote_addr())
                && c.direction() == Direction::Outbound
                && !c.is_draining()
            {
                tracing::debug!(peer = %short(&peer), "session on a LAN path; reach connection drains");
                c.drain();
            }
        }
    }
}

/// Sends our endpoint and relay. Needs a relay named here, the capability at the peer, and the
/// grant, which the callers checked.
fn tell(inner: &Arc<Inner>, sess: &Arc<Session>, pin: &Pin) {
    let Some(link) = inner.reach_link.as_ref() else {
        return;
    };
    if !sess.peer_lists_in(&capability()) {
        return;
    }
    sess.send_raw(
        Frame::Reach(ReachFrame {
            endpoint: Some(wire::endpoint_hex(&link.id())),
            relay: Some(link.relay.to_string()),
        }),
        None,
    );
    note_told(inner, pin, true);
}

/// Sends the frame with both fields null, if the peer can hear it, and says whether it did. A
/// peer that lists no `reach` capability right now can list it later, with a relay named. Saying
/// it twice does no harm.
fn take_back(sess: &Arc<Session>) -> bool {
    if !sess.peer_lists_in(&capability()) {
        return false;
    }
    sess.send_raw(
        Frame::Reach(ReachFrame {
            endpoint: None,
            relay: None,
        }),
        None,
    );
    true
}

/// True unless `conn` is a reach connection whose peer no longer holds the `remote` grant. A
/// revoke sweeps what is tracked, so whatever is tracked after it has to ask (SR-14).
pub(crate) fn still_allowed(inner: &Inner, conn: &Connection) -> bool {
    if !reach::is_reach_addr(conn.remote_addr()) {
        return true;
    }
    inner
        .pins
        .get(&conn.device_id())
        .is_some_and(|p| inner.reach_store.read(|s| s.granted(&p)))
}

/// The other devices this host holds `endpoint` for, under their current pairings.
fn held_by_another(inner: &Inner, endpoint: &EndpointId, device: &DeviceId) -> bool {
    let hex = wire::endpoint_hex(endpoint);
    let others = inner
        .reach_store
        .read(|s| s.others_with_endpoint(&hex, device));
    others.into_iter().any(|(id, paired_at)| {
        inner
            .pins
            .get(&id)
            .is_some_and(|p| p.paired_at == paired_at)
    })
}

fn note_told(inner: &Arc<Inner>, pin: &Pin, told: bool) {
    let already = inner
        .reach_store
        .read(|s| s.entry(pin).is_some_and(|e| e.told) == told);
    if already {
        return;
    }
    let (i2, pin) = (Arc::clone(inner), pin.clone());
    inner.spawn(async move {
        let stored = i2
            .reach_store
            .update(move |s| {
                s.set_told(&pin, told);
                true
            })
            .await;
        if let Err(e) = stored {
            tracing::debug!(error = %e, "reach state not stored");
        }
    });
}

/// A `reach` frame from an authenticated peer (protocol.md 12). The grant is checked here and
/// not through [`Frame::required_grant`]: the `remote` grant lives in this module's store.
pub(crate) async fn on_frame(
    inner: &Arc<Inner>,
    sess: &Arc<Session>,
    env_seq: u64,
    frame: ReachFrame,
) {
    let peer = sess.peer();
    let Some(pin) = inner.pins.get(&peer) else {
        return;
    };
    let info = match wire::parse_values(frame.endpoint.as_deref(), frame.relay.as_deref()) {
        Ok(i) => i,
        Err(e) => {
            tracing::debug!(peer = %short(&peer), error = %e, "reach frame ignored");
            return;
        }
    };
    let granted = inner.reach_store.read(|s| s.granted(&pin));
    // Forgetting is always safe, and what a peer took back should not linger because our own
    // grant happens to be off. Taking new values needs the grant.
    if !granted && info != ReachInfo::Forget {
        sess.refuse_once("reach", ErrorCode::Forbidden, env_seq);
        return;
    }
    if let ReachInfo::Set { endpoint, relay } = &info {
        // Neither this host's own ID nor one held for another peer: a paired peer must not be
        // able to make this host dial someone else (protocol.md 12).
        let own = reach::secret_key_for(&inner.identity).public();
        if *endpoint == own || held_by_another(inner, endpoint, &peer) {
            tracing::debug!(peer = %short(&peer), "reach frame names an endpoint that is not its own; ignored");
            inner.reach_changes.note_conflict(peer);
            return;
        }
        inner.reach_changes.clear_conflict(&peer);
        // The limit is for writes to disk, so a repeat costs nothing, and it does not hold back a
        // forget: that is always safe and needs a set before it to do anything.
        if inner
            .reach_store
            .read(|s| s.has_values(&pin, endpoint, relay))
        {
            return;
        }
        if !inner.reach_changes.allow(peer) {
            tracing::debug!(peer = %short(&peer), "too many reach changes; frame ignored");
            return;
        }
    }
    let p2 = pin.clone();
    let stored = inner
        .reach_store
        .update(move |s| match &info {
            ReachInfo::Forget => s.set_peer_values(&p2, None),
            ReachInfo::Set { endpoint, relay } => s.set_peer_values(&p2, Some((endpoint, relay))),
        })
        .await;
    match stored {
        // A change is answered with our own frame, so the order in which the two sides turned
        // the grant on does not matter: the first side's frame was refused, and it hears again.
        // It stops by itself: an answer that changes nothing is not answered.
        Ok(true) if granted => tell(inner, sess, &pin),
        Ok(_) => {}
        Err(e) => tracing::debug!(error = %e, "reach values not stored"),
    }
}

/// Turns the `remote` grant on or off, from the local UI only (SR-14). Off holds at once whatever
/// the disk says: the frame goes and the connections close, and a failed save is reported after.
pub(crate) async fn set_grant(inner: &Arc<Inner>, peer: DeviceId, on: bool) -> Result<()> {
    let pin = inner.pins.get(&peer).ok_or(Error::NotPaired)?;
    let p2 = pin.clone();
    let saved = inner
        .reach_store
        .update(move |s| {
            s.set_remote(&p2, on);
            true
        })
        .await;
    if on {
        if let Err(e) = saved {
            let p2 = pin.clone();
            inner
                .reach_store
                .apply_unsaved(move |s| s.set_remote(&p2, false))
                .await;
            return Err(e);
        }
        if let Some(sess) = inner.registry.session(&peer) {
            tell(inner, &sess, &pin);
        }
        return Ok(());
    }
    // `told` stays on until a later session has said this again: this frame can be lost with
    // the connection it goes out on.
    let session = inner.registry.session(&peer);
    if let Some(sess) = &session {
        take_back(sess);
        if reach::is_reach_addr(sess.conn.remote_addr()) {
            sess.end();
        }
    }
    // The grant is what let these in: they do not outlive it.
    let conns: Vec<Arc<Connection>> = inner
        .registry
        .connections_of(&peer)
        .into_iter()
        .filter(|c| reach::is_reach_addr(c.remote_addr()))
        .collect();
    let grace = inner
        .reach_link
        .as_ref()
        .map_or(REVOKE_GRACE, |l| l.revoke_grace);
    inner.spawn(async move {
        tokio::time::sleep(grace).await;
        for c in conns {
            c.close(CloseReason::Dead);
        }
    });
    saved.map(|_| ())
}

/// Forgets a peer at unpair (SR-14). Its connections are closed by the caller.
pub(crate) async fn forget(inner: &Arc<Inner>, peer: DeviceId) {
    let removed = inner.reach_store.update(move |s| s.remove(&peer)).await;
    if let Err(e) = removed {
        tracing::debug!(error = %e, "reach settings not updated at unpair");
    }
    inner.reach_changes.forget(&peer);
    if let Some(link) = inner.reach_link.as_ref() {
        lock(&link.last_dial).remove(&peer);
        lock(&link.refused_by).remove(&peer);
    }
}

/// Every peer currently reached across networks and how (SR-39). What counts is the connection in
/// use: the session's, or, with the session on a LAN path, the one this host's sends take.
pub(crate) fn paths(inner: &Inner) -> Vec<ReachPath> {
    if inner.reach_link.is_none() {
        return Vec::new();
    }
    let pins: Vec<Pin> = inner.pins.read().iter().cloned().collect();
    pins.into_iter()
        .filter_map(|pin| {
            let id = pin.device_id;
            let session = inner.registry.session(&id).map(|s| Arc::clone(&s.conn));
            let in_use = session
                .filter(|c| reach::is_reach_addr(c.remote_addr()))
                .or_else(|| {
                    inner
                        .registry
                        .outbound(&id)
                        .filter(|c| reach::is_reach_addr(c.remote_addr()))
                })?;
            Some(ReachPath {
                short_id: id.short_id(),
                label: pin.label.clone(),
                kind: path_kind(&in_use),
            })
        })
        .collect()
}

/// Lines for diagnostics about what is wrong with reach (T46): a peer whose frame was dropped
/// because the Endpoint ID it named is taken, this host's or another peer's. First to name keeps
/// it, so either this peer is lying or the one that holds the ID is.
pub(crate) fn notes(inner: &Inner) -> Vec<String> {
    let conflicts: Vec<DeviceId> = lock(&inner.reach_changes.conflicts)
        .iter()
        .copied()
        .collect();
    conflicts
        .into_iter()
        .filter_map(|id| inner.pins.get(&id))
        .map(|pin| {
            format!(
                "note: {} named a reach endpoint another device already holds, so it was ignored \
                 and {} cannot be reached across networks. If that is wrong, unpair the device \
                 that should not hold it",
                pin.label, pin.label
            )
        })
        .collect()
}

/// Direct if the path QUIC selected on this connection is an IP path, otherwise the relay: the
/// label errs towards saying the relay can see it.
fn path_kind(conn: &Connection) -> ReachPathKind {
    let direct = conn
        .reach_quic()
        .is_some_and(|q| q.paths().iter().any(|p| p.is_selected() && p.is_ip()));
    if direct {
        ReachPathKind::Direct
    } else {
        ReachPathKind::Relay
    }
}

/// Closes the endpoint at shutdown, so a peer learns at once.
pub(crate) async fn shutdown(inner: &Inner) {
    if let Some(link) = inner.reach_link.as_ref() {
        let _ = tokio::time::timeout(Duration::from_secs(2), link.endpoint.close()).await;
    }
}

/// Counters of the link, for tests.
#[cfg(feature = "iroh-test")]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReachCounters {
    /// Reach dials started.
    pub dials: u64,
    /// Connections closed by the gate before any TLS (SR-36).
    pub refused: u64,
    /// Connections that passed the gate.
    pub admitted: u64,
}

#[cfg(feature = "iroh-test")]
pub(crate) fn counters(inner: &Inner) -> ReachCounters {
    inner
        .reach_link
        .as_ref()
        .map_or_else(ReachCounters::default, |l| ReachCounters {
            dials: l.dials.load(Ordering::Relaxed),
            refused: l.refused.load(Ordering::Relaxed),
            admitted: l.admitted.load(Ordering::Relaxed),
        })
}

/// What a test sees of a peer's stored values.
#[cfg(feature = "iroh-test")]
pub(crate) fn stored(inner: &Inner, peer: &DeviceId) -> Option<(String, String)> {
    let pin = inner.pins.get(peer)?;
    inner.reach_store.read(|s| {
        let e = s.entry(&pin)?;
        Some((e.endpoint.clone()?, e.relay.clone()?))
    })
}

/// Tries to open a second stream on the session's reach connection, for the SR-38 gate: true if
/// the peer gave credit for one.
#[cfg(feature = "iroh-test")]
pub(crate) async fn try_second_stream(inner: &Inner, peer: &DeviceId, wait: Duration) -> bool {
    let Some(conn) = inner.registry.session(peer).map(|s| Arc::clone(&s.conn)) else {
        return false;
    };
    let Some(quic) = conn.reach_quic() else {
        return false;
    };
    matches!(tokio::time::timeout(wait, quic.open_bi()).await, Ok(Ok(_)))
}

/// Sends a `reach` frame with whatever values a test wants, as a hostile peer would.
#[cfg(feature = "iroh-test")]
pub(crate) fn send_frame_for_test(
    inner: &Inner,
    peer: &DeviceId,
    endpoint: Option<String>,
    relay: Option<String>,
) -> bool {
    let Some(sess) = inner.registry.session(peer) else {
        return false;
    };
    sess.send_raw(Frame::Reach(ReachFrame { endpoint, relay }), None);
    true
}

/// Dials `endpoint` expecting a device key it does not have, as this host does when a peer's
/// `reach` frame named someone else's Endpoint ID. Returns once the dial failed.
#[cfg(feature = "iroh-test")]
pub(crate) async fn dial_wrong_key_for_test(inner: &Inner, endpoint: &str) -> bool {
    let Some(link) = inner.reach_link.as_ref() else {
        return false;
    };
    let Ok(id) = wire::parse_endpoint(endpoint) else {
        return false;
    };
    let wrong = [0x5a; 32];
    let opts = DialOptions::new(
        tls::build_client_config(&inner.identity, wrong),
        Some(wrong),
    );
    let dialled = tokio::time::timeout(DIAL_TIMEOUT, async {
        let quic = transport::connect(&link.endpoint, id, link.relay.clone())
            .await
            .map_err(|e| e.to_string())?;
        let io = transport::open_stream(&quic)
            .await
            .map_err(|e| e.to_string())?;
        establish_over(io, reach::synthetic_addr(&id), quic.weak_handle(), &opts).await
    })
    .await;
    matches!(dialled, Ok(Err(_)))
}

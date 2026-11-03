//! Reaching pinned peers: last-known dial at start and on a timer, dials triggered by
//! verified adverts, and the "which layer failed" diagnostic (protocol.md 3.2, 8.3;
//! SR-1; PLAN.md discovery).

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;

use super::diagnostics::DialFailure;
use super::events::{DiscoveryPath, HostEvent};
use super::registry::Placement;
use super::{lock, path, session, Inner};
use crate::discovery::{DiscoveryEvent, Via};
use crate::identity::DeviceId;
use crate::net::connection::{short, Heard, REPLACED_PING_DEADLINE};
use crate::net::{canonical_addr, dial_with_alternatives, CloseReason, Connection, DialOptions};
use crate::tls;
use crate::{Error, Result};

/// How often pinned peers without a session are retried. *Choice:* the dial limiter's
/// 5 s (protocol.md 8.3 rule 1) is also the retry period, so a restarted peer is found
/// within about 5 s even when multicast is blocked.
const RETRY: Duration = Duration::from_secs(5);

/// When the discovery diagnostic runs after start (PLAN.md: say which layer failed).
const DIAGNOSE_AFTER: Duration = Duration::from_secs(10);

/// Most discovered addresses remembered per peer.
const MAX_DISCOVERED: usize = 4;

/// Tracks a connection and forgets it when it ends.
pub(crate) fn watch_connection(inner: &Arc<Inner>, conn: &Arc<Connection>) {
    inner.registry.track(conn);
    let i2 = Arc::clone(inner);
    let c = Arc::clone(conn);
    inner.spawn(async move {
        c.closed().await;
        i2.registry.detach(&c);
    });
}

/// The per-peer dial lock. A path switch holds it too (protocol.md 3.5).
pub(crate) fn dial_lock(inner: &Inner, peer: DeviceId) -> Arc<tokio::sync::Mutex<()>> {
    Arc::clone(lock(&inner.dial_locks).entry(peer).or_default())
}

/// A replaced connection gets the 10 s drain only if it is alive (protocol.md 3.5). After
/// a silent path loss its open `PUT` still holds the file the resume needs, so a connection
/// from which nothing at all arrives closes at once. One that is delivering is alive even
/// if the ACK is late: closing it would throw away the session frames still queued on it.
pub(crate) fn close_if_silent(inner: &Arc<Inner>, old: Arc<Connection>) {
    inner.spawn(async move {
        loop {
            match old.heard_from(REPLACED_PING_DEADLINE).await {
                // A normal end: a drained connection goes when its last stream is done.
                Heard::Ack | Heard::Closed => return,
                Heard::Bytes => {}
                Heard::Nothing => {
                    tracing::debug!(peer = %short(&old.device_id()), "replaced connection is silent; closed");
                    old.close(CloseReason::Dead);
                    return;
                }
            }
        }
    });
}

/// Our outbound connection to a pinned peer, dialling its last-known and discovered
/// addresses with the pinned TLS configuration if none is up (protocol.md 3.2, SR-1). A
/// fresh connection also tries to open the session stream (5.1), and the other addresses
/// that answer within the grace go to a path check (3.5).
///
/// While a path switch to the peer is under way, waits for it and returns the new
/// connection, so no new request starts on the one being retired (3.5).
pub(crate) async fn outbound(inner: &Arc<Inner>, peer: DeviceId) -> Result<Arc<Connection>> {
    if let Some(c) = inner.registry.outbound(&peer) {
        if !path::switching(inner, peer) {
            return Ok(c);
        }
    }
    let guard = dial_lock(inner, peer);
    let _held = guard.lock().await;
    if let Some(c) = inner.registry.outbound(&peer) {
        return Ok(c);
    }
    let pin = inner.pins.get(&peer).ok_or(Error::NotPaired)?;
    let mut addrs: Vec<SocketAddr> = pin.last_known.iter().copied().map(canonical_addr).collect();
    let mut path = DiscoveryPath::LastKnown;
    if let Some(r) = lock(&inner.reach).get(&peer) {
        for a in &r.discovered {
            if !addrs.contains(a) {
                addrs.push(*a);
                path = r.last_via.unwrap_or(DiscoveryPath::LastKnown);
            }
        }
    }
    if addrs.is_empty() {
        record_failure(inner, peer, path, "no address known yet".into());
        return Err(Error::NotFound);
    }
    let config = tls::build_client_config(&inner.identity, pin.spki_hash);
    let mut opts = DialOptions::new(config, Some(pin.spki_hash));
    opts.never_hold = path::merged_addresses(inner, peer);
    let grace = path::dial_grace(inner, peer);
    let (conn, alternatives) = match dial_with_alternatives(&addrs, &opts, grace).await {
        Ok(c) => c,
        Err(e) => {
            record_failure(inner, peer, path, e.to_string());
            return Err(e.into());
        }
    };
    tracing::debug!(peer = %short(&peer), addr = %conn.remote_addr(), "dialled");
    watch_connection(inner, &conn);
    // Unpaired while this dial ran: attaching would bring back a link for a forgotten
    // peer. Unpair removes the pin before it lists the tracked connections, so tracking
    // first and then checking the pin leaves no gap (SR-14).
    if !inner.pins.contains(&peer) {
        conn.close(CloseReason::Unpaired);
        return Err(Error::NotPaired);
    }
    if let Some(old) = inner.registry.attach(&conn).replaced {
        close_if_silent(inner, old);
    }
    let addr = conn.remote_addr();
    if pin.last_known.first() != Some(&addr) {
        if let Err(e) = inner
            .pins
            .update(move |s| s.push_last_known(&peer, addr))
            .await
        {
            tracing::debug!(error = %e, "last-known address not stored");
        }
    }
    let session_attempt = try_open_session(inner, &conn);
    path::after_dial(inner, &conn, alternatives, Some(session_attempt));
    Ok(conn)
}

/// Remembers a failed dial for the 10 s report and [`Host::diagnose`](super::Host::diagnose).
fn record_failure(inner: &Inner, peer: DeviceId, path: DiscoveryPath, failure: String) {
    let at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default();
    let mut reach = lock(&inner.reach);
    let r = reach.entry(peer).or_default();
    r.last_dial_error = Some(failure.clone());
    r.last_failure = Some(DialFailure { path, failure, at });
}

/// How long [`try_open_session`] waits for the session to be placed before it reports
/// the attempt settled.
const SESSION_SETTLE: Duration = Duration::from_secs(10);

/// Opens a session on `conn` unless a preferred one is live (protocol.md 3.3); then it
/// only sends [`path::keep`], so the peer counts the connection in use (3.5). The
/// receiver fires once the attempt settled: the session was placed, refused or timed
/// out, or the keep request was answered (a path check waits for it, 3.5).
fn try_open_session(
    inner: &Arc<Inner>,
    conn: &Arc<Connection>,
) -> tokio::sync::oneshot::Receiver<()> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let i2 = Arc::clone(inner);
    let c = Arc::clone(conn);
    if inner.registry.placement(conn) == Placement::Reject {
        inner.spawn(async move {
            if let Err(e) = path::keep(&c).await {
                tracing::debug!(peer = %short(&c.device_id()), error = %e, "keep request failed");
            }
            let _ = tx.send(());
        });
        return rx;
    }
    inner.spawn(async move {
        match session::open_outbound(&i2, &c).await {
            Ok(sess) => {
                path::wait_installed(&i2, &sess, SESSION_SETTLE).await;
            }
            Err(e) => {
                tracing::debug!(peer = %short(&c.device_id()), error = %e, "session not opened");
            }
        }
        let _ = tx.send(());
    });
    rx
}

/// Makes sure a session with `peer` is up or being set up.
pub(crate) async fn ensure_session(inner: &Arc<Inner>, peer: DeviceId) -> Result<()> {
    if inner.registry.session(&peer).is_some() {
        return Ok(());
    }
    let had = inner.registry.outbound(&peer);
    let conn = outbound(inner, peer).await?;
    if had.is_some() {
        drop(try_open_session(inner, &conn));
    }
    Ok(())
}

/// Dials every pinned peer without a session at start and then every 5 s, at most once
/// per peer per 5 s (protocol.md 3.2, 8.3 rule 1). [`Host::network_changed`] wakes it
/// early.
///
/// [`Host::network_changed`]: super::Host::network_changed
pub(crate) async fn dial_loop(inner: Arc<Inner>) {
    loop {
        let ids: Vec<DeviceId> = inner.pins.read().iter().map(|p| p.device_id).collect();
        for id in ids {
            if inner.registry.session(&id).is_some() || inner.is_shutting_down() {
                continue;
            }
            let allowed =
                lock(&inner.dial_limiter).try_dial(&id.to_hex(), tokio::time::Instant::now());
            if allowed {
                let i2 = Arc::clone(&inner);
                inner.spawn(async move {
                    if let Err(e) = ensure_session(&i2, id).await {
                        tracing::debug!(peer = %short(&id), error = %e, "last-known dial failed");
                    }
                });
            }
        }
        tokio::select! {
            () = tokio::time::sleep(RETRY) => {}
            () = inner.dial_nudge.notified() => {}
        }
    }
}

/// Acts on discovery events (protocol.md 8.3): a verified advert may trigger one dial to
/// its source address, rate-limited per peer; "Add device" adverts are only listed.
pub(crate) async fn discovery_events(inner: Arc<Inner>, mut rx: mpsc::Receiver<DiscoveryEvent>) {
    while let Some(ev) = rx.recv().await {
        match ev {
            DiscoveryEvent::Verified {
                device_id,
                addr,
                via,
                ..
            } => {
                let Ok(id) = device_id.parse::<DeviceId>() else {
                    continue;
                };
                on_verified_advert(&inner, id, addr, via);
            }
            DiscoveryEvent::Unpaired {
                name,
                device_type,
                addr,
                ..
            } => {
                let new =
                    first_sighting(&mut lock(&inner.add_device_seen), addr, &name, &device_type);
                if new {
                    inner.emit(HostEvent::AddDeviceSeen {
                        name,
                        device_type,
                        addr,
                    });
                }
            }
        }
    }
}

/// A verified advert (protocol.md 8.3): remembers the address, then re-checks the paths of
/// a peer that is online or dials one that is not, rate-limited per peer.
pub(crate) fn on_verified_advert(inner: &Arc<Inner>, id: DeviceId, addr: SocketAddr, via: Via) {
    let addr = canonical_addr(addr);
    {
        let mut reach = lock(&inner.reach);
        let r = reach.entry(id).or_default();
        match via {
            Via::Mdns => r.mdns = true,
            Via::Beacon => r.beacon = true,
        }
        r.last_via = Some(match via {
            Via::Mdns => DiscoveryPath::Mdns,
            Via::Beacon => DiscoveryPath::Beacon,
        });
        if !r.discovered.contains(&addr) {
            r.discovered.insert(0, addr);
            r.discovered.truncate(MAX_DISCOVERED);
        }
    }
    if inner.registry.session(&id).is_some() {
        // Online already: an advert from another address may be a faster
        // path (protocol.md 3.5).
        path::on_advert(inner, id, addr);
        return;
    }
    let allowed = lock(&inner.dial_limiter).try_dial(&id.to_hex(), tokio::time::Instant::now());
    if allowed {
        let i2 = Arc::clone(inner);
        inner.spawn(async move {
            if let Err(e) = ensure_session(&i2, id).await {
                tracing::debug!(peer = %short(&id), error = %e, "dial after advert failed");
            }
        });
    }
}

/// True the first time an address is seen in this "Add device" window, and again only
/// when its name or type changed (`docs/core-api.md` section 5). The memory is cleared
/// when the window closes.
pub(crate) fn first_sighting(
    seen: &mut std::collections::HashMap<std::net::SocketAddr, (String, String)>,
    addr: std::net::SocketAddr,
    name: &str,
    device_type: &str,
) -> bool {
    let now = (name.to_owned(), device_type.to_owned());
    if seen.get(&addr) == Some(&now) {
        return false;
    }
    seen.insert(addr, now);
    true
}

/// Ten seconds after start, names the discovery paths that produced nothing for each
/// pinned peer still unreached (PLAN.md: "say which layer failed").
pub(crate) async fn diagnostics(inner: Arc<Inner>) {
    tokio::time::sleep(DIAGNOSE_AFTER).await;
    let pins: Vec<(DeviceId, bool)> = inner
        .pins
        .read()
        .iter()
        .map(|p| (p.device_id, p.last_known.is_empty()))
        .collect();
    for (id, no_last_known) in pins {
        if inner.registry.session(&id).is_some() {
            continue;
        }
        let r = lock(&inner.reach).get(&id).cloned().unwrap_or_default();
        let report = |path, failure: String| {
            inner.emit(HostEvent::DiscoveryDiagnostic {
                peer: id.to_hex(),
                short_id: id.short_id(),
                path,
                failure,
            });
        };
        if inner.discovery_on {
            if !r.mdns {
                report(
                    DiscoveryPath::Mdns,
                    "no verified mDNS advert: multicast may be blocked, or the peer has not sent its announce key".into(),
                );
            }
            if !r.beacon {
                report(
                    DiscoveryPath::Beacon,
                    "no verified beacon: UDP 42187 multicast may be blocked or filtered".into(),
                );
            }
        } else {
            report(DiscoveryPath::Mdns, "discovery is turned off".into());
            report(DiscoveryPath::Beacon, "discovery is turned off".into());
        }
        let last = if no_last_known {
            "no last-known address".to_owned()
        } else {
            r.last_dial_error
                .unwrap_or_else(|| "dial did not complete".to_owned())
        };
        report(DiscoveryPath::LastKnown, last);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_device_seen_once_per_address_unless_changed() {
        let mut seen = std::collections::HashMap::new();
        let a: std::net::SocketAddr = "10.0.0.7:42187".parse().unwrap();
        let b: std::net::SocketAddr = "10.0.0.8:42187".parse().unwrap();
        assert!(first_sighting(&mut seen, a, "Pixel", "phone"));
        assert!(!first_sighting(&mut seen, a, "Pixel", "phone"));
        assert!(
            first_sighting(&mut seen, b, "Pixel", "phone"),
            "per address"
        );
        assert!(first_sighting(&mut seen, a, "Pixel 9", "phone"), "renamed");
        assert!(
            first_sighting(&mut seen, a, "Pixel 9", "tablet"),
            "new type"
        );
        assert!(!first_sighting(&mut seen, a, "Pixel 9", "tablet"));
        seen.clear(); // the window closed
        assert!(first_sighting(&mut seen, a, "Pixel 9", "tablet"));
    }
}

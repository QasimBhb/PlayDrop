//! Reaching pinned peers: last-known dial at start and on a timer, dials triggered by
//! verified adverts, and the "which layer failed" diagnostic (protocol.md 3.2, 8.3;
//! SR-1; PLAN.md discovery).

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;

use super::events::{DiscoveryPath, HostEvent};
use super::registry::Placement;
use super::{lock, session, Inner};
use crate::discovery::{DiscoveryEvent, Via};
use crate::identity::DeviceId;
use crate::net::connection::short;
use crate::net::{dial, Connection, DialOptions};
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

fn dial_lock(inner: &Inner, peer: DeviceId) -> Arc<tokio::sync::Mutex<()>> {
    Arc::clone(lock(&inner.dial_locks).entry(peer).or_default())
}

/// Our outbound connection to a pinned peer, dialling its last-known and discovered
/// addresses with the pinned TLS configuration if none is up (protocol.md 3.2, SR-1). A
/// fresh connection also tries to open the session stream (5.1).
pub(crate) async fn outbound(inner: &Arc<Inner>, peer: DeviceId) -> Result<Arc<Connection>> {
    if let Some(c) = inner.registry.outbound(&peer) {
        return Ok(c);
    }
    let guard = dial_lock(inner, peer);
    let _held = guard.lock().await;
    if let Some(c) = inner.registry.outbound(&peer) {
        return Ok(c);
    }
    let pin = inner.pins.get(&peer).ok_or(Error::NotPaired)?;
    let mut addrs = pin.last_known.clone();
    if let Some(r) = lock(&inner.reach).get(&peer) {
        for a in &r.discovered {
            if !addrs.contains(a) {
                addrs.push(*a);
            }
        }
    }
    let config = tls::build_client_config(&inner.identity, pin.spki_hash);
    let conn = match dial(&addrs, &DialOptions::new(config, Some(pin.spki_hash))).await {
        Ok(c) => c,
        Err(e) => {
            lock(&inner.reach).entry(peer).or_default().last_dial_error = Some(e.to_string());
            return Err(e.into());
        }
    };
    tracing::debug!(peer = %short(&peer), addr = %conn.remote_addr(), "dialled");
    watch_connection(inner, &conn);
    inner.registry.attach(&conn);
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
    try_open_session(inner, &conn);
    Ok(conn)
}

/// Opens a session on `conn` unless a preferred one is live (protocol.md 3.3).
fn try_open_session(inner: &Arc<Inner>, conn: &Arc<Connection>) {
    if inner.registry.placement(conn) == Placement::Reject {
        return;
    }
    let i2 = Arc::clone(inner);
    let c = Arc::clone(conn);
    inner.spawn(async move {
        if let Err(e) = session::open_outbound(&i2, &c).await {
            tracing::debug!(peer = %short(&c.device_id()), error = %e, "session not opened");
        }
    });
}

/// Makes sure a session with `peer` is up or being set up.
pub(crate) async fn ensure_session(inner: &Arc<Inner>, peer: DeviceId) -> Result<()> {
    if inner.registry.session(&peer).is_some() {
        return Ok(());
    }
    let had = inner.registry.outbound(&peer);
    let conn = outbound(inner, peer).await?;
    if had.is_some() {
        try_open_session(inner, &conn);
    }
    Ok(())
}

/// Dials every pinned peer without a session at start and then every 5 s, at most once
/// per peer per 5 s (protocol.md 3.2, 8.3 rule 1).
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
        tokio::time::sleep(RETRY).await;
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
                {
                    let mut reach = lock(&inner.reach);
                    let r = reach.entry(id).or_default();
                    match via {
                        Via::Mdns => r.mdns = true,
                        Via::Beacon => r.beacon = true,
                    }
                    if !r.discovered.contains(&addr) {
                        r.discovered.insert(0, addr);
                        r.discovered.truncate(MAX_DISCOVERED);
                    }
                }
                if inner.registry.session(&id).is_some() {
                    continue;
                }
                let allowed =
                    lock(&inner.dial_limiter).try_dial(&device_id, tokio::time::Instant::now());
                if allowed {
                    let i2 = Arc::clone(&inner);
                    inner.spawn(async move {
                        if let Err(e) = ensure_session(&i2, id).await {
                            tracing::debug!(peer = %short(&id), error = %e, "dial after advert failed");
                        }
                    });
                }
            }
            DiscoveryEvent::Unpaired {
                name,
                device_type,
                addr,
                ..
            } => inner.emit(HostEvent::AddDeviceSeen {
                name,
                device_type,
                addr,
            }),
        }
    }
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

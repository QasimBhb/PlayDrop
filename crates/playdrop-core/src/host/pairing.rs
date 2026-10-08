//! Pairing glue: drives `session::PairingSession` for both roles and both methods over a
//! session stream (protocol.md 6.1 to 6.3; SR-6, SR-7, SR-8, SR-9).
//!
//! One attempt runs at a time. The responder side admits a `pair_request` only on an
//! unpaired inbound connection, with the "Add device" window open, under the 5-per-minute
//! per-IP limit. On success the peer is pinned with default grants, the connection becomes
//! trusted in place and both sides send `capabilities` and `announce_key` (6.1).

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use super::events::{HostEvent, PairingPromptKind, Secret};
use super::session::{self, announce_key_frame, local_caps, Session};
use super::{connect, lock, platform, Inner};
use crate::identity::fingerprint_of;
use crate::net::connection::short;
use crate::net::{dial, CloseReason, Connection, DialOptions, Direction};
use crate::pin_store::Pin;
use crate::session::types::{CapabilitiesFrame, Frame};
use crate::session::{
    BeginRefusal, Envelope, Nonce, PairingContext, PairingEvent, PairingOutput, PairingSession,
    Party, QrOffer, QrPayload, QrSecret, RejectReason, Role,
};
use crate::tls;
use crate::{Error, Result};

/// How long an initiator waits for the peer's `hello` before giving up.
const HELLO_WAIT: Duration = Duration::from_secs(10);

/// QR validity (protocol.md 6.3: at most 120 s after display).
const QR_TTL: Duration = Duration::from_secs(crate::constants::ADD_DEVICE_WINDOW_SECS);

static NEXT_ATTEMPT: AtomicU64 = AtomicU64::new(1);

/// The attempt in progress.
pub(crate) struct ActivePairing {
    attempt: u64,
    machine: PairingSession,
    sess: Arc<Session>,
    peer_name: String,
    pin_addr: SocketAddr,
    counts_window: bool,
}

/// How an initiator starts.
enum Start {
    Sas,
    Qr(QrPayload),
}

/// Opens the "Add device" window for 120 s (protocol.md 6.1, SR-6) and closes the flag
/// when it expires or fails three times.
pub(crate) fn open_window(inner: &Arc<Inner>) -> Result<()> {
    // TODO(SR-6, Phase 2): refuse while the desktop is locked (logind LockedHint,
    // WTS_SESSION_LOCK); the CLI has no lock signal.
    lock(&inner.window)
        .open(Instant::now(), false)
        .map_err(|e| Error::Protocol(e.to_string()))?;
    inner.window_flag.store(true, Ordering::SeqCst);
    let i2 = Arc::clone(inner);
    inner.spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_millis(250)).await;
            let open = lock(&i2.window).is_open(Instant::now());
            if !open {
                i2.window_flag.store(false, Ordering::SeqCst);
                *lock(&i2.qr_offer) = None;
                return;
            }
            if !i2.window_flag.load(Ordering::SeqCst) {
                return;
            }
        }
    });
    Ok(())
}

fn context(inner: &Inner, sess: &Session, peer_name: &str) -> Result<PairingContext> {
    let local = Party::new(inner.local_id.to_hex(), inner.identity.spki_der().to_vec());
    let peer_id = sess.peer();
    let peer = Party::new(peer_id.to_hex(), sess.conn.peer().spki_der.clone());
    PairingContext::new(
        local,
        peer,
        sess.conn.exporter(),
        peer_name,
        peer_id.short_id(),
    )
    .map_err(Error::from)
}

/// Routes a `pair_*` frame: to the running attempt on this session, or, for a
/// `pair_request` on an unpaired inbound connection, to a new responder attempt.
pub(crate) async fn on_frame(inner: &Arc<Inner>, sess: &Arc<Session>, env: &Envelope) {
    let now = Instant::now();
    let outputs = {
        let mut slot = lock(&inner.pairing);
        match slot.as_mut() {
            Some(ap) if ap.sess.id == sess.id => ap.machine.handle_frame(now, env),
            Some(ap) if !ap.machine.is_finished() => {
                if matches!(env.frame, Frame::PairRequest(_)) {
                    vec![reject(RejectReason::Busy)]
                } else {
                    Vec::new()
                }
            }
            _ => match begin_responder(inner, sess, env, now) {
                Ok(Some((ap, outs))) => {
                    *slot = Some(ap);
                    outs
                }
                Ok(None) => Vec::new(),
                Err(outs) => outs,
            },
        }
    };
    process(inner, sess, outputs).await;
}

fn reject(reason: RejectReason) -> PairingOutput {
    PairingOutput::Send {
        frame: Frame::PairReject(crate::session::types::PairReject { reason }),
        re: None,
    }
}

type Begun = (ActivePairing, Vec<PairingOutput>);

/// Admits a responder attempt (protocol.md 6.1). `Err` carries frames to send instead.
fn begin_responder(
    inner: &Arc<Inner>,
    sess: &Arc<Session>,
    env: &Envelope,
    now: Instant,
) -> std::result::Result<Option<Begun>, Vec<PairingOutput>> {
    if !matches!(env.frame, Frame::PairRequest(_)) {
        return Ok(None);
    }
    if sess.conn.is_trusted() || sess.conn.direction() != Direction::Inbound {
        return Ok(None);
    }
    let ip = sess.conn.remote_addr().ip();
    if !lock(&inner.pair_limiter).check(ip, now) {
        return Err(vec![reject(RejectReason::RateLimited)]);
    }
    match lock(&inner.window).try_begin(now) {
        Ok(()) => {}
        Err(BeginRefusal::Busy) => return Err(vec![reject(RejectReason::Busy)]),
        Err(BeginRefusal::Closed) => {
            // Outside the window: close with no UI (protocol.md 6.1, SR-6).
            sess.conn.close(CloseReason::PairingEnded);
            return Ok(None);
        }
    }
    let Some(hello) = sess.peer_hello() else {
        lock(&inner.window).finish(false);
        return Ok(None);
    };
    let ctx = match context(inner, sess, &hello.name) {
        Ok(c) => c,
        Err(e) => {
            tracing::debug!(error = %e, "pairing context refused");
            lock(&inner.window).finish(false);
            sess.conn.close(CloseReason::PairingEnded);
            return Ok(None);
        }
    };
    let qr = lock(&inner.qr_offer).take();
    let mut machine = PairingSession::responder(ctx, now, Nonce::random(), qr);
    let outs = machine.handle_frame(now, env);
    let ap = ActivePairing {
        attempt: NEXT_ATTEMPT.fetch_add(1, Ordering::Relaxed),
        machine,
        sess: Arc::clone(sess),
        peer_name: hello.name.clone(),
        pin_addr: SocketAddr::new(ip, hello.port),
        counts_window: true,
    };
    arm_timeout(inner, ap.attempt, ap.machine.deadline());
    Ok(Some((ap, outs)))
}

/// Fires the 30 s attempt timeout (protocol.md 6.2).
fn arm_timeout(inner: &Arc<Inner>, attempt: u64, deadline: Instant) {
    let i2 = Arc::clone(inner);
    inner.spawn(async move {
        let wait = deadline.saturating_duration_since(Instant::now()) + Duration::from_millis(20);
        tokio::time::sleep(wait).await;
        let (sess, outs) = {
            let mut slot = lock(&i2.pairing);
            match slot.as_mut() {
                Some(ap) if ap.attempt == attempt => {
                    (Arc::clone(&ap.sess), ap.machine.poll(Instant::now()))
                }
                _ => return,
            }
        };
        process(&i2, &sess, outs).await;
    });
}

/// Performs a state machine's outputs in order.
async fn process(inner: &Arc<Inner>, sess: &Arc<Session>, outputs: Vec<PairingOutput>) {
    for out in outputs {
        match out {
            PairingOutput::Send { frame, re } => sess.send_raw(frame, re),
            PairingOutput::Event(ev) => on_event(inner, sess, ev).await,
        }
    }
}

async fn on_event(inner: &Arc<Inner>, sess: &Arc<Session>, ev: PairingEvent) {
    let peer = sess.peer();
    match ev {
        PairingEvent::ShowSas {
            sas,
            name,
            short_id,
        } => inner.emit(HostEvent::PairingPrompt {
            peer: peer.to_hex(),
            short_id,
            name,
            prompt: PairingPromptKind::Sas {
                sas: Secret(sas.as_str().to_owned()),
            },
        }),
        PairingEvent::AskConfirmScanner { name, short_id } => {
            inner.emit(HostEvent::PairingPrompt {
                peer: peer.to_hex(),
                short_id,
                name,
                prompt: PairingPromptKind::ScannerConfirm,
            });
        }
        PairingEvent::Paired { .. } => {
            let ap = take_attempt(inner, sess);
            if let Some(ap) = ap {
                if let Err(e) = complete(inner, ap).await {
                    tracing::warn!(error = %e, "pairing succeeded but the pin was not stored");
                    inner.emit(HostEvent::PairingFailed {
                        reason: format!("could not store the pin: {e}"),
                    });
                    sess.close_after_flush(inner, CloseReason::PairingEnded);
                }
            }
        }
        PairingEvent::Rejected { reason, by_peer } => {
            fail(
                inner,
                sess,
                &format!(
                    "{}{reason}",
                    if by_peer { "rejected by peer: " } else { "" }
                ),
            );
        }
        PairingEvent::Aborted { detail } => fail(inner, sess, detail),
    }
}

fn take_attempt(inner: &Inner, sess: &Session) -> Option<ActivePairing> {
    let mut slot = lock(&inner.pairing);
    if slot.as_ref().is_some_and(|ap| ap.sess.id == sess.id) {
        slot.take()
    } else {
        None
    }
}

fn fail(inner: &Arc<Inner>, sess: &Arc<Session>, reason: &str) {
    if let Some(ap) = take_attempt(inner, sess) {
        if ap.counts_window {
            lock(&inner.window).finish(false);
            if !lock(&inner.window).is_open(Instant::now()) {
                inner
                    .window_flag
                    .store(false, std::sync::atomic::Ordering::SeqCst);
            }
        }
    }
    tracing::info!(peer = %short(&sess.peer()), reason, "pairing failed");
    inner.emit(HostEvent::PairingFailed {
        reason: reason.to_owned(),
    });
    if !sess.conn.is_trusted() {
        sess.close_after_flush(inner, CloseReason::PairingEnded);
    }
}

/// Pins the peer with default grants, trusts the connection in place, sends
/// `capabilities` and `announce_key`, and makes the session live (protocol.md 6.1, 6.4).
async fn complete(inner: &Arc<Inner>, ap: ActivePairing) -> Result<()> {
    let conn = Arc::clone(&ap.sess.conn);
    let id = conn.device_id();
    let pin = Pin::new(conn.peer().cert_der.clone(), ap.peer_name.clone())?;
    let addr = ap.pin_addr;
    inner
        .pins
        .update(move |s| {
            s.insert(pin)?;
            s.push_last_known(&id, addr)?;
            Ok(())
        })
        .await?;
    conn.mark_trusted();
    inner.close_window();
    *lock(&inner.qr_offer) = None;
    if ap.counts_window {
        lock(&inner.window).finish(true);
    }
    inner.registry.attach(&conn);
    ap.sess.send_raw(
        Frame::Capabilities(CapabilitiesFrame { caps: local_caps() }),
        None,
    );
    ap.sess.send_raw(announce_key_frame(inner), None);
    inner.emit(HostEvent::Paired {
        peer: id.to_hex(),
        short_id: id.short_id(),
        name: ap.peer_name.clone(),
    });
    tracing::info!(peer = %short(&id), "paired");
    if let Some(v) = ap.sess.peer_hello() {
        session::activate(inner, &ap.sess, &v);
    }
    Ok(())
}

/// Ends a running attempt whose stream is gone.
pub(crate) fn on_session_end(inner: &Arc<Inner>, sess: &Arc<Session>) {
    let pending = lock(&inner.pairing)
        .as_ref()
        .is_some_and(|ap| ap.sess.id == sess.id);
    if pending {
        fail(inner, sess, "connection closed during pairing");
    }
}

/// The user's answer to the current prompt.
pub(crate) fn respond(inner: &Arc<Inner>, accept: bool) {
    let now = Instant::now();
    let found = {
        let mut slot = lock(&inner.pairing);
        slot.as_mut().map(|ap| {
            let outs = if accept {
                ap.machine.confirm(now)
            } else {
                ap.machine.reject(now)
            };
            (Arc::clone(&ap.sess), outs)
        })
    };
    if let Some((sess, outs)) = found {
        let inner = Arc::clone(inner);
        let i2 = Arc::clone(&inner);
        i2.spawn(async move { process(&inner, &sess, outs).await });
    }
}

/// Dials with the unpaired configuration (protocol.md 6.1): any valid self-signed Ed25519
/// certificate is accepted, and pairing authenticates it.
async fn dial_unpaired(inner: &Arc<Inner>, addrs: &[SocketAddr]) -> Result<Arc<Connection>> {
    let config = tls::build_client_config_unpaired(&inner.identity);
    let conn = dial(addrs, &DialOptions::new(config, None))
        .await
        .map_err(|e| {
            Error::Protocol(format!(
                "could not reach the device (is its Add device window open?): {e}"
            ))
        })?;
    connect::watch_connection(inner, &conn);
    if inner.pins.contains(&conn.device_id()) {
        conn.close(CloseReason::Duplicate);
        return Err(Error::BadRequest("that device is already paired".into()));
    }
    Ok(conn)
}

async fn start_initiator(inner: &Arc<Inner>, sess: Arc<Session>, how: Start) -> Result<()> {
    let hello = sess
        .wait_hello(HELLO_WAIT)
        .await
        .ok_or_else(|| Error::Protocol("peer sent no hello".into()))?;
    let ctx = context(inner, &sess, &hello.name)?;
    let now = Instant::now();
    let (machine, outs) = match how {
        Start::Sas => PairingSession::sas_initiator(ctx, now, Nonce::random()),
        Start::Qr(payload) => {
            let fp = fingerprint_of(&sess.conn.peer().cert_der);
            PairingSession::qr_initiator(ctx, now, &payload, &fp)?
        }
    };
    let attempt = NEXT_ATTEMPT.fetch_add(1, Ordering::Relaxed);
    let deadline = machine.deadline();
    {
        let mut slot = lock(&inner.pairing);
        if slot.as_ref().is_some_and(|ap| !ap.machine.is_finished()) {
            return Err(Error::PairRejected("busy"));
        }
        *slot = Some(ActivePairing {
            attempt,
            machine,
            sess: Arc::clone(&sess),
            peer_name: hello.name.clone(),
            pin_addr: sess.conn.remote_addr(),
            counts_window: false,
        });
    }
    debug_assert_eq!(
        lock(&inner.pairing).as_ref().map(|a| a.machine.role()),
        Some(Role::Initiator)
    );
    arm_timeout(inner, attempt, deadline);
    process(inner, &sess, outs).await;
    Ok(())
}

/// SAS initiator (protocol.md 6.2): the user picked `addr` from the "Add device" list.
pub(crate) async fn pair_with(inner: &Arc<Inner>, addr: SocketAddr) -> Result<()> {
    if !inner.window_flag.load(Ordering::SeqCst) {
        open_window(inner)?;
    }
    let conn = dial_unpaired(inner, &[addr]).await?;
    let sess = session::open_outbound(inner, &conn).await?;
    start_initiator(inner, sess, Start::Sas).await
}

/// QR displayer (protocol.md 6.3): a fresh one-time secret valid for 120 s.
pub(crate) fn show_qr(inner: &Arc<Inner>) -> Result<String> {
    open_window(inner)?;
    let secret = QrSecret::random();
    let exp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
        + QR_TTL.as_secs();
    *lock(&inner.qr_offer) = Some(QrOffer::new(secret.clone(), Instant::now() + QR_TTL));
    let ips = if inner.bind_ip.is_unspecified() {
        platform::local_addresses(inner.interfaces.as_ref())
    } else {
        vec![inner.bind_ip]
    };
    let payload = QrPayload {
        device_id: inner.local_id.to_hex(),
        fingerprint: inner.identity.fingerprint(),
        addrs: ips
            .into_iter()
            .map(|ip| SocketAddr::new(ip, inner.port()))
            .collect(),
        secret,
        exp,
    };
    Ok(payload.to_uri())
}

/// QR scanner (protocol.md 6.3): aborts before sending anything unless the peer's
/// certificate matches both `id` and `fp` (SR-9).
pub(crate) async fn scan_qr(inner: &Arc<Inner>, uri: &str) -> Result<()> {
    let payload = QrPayload::parse(uri)?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default();
    if now > payload.exp {
        return Err(Error::PairRejected("qr_expired"));
    }
    let conn = dial_unpaired(inner, &payload.addrs).await?;
    let fp = fingerprint_of(&conn.peer().cert_der);
    if !payload.matches_peer(&conn.device_id().to_hex(), &fp) {
        conn.close(CloseReason::PairingEnded);
        return Err(Error::Protocol(
            "the device that answered is not the one in the QR code".into(),
        ));
    }
    let sess = session::open_outbound(inner, &conn).await?;
    start_initiator(inner, sess, Start::Qr(payload)).await
}

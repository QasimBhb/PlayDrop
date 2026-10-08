//! The session stream (protocol.md 5): `POST /v1/session`, `hello` first from both sides,
//! length-prefixed frames both ways for the connection's life, the replay rule (SR-16),
//! capability negotiation (section 4), grants (SR-13, SR-14) and the frame handlers.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::{Bytes, BytesMut};
use h2::server::SendResponse;
use h2::{RecvStream, SendStream};
use http::{Method, Request, StatusCode};
use tokio::sync::{mpsc, watch};

use super::events::{HostEvent, Secret};
use super::registry::Placement;
use super::{lock, pairing, receive, send, Inner};
use crate::identity::DeviceId;
use crate::net::connection::short;
use crate::net::server::SESSION_PATH;
use crate::net::{
    is_remote_reset, respond_error, send_all, BodyReader, CloseReason, Connection, HttpError,
    KeepAlive, FRAMES_CONTENT_TYPE,
};
use crate::pin_store::{AnnounceKey as PinAnnounceKey, Grants};
use crate::session::types::{
    AnnounceKey, CapabilitiesFrame, Capability, Caps, Frame, GrantKind, Hello, Trust,
};
use crate::session::{
    may_send_frame, validate_hello, ErrorCode, Inbound, InboundDecoder, OutboundEncoder,
    TrustCheck, ValidatedHello,
};
use crate::{Error, Result};

/// What this build accepts (`caps.in`, protocol.md section 4). Media, input, outbox and
/// blobs are Phase 3 to 5 and are not advertised.
pub(crate) const LOCAL_IN: &[Capability] = &[
    Capability::Ping,
    Capability::Find,
    Capability::Clipboard,
    Capability::Battery,
    Capability::Presence,
    Capability::Transfer,
    Capability::TransferBundle,
];

/// What this build may send (`caps.out`).
pub(crate) const LOCAL_OUT: &[Capability] = &[
    Capability::Ping,
    Capability::Find,
    Capability::Clipboard,
    Capability::Battery,
    Capability::Presence,
    Capability::Transfer,
    Capability::TransferBundle,
];

/// Time allowed for a closing session to flush its last frames.
const FLUSH_GRACE: Duration = Duration::from_millis(200);

pub(crate) fn local_caps() -> Caps {
    Caps {
        incoming: LOCAL_IN.to_vec(),
        outgoing: LOCAL_OUT.to_vec(),
    }
}

#[allow(
    clippy::large_enum_variant,
    reason = "frames are the common case; boxing would allocate per frame"
)]
enum Out {
    Frame(Frame, Option<u64>),
    End,
}

#[derive(Default)]
struct State {
    hello: Option<ValidatedHello>,
    peer_in: Vec<Capability>,
    forbidden_sent: HashSet<String>,
}

static NEXT_SESSION: AtomicU64 = AtomicU64::new(1);

/// One session stream.
pub(crate) struct Session {
    pub(crate) id: u64,
    pub(crate) conn: Arc<Connection>,
    tx: mpsc::UnboundedSender<Out>,
    state: Mutex<State>,
    hello_tx: watch::Sender<bool>,
    live: AtomicBool,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("id", &self.id)
            .field("conn", &self.conn.id())
            .finish_non_exhaustive()
    }
}

impl Session {
    fn new(conn: Arc<Connection>) -> (Arc<Self>, mpsc::UnboundedReceiver<Out>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let s = Arc::new(Self {
            id: NEXT_SESSION.fetch_add(1, Ordering::Relaxed),
            conn,
            tx,
            state: Mutex::new(State::default()),
            hello_tx: watch::Sender::new(false),
            live: AtomicBool::new(true),
        });
        (s, rx)
    }

    /// The peer's Device ID, from TLS (SR-13).
    pub(crate) fn peer(&self) -> DeviceId {
        self.conn.device_id()
    }

    /// True until the stream ends.
    pub(crate) fn is_live(&self) -> bool {
        self.live.load(Ordering::Acquire) && !self.conn.is_closed()
    }

    /// The peer's validated `hello`.
    pub(crate) fn peer_hello(&self) -> Option<ValidatedHello> {
        lock(&self.state).hello.clone()
    }

    /// Waits for the peer's `hello`.
    pub(crate) async fn wait_hello(&self, timeout: Duration) -> Option<ValidatedHello> {
        let mut rx = self.hello_tx.subscribe();
        let _ = tokio::time::timeout(timeout, rx.wait_for(|h| *h)).await;
        self.peer_hello()
    }

    /// Sends a frame if both capability lists allow it (protocol.md section 4: never send
    /// what the peer's `in` list lacks). Frames with no capability always go.
    pub(crate) fn send(&self, frame: Frame) {
        let allowed = {
            let st = lock(&self.state);
            may_send_frame(&frame, LOCAL_OUT, &st.peer_in)
        };
        if allowed {
            self.send_raw(frame, None);
        } else {
            tracing::debug!(
                t = frame.type_name(),
                "frame not sent: peer lacks the capability"
            );
        }
    }

    /// Sends a frame with no capability check (hello, pairing, error, pong, `re` replies).
    pub(crate) fn send_raw(&self, frame: Frame, re: Option<u64>) {
        let _ = self.tx.send(Out::Frame(frame, re));
    }

    /// Ends our direction of the stream after queued frames.
    pub(crate) fn end(&self) {
        let _ = self.tx.send(Out::End);
    }

    /// Ends the stream and then closes its connection, giving the last frames time to go
    /// out (protocol.md 5.4, 6.2: send the error or reject, then close).
    pub(crate) fn close_after_flush(&self, inner: &Arc<Inner>, reason: CloseReason) {
        self.end();
        let conn = Arc::clone(&self.conn);
        inner.spawn(async move {
            tokio::time::sleep(FLUSH_GRACE).await;
            conn.close(reason);
        });
    }

    /// Sends at most one `forbidden` or `not_paired` error per frame type per connection
    /// (protocol.md 5.3, section 11 SR-14 deviation).
    fn refuse_once(&self, t: &str, code: ErrorCode) {
        let first = lock(&self.state).forbidden_sent.insert(t.to_owned());
        if first {
            self.send_raw(Frame::error(code, None), None);
        }
    }
}

fn our_hello(inner: &Inner, conn: &Connection) -> Frame {
    let paired = inner.pins.contains(&conn.device_id());
    Frame::Hello(Hello {
        protocol_version: crate::constants::PROTOCOL_VERSION,
        device_id: inner.local_id.to_hex(),
        name: inner.device_name.clone(),
        device_type: inner.device_type.clone(),
        port: inner.port(),
        app: concat!("playdrop-core/", env!("CARGO_PKG_VERSION")).to_owned(),
        keep_alive: u32::try_from(inner.keepalive.as_secs()).unwrap_or(u32::MAX),
        trust: if paired {
            Trust::Paired
        } else {
            Trust::Unpaired
        },
        caps: local_caps(),
    })
}

/// Our announce key frame (protocol.md 8.2).
pub(crate) fn announce_key_frame(inner: &Inner) -> Frame {
    let k = inner.pins.read().local_announce_key().clone();
    Frame::AnnounceKey(AnnounceKey::new(u64::from(k.kid), &k.key))
}

/// Starts both halves of a session whose stream is open.
fn start(
    inner: &Arc<Inner>,
    conn: &Arc<Connection>,
    send: SendStream<Bytes>,
    body: RecvStream,
) -> Arc<Session> {
    let (sess, rx) = Session::new(Arc::clone(conn));
    sess.send_raw(our_hello(inner, conn), None);
    inner.spawn(writer(send, rx));
    inner.spawn(reader(Arc::clone(inner), Arc::clone(&sess), body));
    sess
}

/// `POST /v1/session` from the dialler (protocol.md 5.1): `200` at once, or `409 conflict`
/// for a second stream on the connection or when a preferred session is live (3.3).
pub(crate) async fn accept_inbound(
    inner: Arc<Inner>,
    conn: Arc<Connection>,
    req: Request<RecvStream>,
    mut respond: SendResponse<Bytes>,
) {
    let conflict = !conn.claim_session()
        || (conn.is_trusted() && inner.registry.placement(&conn) == Placement::Reject);
    if conflict {
        let err = HttpError::from_code(ErrorCode::Conflict, "a session stream already exists");
        let _ = respond_error(&mut respond, &err).await;
        return;
    }
    let head = crate::net::response_head(StatusCode::OK, Some(FRAMES_CONTENT_TYPE));
    let send = match respond.send_response(head, false) {
        Ok(s) => s,
        Err(e) => {
            tracing::debug!(error = %e, "session response failed");
            return;
        }
    };
    start(&inner, &conn, send, req.into_body());
}

/// Opens the session stream on a connection we dialled (protocol.md 5.1). A `409` means
/// the peer keeps a preferred session (3.3); the connection stays for requests.
pub(crate) async fn open_outbound(
    inner: &Arc<Inner>,
    conn: &Arc<Connection>,
) -> Result<Arc<Session>> {
    if !conn.claim_session() {
        return Err(Error::Protocol(
            "session already open on this connection".into(),
        ));
    }
    let result = open_outbound_inner(inner, conn).await;
    if result.is_err() {
        conn.release_session();
    }
    result
}

async fn open_outbound_inner(inner: &Arc<Inner>, conn: &Arc<Connection>) -> Result<Arc<Session>> {
    let req = Request::builder()
        .method(Method::POST)
        .uri(SESSION_PATH)
        .header(http::header::CONTENT_TYPE, FRAMES_CONTENT_TYPE)
        .body(())
        .map_err(|e| Error::Protocol(e.to_string()))?;
    let (resp, send) = conn.request(req, false).await?;
    let resp = tokio::time::timeout(Duration::from_secs(10), resp)
        .await
        .map_err(|_| Error::Protocol("session response timed out".into()))?
        .map_err(|e| Error::Io(crate::net::h2_io(e)))?;
    match resp.status() {
        StatusCode::OK => Ok(start(inner, conn, send, resp.into_body())),
        StatusCode::CONFLICT => Err(Error::Protocol("peer keeps another session (409)".into())),
        s => Err(Error::Protocol(format!("session refused with {s}"))),
    }
}

async fn writer(mut send: SendStream<Bytes>, mut rx: mpsc::UnboundedReceiver<Out>) {
    let mut enc = OutboundEncoder::new();
    while let Some(first) = rx.recv().await {
        let mut batch = BytesMut::new();
        let mut end = false;
        let mut push = |m: Out, batch: &mut BytesMut| match m {
            Out::Frame(f, re) => {
                if let Err(e) = enc.encode_into(&f, re, batch) {
                    tracing::debug!(error = %e, t = f.type_name(), "frame not encoded");
                }
                false
            }
            Out::End => true,
        };
        end |= push(first, &mut batch);
        // Batch whatever else is queued into one TLS write (protocol.md section 10).
        while !end {
            match rx.try_recv() {
                Ok(m) => end |= push(m, &mut batch),
                Err(_) => break,
            }
        }
        if send_all(&mut send, batch.freeze(), end).await.is_err() || end {
            return;
        }
    }
}

async fn reader(inner: Arc<Inner>, sess: Arc<Session>, body: RecvStream) {
    let mut body = BodyReader::new(body);
    let mut buf = BytesMut::new();
    let mut dec = InboundDecoder::new();
    'outer: loop {
        loop {
            match dec.decode(&mut buf) {
                Ok(Some(item)) => {
                    if handle_inbound(&inner, &sess, item).await.is_err() {
                        break 'outer;
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    // Oversized, malformed and out-of-sequence frames close the connection
                    // without an error frame (protocol.md section 9).
                    tracing::info!(peer = %short(&sess.peer()), error = %e, "session protocol error; closing");
                    sess.conn.close(CloseReason::Protocol);
                    break 'outer;
                }
            }
        }
        match body.chunk().await {
            Some(Ok(b)) => buf.extend_from_slice(&b),
            Some(Err(e)) => {
                if is_remote_reset(&e) {
                    sess.conn.record_reset();
                }
                break;
            }
            None => break,
        }
    }
    on_end(&inner, &sess);
}

/// The stream ended: forget it, report the peer offline if it was the live session, and
/// end any pairing attempt on it.
fn on_end(inner: &Arc<Inner>, sess: &Arc<Session>) {
    tracing::debug!(peer = %short(&sess.peer()), session = sess.id, conn = sess.conn.id(), "session ended");
    sess.live.store(false, Ordering::Release);
    sess.end();
    sess.conn.release_session();
    if inner.registry.uninstall(sess) {
        let id = sess.peer();
        inner.emit(HostEvent::PeerOffline {
            peer: id.to_hex(),
            short_id: id.short_id(),
        });
    }
    pairing::on_session_end(inner, sess);
}

/// Error that ends the reader (the connection is already being closed).
struct Fatal;

async fn handle_inbound(
    inner: &Arc<Inner>,
    sess: &Arc<Session>,
    item: Inbound,
) -> std::result::Result<(), Fatal> {
    match item {
        Inbound::Duplicate { .. } => Ok(()),
        Inbound::UnsupportedVersion { .. } => {
            if let Some((f, re)) = item.reply() {
                sess.send_raw(f, Some(re));
            }
            Ok(())
        }
        Inbound::Frame(env) => handle_frame(inner, sess, env).await,
    }
}

fn has_grant(grants: Grants, g: GrantKind) -> bool {
    match g {
        GrantKind::Files => grants.files,
        GrantKind::ClipboardIn => grants.clipboard_in,
        GrantKind::ClipboardOut => grants.clipboard_out,
        GrantKind::Input => grants.input,
        GrantKind::Media => grants.media,
        GrantKind::Find => grants.find,
    }
}

async fn handle_frame(
    inner: &Arc<Inner>,
    sess: &Arc<Session>,
    env: crate::session::Envelope,
) -> std::result::Result<(), Fatal> {
    let seen_hello = lock(&sess.state).hello.is_some();
    if let Frame::Hello(h) = &env.frame {
        if seen_hello {
            return Ok(());
        }
        return on_hello(inner, sess, h);
    }
    if !seen_hello {
        tracing::info!(peer = %short(&sess.peer()), "first frame was not hello; closing");
        sess.conn.close(CloseReason::Protocol);
        return Err(Fatal);
    }
    let peer = sess.peer();
    let t = env.frame.type_name().to_owned();
    if sess.conn.is_trusted() {
        // Authorisation uses the TLS-authenticated Device ID only (SR-13, SR-14).
        if let Some(g) = env.frame.required_grant() {
            let ok = inner
                .pins
                .get(&peer)
                .is_some_and(|p| has_grant(p.grants, g));
            if !ok {
                sess.refuse_once(&t, ErrorCode::Forbidden);
                return Ok(());
            }
        }
    } else if !env.frame.allowed_unpaired() {
        sess.refuse_once(&t, ErrorCode::NotPaired);
        return Ok(());
    }
    let hex = peer.to_hex();
    match env.frame {
        Frame::Capabilities(CapabilitiesFrame { caps }) => {
            lock(&sess.state).peer_in = caps.incoming;
        }
        Frame::AnnounceKey(k) => store_announce_key(inner, peer, &k).await,
        Frame::Ping(p) => {
            inner.emit(HostEvent::Ping {
                peer: hex,
                message: p.message,
            });
            sess.send_raw(Frame::Pong, Some(env.seq));
        }
        Frame::Find(f) => inner.emit(HostEvent::Find {
            peer: hex,
            start: f.action == crate::session::types::FindAction::Start,
        }),
        Frame::Clipboard(c) => {
            // Echo prevention (protocol.md 5.7): remember what we applied.
            lock(&inner.clipboard_echo).last_applied = Some(c.hash.clone());
            inner.emit(HostEvent::Clipboard {
                peer: hex,
                text: Secret(c.text),
                sensitive: c.sensitive,
            });
        }
        Frame::Battery(b) => inner.emit(HostEvent::Battery {
            peer: hex,
            level: b.level,
            charging: b.charging,
            low: b.low,
        }),
        Frame::Presence(p) => inner.emit(HostEvent::Presence {
            peer: hex,
            state: p.state.to_string(),
        }),
        // TODO(Phase 3/5): media control, remote input and the outbox act on these.
        Frame::Media(_) | Frame::Input(_) | Frame::OutboxReady(_) => {
            inner.emit(HostEvent::Unsupported {
                peer: hex,
                frame_type: t,
            });
        }
        Frame::Transfer(tf) => send::on_transfer_frame(inner, peer, &tf),
        Frame::PairRequest(_)
        | Frame::PairResponse(_)
        | Frame::PairReveal(_)
        | Frame::PairConfirm(_)
        | Frame::PairReject(_) => pairing::on_frame(inner, sess, &env).await,
        Frame::Unpair => {
            if sess.conn.is_trusted() {
                let inner2 = Arc::clone(inner);
                inner.spawn(async move {
                    if let Err(e) = unpair(&inner2, peer, false).await {
                        tracing::debug!(error = %e, "unpair from peer failed");
                    }
                });
            }
        }
        Frame::Error(e) => {
            tracing::debug!(peer = %short(&peer), code = %e.code, "peer sent error frame");
        }
        Frame::Pong | Frame::Hello(_) | Frame::Unknown { .. } => {}
    }
    Ok(())
}

fn on_hello(
    inner: &Arc<Inner>,
    sess: &Arc<Session>,
    hello: &Hello,
) -> std::result::Result<(), Fatal> {
    let peer = sess.peer();
    let pinned = inner.pins.contains(&peer);
    let v = match validate_hello(hello, &peer.to_hex(), sess.conn.alpn_version(), pinned) {
        Ok(v) => v,
        Err(e) => {
            tracing::info!(peer = %short(&peer), error = %e, "hello refused; closing");
            sess.send_raw(e.error_frame(), None);
            sess.close_after_flush(inner, CloseReason::Protocol);
            return Err(Fatal);
        }
    };
    {
        let mut st = lock(&sess.state);
        st.peer_in.clone_from(&v.caps.incoming);
        st.hello = Some(v.clone());
    }
    sess.hello_tx.send_replace(true);
    // TODO(protocol.md 5.4, SR-8): mark the pin "removed by peer" in the pin store; for now
    // the UI gets the event and the pin is kept until the user acts.
    if v.trust == TrustCheck::RemovedByPeer {
        inner.emit(HostEvent::RemovedByPeer {
            peer: peer.to_hex(),
            short_id: peer.short_id(),
        });
    }
    if sess.conn.is_trusted() {
        activate(inner, sess, &v);
    }
    Ok(())
}

/// Makes a trusted session the peer's live session if the 3.3 rule allows, starts the
/// keep-alive and sends our announce key (protocol.md 8.2: at every session start).
pub(crate) fn activate(inner: &Arc<Inner>, sess: &Arc<Session>, v: &ValidatedHello) {
    let peer = sess.peer();
    let (placement, old) = inner.registry.install(sess);
    tracing::debug!(peer = %short(&peer), session = sess.id, conn = sess.conn.id(), ?placement, "session placed");
    match placement {
        Placement::Reject => {
            tracing::debug!(peer = %short(&peer), "duplicate session refused (3.3)");
            sess.end();
            return;
        }
        Placement::Install => inner.emit(HostEvent::PeerOnline {
            peer: peer.to_hex(),
            short_id: peer.short_id(),
            name: v.name.clone(),
            addr: sess.conn.remote_addr(),
        }),
        Placement::Replace => {}
    }
    if let Some(old) = old {
        tracing::debug!(peer = %short(&peer), "session moved to the preferred connection (3.3)");
        old.end();
    }
    sess.conn.start_keepalive(KeepAlive {
        local: inner.keepalive,
        peer: Duration::from_secs(u64::from(v.keep_alive.max(1))),
    });
    sess.send_raw(announce_key_frame(inner), None);
}

async fn store_announce_key(inner: &Arc<Inner>, peer: DeviceId, k: &AnnounceKey) {
    let (Some(key), Ok(kid)) = (k.key_bytes(), u32::try_from(k.kid)) else {
        tracing::debug!(peer = %short(&peer), "announce_key malformed; ignored");
        return;
    };
    let stored = inner
        .pins
        .update(move |s| s.set_announce_key(&peer, PinAnnounceKey { kid, key }))
        .await;
    if let Err(e) = stored {
        tracing::debug!(error = %e, "announce key not stored");
    }
}

/// Unpairs `peer` (protocol.md 6.4): optionally tells it, deletes the pin (which rotates
/// our announce key, 8.2), closes its connections within 1 s, drops its transfers and
/// pushes the new key to every remaining peer.
pub(crate) async fn unpair(inner: &Arc<Inner>, peer: DeviceId, notify: bool) -> Result<()> {
    if notify {
        if let Some(s) = inner.registry.session(&peer) {
            s.send_raw(Frame::Unpair, None);
        }
    }
    let removed = inner.pins.update(move |s| s.remove(&peer)).await?;
    if removed.is_none() {
        return if notify { Err(Error::NotFound) } else { Ok(()) };
    }
    let key = inner.pins.read().local_announce_key().key;
    if let Some(d) = lock(&inner.discovery).as_ref() {
        d.set_announce_key(key);
    }
    let conns = inner.registry.forget(&peer);
    inner.spawn(async move {
        tokio::time::sleep(FLUSH_GRACE).await;
        for c in conns {
            c.close(CloseReason::Unpaired);
        }
    });
    receive::drop_peer(inner, &peer).await;
    send::drop_peer(inner, &peer);
    for s in inner.registry.sessions() {
        if s.peer() != peer {
            s.send_raw(announce_key_frame(inner), None);
        }
    }
    inner.emit(HostEvent::Unpaired {
        peer: peer.to_hex(),
        short_id: peer.short_id(),
    });
    Ok(())
}

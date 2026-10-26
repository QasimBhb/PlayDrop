//! The session stream (protocol.md 5): `POST /v1/session`, `hello` first from both sides,
//! length-prefixed frames both ways for the connection's life, the replay rule (SR-16),
//! capability negotiation (section 4), grants (SR-13, SR-14) and the frame handlers.

use std::collections::{HashSet, VecDeque};
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
use super::{blobs, input, lock, outbox, pairing, receive, send, send_resume, status, Inner};
use crate::identity::DeviceId;
use crate::net::connection::short;
use crate::net::server::SESSION_PATH;
use crate::net::{
    is_remote_reset, respond_error, send_all, BodyReader, CloseReason, Connection, HttpError,
    KeepAlive, FRAMES_CONTENT_TYPE,
};
use crate::pin_store::{AnnounceKey as PinAnnounceKey, Grants};
use crate::session::types::{
    AnnounceKey, CapabilitiesFrame, Capability, Caps, Frame, GrantKind, Hello, Media, Trust,
};
use crate::session::{
    may_send_frame, validate_hello, ErrorCode, Inbound, InboundDecoder, OutboundEncoder,
    TrustCheck, ValidatedHello,
};
use crate::{Error, Result};

/// What every host accepts (`caps.in`, protocol.md section 4; `docs/core-api.md`
/// section 6). Remote input (`input.*`, [`INPUT_CAPS`]) is added only by a host with a
/// backend that applies it (`HostConfig::accepts_input`).
pub(crate) const LOCAL_IN: &[Capability] = &[
    Capability::Ping,
    Capability::Find,
    Capability::Clipboard,
    Capability::ClipboardImage,
    Capability::Battery,
    Capability::MediaState,
    Capability::MediaControl,
    Capability::Presence,
    Capability::Transfer,
    Capability::TransferBundle,
    Capability::Outbox,
    Capability::Blobs,
];

/// The remote input capabilities (protocol.md 5.12).
pub(crate) const INPUT_CAPS: &[Capability] = &[
    Capability::InputPointer,
    Capability::InputKey,
    Capability::InputPresenter,
];

/// What this build may send (`caps.out`): [`LOCAL_IN`] plus [`INPUT_CAPS`], since any host
/// may act as the controller.
pub(crate) const LOCAL_OUT: &[Capability] = &[
    Capability::Ping,
    Capability::Find,
    Capability::Clipboard,
    Capability::ClipboardImage,
    Capability::Battery,
    Capability::MediaState,
    Capability::MediaControl,
    Capability::Presence,
    Capability::Transfer,
    Capability::TransferBundle,
    Capability::Outbox,
    Capability::Blobs,
    Capability::InputPointer,
    Capability::InputKey,
    Capability::InputPresenter,
];

/// Time allowed for a closing session to flush its last frames.
const FLUSH_GRACE: Duration = Duration::from_millis(200);

/// Frame types remembered per connection so an `error` reply can be matched to what we
/// sent (protocol.md 5.14 `re`).
const SENT_TYPES_KEPT: usize = 256;

/// This host's capability lists for `hello` (protocol.md 5.4).
pub(crate) fn local_caps(inner: &Inner) -> Caps {
    let mut incoming = LOCAL_IN.to_vec();
    if inner.accepts_input {
        incoming.extend_from_slice(INPUT_CAPS);
    }
    Caps {
        incoming,
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
    /// `(seq, type)` of the last frames we sent, newest last, for `error.re` lookups.
    sent_types: VecDeque<(u64, String)>,
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

    /// True if the peer accepts `cap` (its `caps.in`) and this build may send it.
    pub(crate) fn peer_accepts(&self, cap: &Capability) -> bool {
        crate::session::may_send(LOCAL_OUT, &lock(&self.state).peer_in, cap)
    }

    /// The peer's `caps.in` as wire strings, for `PeerSummary::capabilities`.
    pub(crate) fn peer_caps_in(&self) -> Vec<String> {
        lock(&self.state)
            .peer_in
            .iter()
            .map(|c| c.as_str().to_owned())
            .collect()
    }

    /// Remembers the type of a frame we sent under `seq` (bounded).
    fn record_sent(&self, seq: u64, t: &str) {
        let mut st = lock(&self.state);
        if st.sent_types.len() >= SENT_TYPES_KEPT {
            st.sent_types.pop_front();
        }
        st.sent_types.push_back((seq, t.to_owned()));
    }

    /// The type of the frame we sent under `seq`, if still remembered.
    fn sent_type(&self, seq: u64) -> Option<String> {
        lock(&self.state)
            .sent_types
            .iter()
            .rev()
            .find(|(s, _)| *s == seq)
            .map(|(_, t)| t.clone())
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
    fn refuse_once(&self, t: &str, code: ErrorCode, re: u64) {
        let first = lock(&self.state).forbidden_sent.insert(t.to_owned());
        if first {
            let message = match code {
                ErrorCode::Forbidden => format!("{t} is not allowed for this device"),
                _ => format!("{t} needs a paired connection"),
            };
            // `re` names the refused frame so the sender can tell which feature to hide.
            self.send_raw(Frame::error(code, Some(&message)), Some(re));
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
        caps: local_caps(inner),
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
    inner.spawn(writer(Arc::clone(&sess), send, rx));
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

async fn writer(
    sess: Arc<Session>,
    mut send: SendStream<Bytes>,
    mut rx: mpsc::UnboundedReceiver<Out>,
) {
    let mut enc = OutboundEncoder::new();
    while let Some(first) = rx.recv().await {
        let mut batch = BytesMut::new();
        let mut end = false;
        let mut push = |m: Out, batch: &mut BytesMut| match m {
            Out::Frame(f, re) => {
                match enc.encode_into(&f, re, batch) {
                    Ok(seq) => sess.record_sent(seq, f.type_name()),
                    Err(e) => {
                        tracing::debug!(error = %e, t = f.type_name(), "frame not encoded");
                    }
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
        input::on_session_end(inner, id);
        inner.persist_last_seen(id);
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
        Inbound::Frame(env) => {
            // Any frame from a paired device counts as seen (SR-8).
            if sess.conn.is_trusted() {
                inner.touch_peer(sess.peer());
            }
            handle_frame(inner, sess, env).await
        }
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

#[allow(clippy::too_many_lines, reason = "one dispatch over every frame type")]
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
                sess.refuse_once(&t, ErrorCode::Forbidden, env.seq);
                return Ok(());
            }
        }
    } else if !env.frame.allowed_unpaired() {
        sess.refuse_once(&t, ErrorCode::NotPaired, env.seq);
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
        Frame::Clipboard(c) if c.blob.is_some() => {
            // Long text and images arrive as a blob (5.15): fetched and verified first; the
            // echo hash is recorded only once the bytes match it.
            blobs::fetch_clipboard(inner, peer, c);
        }
        Frame::Clipboard(c) => {
            // The hash feeds echo prevention, so it must describe the text it came with:
            // a mismatch is a protocol error and the frame is dropped (protocol.md 5.7).
            if let Some(text) = &c.text {
                if crate::session::types::clipboard_text_hash(text) != c.hash {
                    tracing::debug!(peer = %short(&peer), "clipboard hash does not match its text; dropped");
                    return Ok(());
                }
            }
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
        Frame::Media(Media::State(m)) => inner.emit(HostEvent::MediaState {
            peer: hex,
            state: m.into(),
        }),
        Frame::Media(Media::Command(c)) => inner.emit(HostEvent::MediaCommand {
            peer: hex,
            player: c.player,
            action: c.action,
            value: c.value,
        }),
        Frame::OutboxReady(o) => inner.emit(HostEvent::OutboxReady {
            peer: hex,
            count: o.count,
            bytes: o.bytes,
            ids: o.ids,
        }),
        Frame::Input(i) => input::on_frame(inner, peer, i),
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
        Frame::Error(e) if e.code == ErrorCode::InputSessionEnded => {
            input::on_ended_by_target(inner, peer, e.message);
        }
        Frame::Error(e) => {
            tracing::debug!(peer = %short(&peer), code = %e.code, "peer sent error frame");
            let frame_type = env.re.and_then(|re| sess.sent_type(re));
            inner.emit(HostEvent::PeerRefused {
                peer: hex,
                frame_type,
                code: e.code.as_str().to_owned(),
                message: e.message,
            });
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
    // protocol.md 5.4: the pin is marked "removed by peer" and kept until the user pairs
    // again (a fresh pin) or unpairs (no pin).
    if v.trust == TrustCheck::RemovedByPeer {
        let i2 = Arc::clone(inner);
        inner.spawn(async move {
            let stored = i2
                .pins
                .update(move |s| s.set_removed_by_peer(&peer, true))
                .await;
            if let Err(e) = stored {
                tracing::debug!(error = %e, "removed-by-peer not stored");
            }
        });
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
    if matches!(placement, Placement::Reject) {
        tracing::debug!(peer = %short(&peer), "duplicate session refused (3.3)");
        sess.end();
        return;
    }
    // Record the time before announcing the peer: a UI that reads `peers()` on the
    // online event must already see a last-seen time (SR-8).
    inner.touch_peer(peer);
    inner.persist_last_seen(peer);
    if matches!(placement, Placement::Install) {
        inner.emit(HostEvent::PeerOnline {
            peer: peer.to_hex(),
            short_id: peer.short_id(),
            name: v.name.clone(),
            addr: sess.conn.remote_addr(),
        });
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
    // Session start: remembered battery and presence (5.8, 5.10), waiting outbox (5.11).
    status::on_activate(inner, sess);
    outbox::notify(inner, peer);
    // Sends that gave up while the peer was away resume now (protocol.md 7.6).
    send_resume::resume_for_peer(inner, peer);
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
    send_resume::drop_peer(inner, &peer).await;
    outbox::drop_peer(inner, &peer).await;
    lock(&inner.status).forget(&peer);
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

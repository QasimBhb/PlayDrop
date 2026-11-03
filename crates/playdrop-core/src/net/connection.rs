//! One authenticated HTTP/2 connection to a peer, protocol.md 3.1 to 3.4.
//!
//! A [`Connection`] is created by the server (inbound) or the dialler (outbound) after the
//! TLS handshake and the HTTP/2 handshake. It holds what the rest of the host needs: the
//! TLS-authenticated [`PeerIdentity`], the pairing exporter value, a handle for opening
//! request streams (outbound only, since HTTP/2 lets only the client open streams), a
//! close signal, the SR-15 reset counter and the 3.4 keep-alive.

use std::fmt;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use bytes::Bytes;
use h2::client::{ResponseFuture, SendRequest};
use h2::{Ping, PingPong, SendStream};
use http::Request;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::watch;

use crate::identity::DeviceId;
use crate::session::ResetRateCounter;
use crate::tls::{PeerIdentity, PeerTrust, EXPORTER_LEN};
use crate::{Error, Result};

/// Keep-alive interval of a desktop host, sent in `hello.keepAlive` (protocol.md 3.4).
pub const DESKTOP_KEEPALIVE: Duration = Duration::from_secs(60);

/// Keep-alive interval of a mobile host (protocol.md 3.4).
pub const MOBILE_KEEPALIVE: Duration = Duration::from_secs(300);

/// A PING without an ACK for this long means the connection is dead (protocol.md 3.4).
pub const PING_DEADLINE: Duration = Duration::from_secs(15);

/// Deadline for the duplicate-connection liveness PING (protocol.md 3.3).
pub const DUPLICATE_PING_DEADLINE: Duration = Duration::from_secs(2);

/// Which side dialled the connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// The peer dialled us; we are the HTTP/2 server.
    Inbound,
    /// We dialled the peer; we are the HTTP/2 client and may open request streams.
    Outbound,
}

/// Why a connection was closed locally.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseReason {
    /// The host is shutting down.
    Shutdown,
    /// A newer connection replaced this one (protocol.md 3.3, `duplicate_connection`).
    Duplicate,
    /// Keep-alive PING went unanswered (protocol.md 3.4).
    Dead,
    /// The peer was unpaired (protocol.md 6.4).
    Unpaired,
    /// More than 100 stream resets in one second (SR-15).
    ResetFlood,
    /// A session-plane protocol violation (protocol.md 5.2, 5.4).
    Protocol,
    /// Pairing ended without success (protocol.md 6.2).
    PairingEnded,
    /// Replaced by a newer connection (protocol.md 3.3, 3.5): no new requests, and the
    /// open streams get [`DRAIN_GRACE`] to finish before the connection is dropped.
    Drain,
}

impl CloseReason {
    /// True for the reasons that let open streams finish first (a connection replaced by
    /// a newer one, protocol.md 3.3, 3.5). Any other reason overrides them.
    pub fn is_graceful(self) -> bool {
        matches!(self, Self::Drain | Self::Duplicate)
    }
}

/// How long a replaced connection may stay silent, no ACK and no bytes at all, before it
/// counts as dead and closes at once (protocol.md 3.5). *Choice:* 1 s, the close grace every
/// replaced connection had before the 10 s drain: a dialler usually redials because the old
/// path died, and the dead connection's open `PUT` holds its file until the connection goes,
/// so a resume waits this long at worst, as it did then.
pub const REPLACED_PING_DEADLINE: Duration = Duration::from_secs(1);

/// How long a replaced connection keeps its open streams (protocol.md 3.5): a request
/// that started on it before a path switch finishes there.
pub const DRAIN_GRACE: Duration = Duration::from_secs(10);

/// Keep-alive intervals of the two ends (protocol.md 3.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeepAlive {
    /// This host's interval.
    pub local: Duration,
    /// The peer's interval from its `hello`.
    pub peer: Duration,
}

impl KeepAlive {
    /// Whether this end sends the PINGs: only the side with the shorter interval pings.
    /// *Choice:* with equal intervals the dialler pings, so exactly one side does.
    pub fn we_ping(&self, direction: Direction) -> bool {
        match self.local.cmp(&self.peer) {
            std::cmp::Ordering::Less => true,
            std::cmp::Ordering::Greater => false,
            std::cmp::Ordering::Equal => direction == Direction::Outbound,
        }
    }

    /// Shortest idle time after which a connection may be closed: 2.5 times the larger
    /// interval (protocol.md 3.4).
    pub fn min_idle_close(&self) -> Duration {
        self.local.max(self.peer).mul_f64(2.5)
    }
}

/// The HTTP/2 PING handle and whether a PING is still waiting for its ACK.
struct PingState {
    handle: Option<PingPong>,
    unanswered: bool,
}

/// What [`Connection::heard_from`] found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Heard {
    /// The PING was acknowledged.
    Ack,
    /// No ACK in time, but bytes arrived: the path is alive and slow, and the ACK is queued
    /// behind what it is still delivering.
    Bytes,
    /// Nothing at all arrived.
    Nothing,
    /// The connection has ended.
    Closed,
}

/// Counts the bytes read from a connection's stream, so a path that is slow can be told
/// from one that is dead (protocol.md 3.5). Sits above TLS: only authenticated bytes count.
#[derive(Debug)]
pub(crate) struct ActivityIo<T> {
    inner: T,
    bytes_read: Arc<AtomicU64>,
}

impl<T> ActivityIo<T> {
    pub(crate) fn new(inner: T, bytes_read: Arc<AtomicU64>) -> Self {
        Self { inner, bytes_read }
    }
}

impl<T: AsyncRead + Unpin> AsyncRead for ActivityIo<T> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let before = buf.filled().len();
        let polled = Pin::new(&mut self.inner).poll_read(cx, buf);
        let read = buf.filled().len().saturating_sub(before);
        if read > 0 {
            self.bytes_read.fetch_add(read as u64, Ordering::Relaxed);
        }
        polled
    }
}

impl<T: AsyncWrite + Unpin> AsyncWrite for ActivityIo<T> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[std::io::IoSlice<'_>],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write_vectored(cx, bufs)
    }

    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }
}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// One authenticated HTTP/2 connection (protocol.md 3.1). Shared as `Arc<Connection>`.
pub struct Connection {
    id: u64,
    direction: Direction,
    peer: PeerIdentity,
    trusted: AtomicBool,
    remote: SocketAddr,
    exporter: [u8; EXPORTER_LEN],
    send: Mutex<Option<SendRequest<Bytes>>>,
    close_tx: watch::Sender<Option<CloseReason>>,
    closed_tx: watch::Sender<bool>,
    ping: tokio::sync::Mutex<PingState>,
    resets: Mutex<ResetRateCounter>,
    keepalive_started: AtomicBool,
    session_claimed: AtomicBool,
    probes: Mutex<super::probe::ProbeLimiter>,
    bytes_read: Arc<AtomicU64>,
    opened_at: Instant,
}

impl fmt::Debug for Connection {
    /// Leaves out the exporter value (SR-32).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Connection")
            .field("id", &self.id)
            .field("direction", &self.direction)
            .field("peer", &short(&self.peer.device_id))
            .field("remote", &self.remote)
            .finish_non_exhaustive()
    }
}

/// First 8 hex characters of a Device ID, the only form logs may carry (SR-32).
pub fn short(id: &DeviceId) -> String {
    let mut s = id.to_hex();
    s.truncate(8);
    s
}

impl Connection {
    /// Wraps a freshly handshaken connection. The caller drives the `h2` connection and
    /// watches [`Connection::close_requested`]; it calls [`Connection::mark_closed`] once
    /// the connection is gone.
    #[allow(
        clippy::too_many_arguments,
        reason = "one call each from client and server"
    )]
    pub(crate) fn new(
        direction: Direction,
        peer: PeerIdentity,
        remote: SocketAddr,
        exporter: [u8; EXPORTER_LEN],
        send: Option<SendRequest<Bytes>>,
        ping: Option<PingPong>,
        max_resets_per_second: usize,
        bytes_read: Arc<AtomicU64>,
    ) -> Arc<Self> {
        let trusted = peer.trust == PeerTrust::Pinned;
        Arc::new(Self {
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            direction,
            peer,
            trusted: AtomicBool::new(trusted),
            remote,
            exporter,
            send: Mutex::new(send),
            close_tx: watch::Sender::new(None),
            closed_tx: watch::Sender::new(false),
            ping: tokio::sync::Mutex::new(PingState {
                handle: ping,
                unanswered: false,
            }),
            resets: Mutex::new(ResetRateCounter::new(max_resets_per_second)),
            keepalive_started: AtomicBool::new(false),
            session_claimed: AtomicBool::new(false),
            probes: Mutex::new(super::probe::ProbeLimiter::default()),
            bytes_read,
            opened_at: Instant::now(),
        })
    }

    /// Process-unique connection number.
    pub fn id(&self) -> u64 {
        self.id
    }

    /// Who dialled.
    pub fn direction(&self) -> Direction {
        self.direction
    }

    /// The TLS-authenticated peer (protocol.md 3.1). Every authorisation decision uses this,
    /// never anything in a payload (SR-13).
    pub fn peer(&self) -> &PeerIdentity {
        &self.peer
    }

    /// The peer's Device ID.
    pub fn device_id(&self) -> DeviceId {
        self.peer.device_id
    }

    /// The peer's socket address.
    pub fn remote_addr(&self) -> SocketAddr {
        self.remote
    }

    /// When the connection was established.
    pub fn opened_at(&self) -> Instant {
        self.opened_at
    }

    /// The `EXPORTER-playdrop-pair` value of this TLS session (protocol.md 6.2, 6.3). Never
    /// log it (SR-32).
    pub fn exporter(&self) -> [u8; EXPORTER_LEN] {
        self.exporter
    }

    /// The protocol version ALPN selected (protocol.md section 4). Only `playdrop/1` exists.
    pub fn alpn_version(&self) -> u32 {
        crate::constants::PROTOCOL_VERSION
    }

    /// True once the peer is pinned: at the handshake, or in place after pairing
    /// (protocol.md 6.1).
    pub fn is_trusted(&self) -> bool {
        self.trusted.load(Ordering::Acquire)
    }

    /// Marks the connection trusted after pairing succeeded on it (protocol.md 6.1).
    pub fn mark_trusted(&self) {
        self.trusted.store(true, Ordering::Release);
    }

    /// Claims the connection's one session stream (protocol.md 5.1: a second one gets
    /// `409`). Returns false if it is already claimed.
    pub fn claim_session(&self) -> bool {
        !self.session_claimed.swap(true, Ordering::AcqRel)
    }

    /// Releases the claim after a session stream failed to open, so it can be retried.
    pub fn release_session(&self) {
        self.session_claimed.store(false, Ordering::Release);
    }

    /// Takes this connection's probe slot if the protocol.md 3.5 limits allow (one in
    /// flight, 10 per minute, SR-15).
    pub(crate) fn probe_try_start(&self) -> bool {
        self.probes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .try_start(Instant::now())
    }

    /// Frees the probe slot.
    pub(crate) fn probe_finished(&self) {
        self.probes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .finish();
    }

    /// Asks the driver to close the connection. Idempotent; the first reason wins,
    /// except that a harder reason (shutdown, unpair, dead, a protocol error) overrides a
    /// graceful one while it waits for open streams, and the connection then goes at
    /// once (SR-14: unpair closes within 1 s).
    pub fn close(&self, reason: CloseReason) {
        self.close_tx.send_if_modified(|r| match r {
            None => {
                *r = Some(reason);
                true
            }
            Some(old) if old.is_graceful() && !reason.is_graceful() => {
                *r = Some(reason);
                true
            }
            Some(_) => false,
        });
    }

    /// Retires the connection gracefully (protocol.md 3.5): no new requests; streams
    /// already open may finish within [`DRAIN_GRACE`]. On a connection we dialled, `h2`
    /// then ends it once its last stream is done; on one the peer dialled, the server
    /// sends GOAWAY and waits the same grace.
    pub fn drain(&self) {
        drop(
            self.send
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take(),
        );
        self.close(CloseReason::Drain);
    }

    /// True once [`Connection::drain`] retired a connection this host dialled: it takes
    /// no new requests.
    pub fn is_draining(&self) -> bool {
        self.direction == Direction::Outbound
            && self
                .send
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .is_none()
    }

    /// The close request channel, for the driver.
    pub(crate) fn close_requested(&self) -> watch::Receiver<Option<CloseReason>> {
        self.close_tx.subscribe()
    }

    /// Records that the connection is gone. Called by the driver.
    pub(crate) fn mark_closed(&self) {
        self.close(CloseReason::Shutdown);
        self.closed_tx.send_replace(true);
    }

    /// True once the connection has ended.
    pub fn is_closed(&self) -> bool {
        *self.closed_tx.borrow()
    }

    /// Resolves when the connection has ended.
    pub async fn closed(&self) {
        let mut rx = self.closed_tx.subscribe();
        let _ = rx.wait_for(|c| *c).await;
    }

    /// Records a stream reset sent by the peer; above the SR-15 rate the connection is
    /// closed. Returns true if it tripped.
    pub fn record_reset(&self) -> bool {
        let tripped = self
            .resets
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .record(Instant::now());
        if tripped {
            tracing::warn!(peer = %short(&self.peer.device_id), "stream reset flood; closing");
            self.close(CloseReason::ResetFlood);
        }
        tripped
    }

    /// Opens a request stream (outbound connections only: HTTP/2 lets only the client open
    /// streams). Returns the response future and the request body stream.
    pub async fn request(
        &self,
        req: Request<()>,
        end_of_stream: bool,
    ) -> Result<(ResponseFuture, SendStream<Bytes>)> {
        let send = self
            .send
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let Some(send) = send else {
            if self.direction == Direction::Outbound {
                return Err(Error::Io(std::io::Error::new(
                    std::io::ErrorKind::NotConnected,
                    "connection replaced",
                )));
            }
            return Err(Error::Protocol(
                "requests go only on connections this host dialled".into(),
            ));
        };
        if self.is_closed() {
            return Err(Error::Io(std::io::Error::new(
                std::io::ErrorKind::NotConnected,
                "connection closed",
            )));
        }
        let mut ready = send.ready().await.map_err(|e| Error::Io(super::h2_io(e)))?;
        ready
            .send_request(req, end_of_stream)
            .map_err(|e| Error::Io(super::h2_io(e)))
    }

    /// Sends an HTTP/2 PING and waits for the ACK up to `deadline` (protocol.md 3.3, 3.4).
    /// Returns the round-trip time.
    pub async fn ping(&self, deadline: Duration) -> Result<Duration> {
        let started = Instant::now();
        let waited = tokio::time::timeout(deadline, async {
            let mut guard = self.ping.lock().await;
            let state = &mut *guard;
            let pp = state
                .handle
                .as_mut()
                .ok_or_else(|| Error::Protocol("no ping handle".into()))?;
            // `h2` allows one user PING at a time and refuses another until the first is
            // acknowledged, so after a timeout the next call waits for that ACK instead of
            // failing: a stall that passes must not read as a dead connection. Its late
            // ACK proves the connection answers.
            if !state.unanswered {
                pp.send_ping(Ping::opaque())
                    .map_err(|e| Error::Io(super::h2_io(e)))?;
                state.unanswered = true;
            }
            std::future::poll_fn(|cx| pp.poll_pong(cx))
                .await
                .map_err(|e| Error::Io(super::h2_io(e)))?;
            state.unanswered = false;
            Ok(())
        })
        .await;
        match waited {
            Ok(Ok(())) => Ok(started.elapsed()),
            Ok(Err(e)) => Err(e),
            Err(_) => Err(Error::Io(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "ping not acknowledged",
            ))),
        }
    }

    /// Whether the peer can still be heard, within `deadline` (protocol.md 3.5). A PING
    /// alone cannot tell: on a slow path its ACK queues behind what is being delivered, so
    /// any bytes arriving meanwhile also count as alive.
    pub async fn heard_from(&self, deadline: Duration) -> Heard {
        if self.is_closed() {
            return Heard::Closed;
        }
        let before = self.bytes_read.load(Ordering::Relaxed);
        match self.ping(deadline).await {
            Ok(_) => Heard::Ack,
            // Only running out of time says anything about the path; any other error is the
            // connection already ending.
            Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::TimedOut => {
                if self.is_closed() {
                    Heard::Closed
                } else if self.bytes_read.load(Ordering::Relaxed) > before {
                    Heard::Bytes
                } else {
                    Heard::Nothing
                }
            }
            Err(_) => Heard::Closed,
        }
    }

    /// Starts the protocol.md 3.4 keep-alive once both intervals are known: the side with
    /// the shorter interval PINGs every interval and closes the connection when an ACK is
    /// 15 s late. Starting it twice does nothing. Idle connections are never closed here,
    /// which satisfies "nobody closes sooner than 2.5 times the larger interval".
    pub fn start_keepalive(self: &Arc<Self>, ka: KeepAlive) {
        if self.keepalive_started.swap(true, Ordering::AcqRel) {
            return;
        }
        if !ka.we_ping(self.direction) {
            return;
        }
        let conn = Arc::clone(self);
        let interval = ka.local.min(ka.peer).max(Duration::from_secs(1));
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    () = tokio::time::sleep(interval) => {}
                    () = conn.closed() => return,
                }
                if let Err(e) = conn.ping(PING_DEADLINE).await {
                    tracing::info!(peer = %short(&conn.peer.device_id), error = %e, "keep-alive failed; closing");
                    conn.close(CloseReason::Dead);
                    return;
                }
            }
        });
    }
}

//! One authenticated HTTP/2 connection to a peer, protocol.md 3.1 to 3.4.
//!
//! A [`Connection`] is created by the server (inbound) or the dialler (outbound) after the
//! TLS handshake and the HTTP/2 handshake. It holds what the rest of the host needs: the
//! TLS-authenticated [`PeerIdentity`], the pairing exporter value, a handle for opening
//! request streams (outbound only, since HTTP/2 lets only the client open streams), a
//! close signal, the SR-15 reset counter and the 3.4 keep-alive.

use std::fmt;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use bytes::Bytes;
use h2::client::{ResponseFuture, SendRequest};
use h2::{Ping, PingPong, SendStream};
use http::Request;
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
}

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

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// One authenticated HTTP/2 connection (protocol.md 3.1). Shared as `Arc<Connection>`.
pub struct Connection {
    id: u64,
    direction: Direction,
    peer: PeerIdentity,
    trusted: AtomicBool,
    remote: SocketAddr,
    exporter: [u8; EXPORTER_LEN],
    send: Option<SendRequest<Bytes>>,
    close_tx: watch::Sender<Option<CloseReason>>,
    closed_tx: watch::Sender<bool>,
    ping: tokio::sync::Mutex<Option<PingPong>>,
    resets: Mutex<ResetRateCounter>,
    keepalive_started: AtomicBool,
    session_claimed: AtomicBool,
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
    pub(crate) fn new(
        direction: Direction,
        peer: PeerIdentity,
        remote: SocketAddr,
        exporter: [u8; EXPORTER_LEN],
        send: Option<SendRequest<Bytes>>,
        ping: Option<PingPong>,
        max_resets_per_second: usize,
    ) -> Arc<Self> {
        let trusted = peer.trust == PeerTrust::Pinned;
        Arc::new(Self {
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            direction,
            peer,
            trusted: AtomicBool::new(trusted),
            remote,
            exporter,
            send,
            close_tx: watch::Sender::new(None),
            closed_tx: watch::Sender::new(false),
            ping: tokio::sync::Mutex::new(ping),
            resets: Mutex::new(ResetRateCounter::new(max_resets_per_second)),
            keepalive_started: AtomicBool::new(false),
            session_claimed: AtomicBool::new(false),
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

    /// Asks the driver to close the connection. Idempotent; the first reason wins.
    pub fn close(&self, reason: CloseReason) {
        self.close_tx.send_if_modified(|r| {
            if r.is_none() {
                *r = Some(reason);
                true
            } else {
                false
            }
        });
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
        let Some(send) = self.send.clone() else {
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
        let mut guard = self.ping.lock().await;
        let pp = guard
            .as_mut()
            .ok_or_else(|| Error::Protocol("no ping handle".into()))?;
        let started = Instant::now();
        match tokio::time::timeout(deadline, pp.ping(Ping::opaque())).await {
            Ok(Ok(_)) => Ok(started.elapsed()),
            Ok(Err(e)) => Err(Error::Io(super::h2_io(e))),
            Err(_) => Err(Error::Io(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "ping not acknowledged",
            ))),
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

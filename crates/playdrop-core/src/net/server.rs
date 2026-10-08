//! The listening side: exclusive bind, connection limits, TLS accept, HTTP/2 accept and
//! request routing. Implements protocol.md section 1 (port rules), 3.1, 6.1 and section 10
//! (SR-11, SR-13, SR-15, SR-31).

use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use h2::server::SendResponse;
use h2::{Reason, RecvStream};
use http::{Method, Request, StatusCode};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;

use super::connection::{short, CloseReason, Connection, Direction};
use super::{respond_error, server_builder, stopped, tune_socket, HttpError};
use crate::constants::FORBIDDEN_PORTS;
use crate::session::{ConnectionCounter, ErrorCode, HandshakeGate, Limits};
use crate::tls::{self, PeerTrust, PinLookup};
use crate::{Error, Result};

/// Path of the session stream, the only path an unpaired connection may use (protocol.md
/// 5.1, 6.1).
pub const SESSION_PATH: &str = "/v1/session";

/// How long a closing connection may take to flush its GOAWAY before it is dropped.
const CLOSE_GRACE: Duration = Duration::from_secs(1);

/// A boxed request handler future.
pub type HandlerFuture = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

/// What the host plugs into the server.
pub trait Router: Send + Sync + 'static {
    /// A connection finished its TLS and HTTP/2 handshakes. Pinned or unpaired, see
    /// [`Connection::is_trusted`].
    fn connection_opened(&self, conn: &Arc<Connection>);

    /// Handles one request. The server has already applied the SR-13 gate: an unpaired
    /// connection only reaches `POST /v1/session` here.
    fn handle(
        &self,
        conn: Arc<Connection>,
        req: Request<RecvStream>,
        respond: SendResponse<Bytes>,
    ) -> HandlerFuture;

    /// The connection has ended.
    fn connection_closed(&self, conn: &Arc<Connection>);
}

/// The TLS side of the server: the protocol.md 3.1 configuration plus the pin lookup and
/// "Add device" flag it was built with (needed again after the handshake).
#[derive(Clone)]
pub struct TlsAcceptor {
    /// From [`tls::build_server_config`].
    pub config: Arc<rustls::ServerConfig>,
    /// The pins the verifier uses.
    pub pins: Arc<dyn PinLookup>,
    /// The "Add device" window flag (protocol.md 6.1).
    pub window: Arc<AtomicBool>,
}

/// True if `port` is one `PlayDrop` must never bind (protocol.md section 1).
pub fn is_forbidden_port(port: u16) -> bool {
    FORBIDDEN_PORTS.iter().any(|r| r.contains(&port))
}

/// A bound, not yet running listener.
#[derive(Debug)]
pub struct Server {
    listener: TcpListener,
    local: SocketAddr,
    limits: Limits,
}

impl Server {
    /// Binds `addr` exclusively (SR-31): never `SO_REUSEPORT`, so a second process cannot
    /// share the port; `SO_REUSEADDR` on Unix only lets a restarted host rebind over
    /// `TIME_WAIT` sockets, which on Linux never admits a second listener. A held port is
    /// [`Error::PortHeld`]; there is no fallback port (protocol.md section 1). Forbidden
    /// ports are refused with [`Error::BadRequest`]. An unspecified IPv6 address binds
    /// dual-stack.
    pub fn bind(addr: SocketAddr, limits: Limits) -> Result<Self> {
        use socket2::{Domain, Protocol, Socket, Type};

        if is_forbidden_port(addr.port()) {
            return Err(Error::BadRequest(format!(
                "port {} is reserved for another app (protocol.md section 1)",
                addr.port()
            )));
        }
        let socket = Socket::new(Domain::for_address(addr), Type::STREAM, Some(Protocol::TCP))?;
        if let SocketAddr::V6(v6) = addr {
            if v6.ip().is_unspecified() {
                socket.set_only_v6(false)?;
            }
        }
        #[cfg(unix)]
        socket.set_reuse_address(true)?;
        // TODO(SR-31): on Windows set SO_EXCLUSIVEADDRUSE before bind; socket2 does not
        // expose it, so Phase 2 adds it through windows-sys.
        socket.set_nonblocking(true)?;
        match socket.bind(&addr.into()) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
                return Err(Error::PortHeld { port: addr.port() })
            }
            Err(e) => return Err(e.into()),
        }
        socket.listen(1024)?;
        let listener = TcpListener::from_std(socket.into())?;
        let local = listener.local_addr()?;
        Ok(Self {
            listener,
            local,
            limits,
        })
    }

    /// The bound address (the real port when bound to port 0).
    pub fn local_addr(&self) -> SocketAddr {
        self.local
    }

    /// Accepts connections until `shutdown` turns true. Each TCP connection must get a slot
    /// in the per-IP and total counters and a handshake permit (SR-15); excess connections
    /// are dropped at once. Unpaired connections also take a slot in the 3-per-IP unpaired
    /// counter (protocol.md 6.1).
    pub async fn run(
        self,
        tls: TlsAcceptor,
        router: Arc<dyn Router>,
        mut shutdown: watch::Receiver<bool>,
    ) {
        let counter = ConnectionCounter::from_limits(&self.limits);
        let unpaired = ConnectionCounter::new(
            self.limits.max_unpaired_per_ip,
            self.limits.max_connections_total,
        );
        let gate = HandshakeGate::new(self.limits.max_pending_handshakes);
        let limits = Arc::new(self.limits);
        let mut conns = tokio::task::JoinSet::new();
        loop {
            let accepted = tokio::select! {
                biased;
                () = stopped(&mut shutdown) => break,
                r = self.listener.accept() => r,
                Some(_) = conns.join_next(), if !conns.is_empty() => continue,
            };
            let (tcp, remote) = match accepted {
                Ok(pair) => pair,
                Err(e) => {
                    tracing::debug!(error = %e, "accept failed");
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    continue;
                }
            };
            let permit = match counter.try_acquire(remote.ip()) {
                Ok(p) => p,
                Err(why) => {
                    tracing::debug!(%why, "connection refused by limits");
                    continue;
                }
            };
            let Some(hs) = gate.try_acquire() else {
                tracing::debug!("too many handshakes in progress; connection dropped");
                continue;
            };
            let ctx = ConnCtx {
                tls: tls.clone(),
                router: Arc::clone(&router),
                limits: Arc::clone(&limits),
                unpaired: unpaired.clone(),
                shutdown: shutdown.clone(),
            };
            conns.spawn(async move {
                let _permit = permit;
                serve_connection(tcp, remote, hs, ctx).await;
            });
        }
        // Stop listening first, then end every connection and wait for them, so a host
        // restarted on the same port and folders never overlaps with the old one.
        drop(self.listener);
        conns.shutdown().await;
    }
}

struct ConnCtx {
    tls: TlsAcceptor,
    router: Arc<dyn Router>,
    limits: Arc<Limits>,
    unpaired: ConnectionCounter,
    shutdown: watch::Receiver<bool>,
}

async fn serve_connection(
    tcp: TcpStream,
    remote: SocketAddr,
    handshake_permit: tokio::sync::OwnedSemaphorePermit,
    mut ctx: ConnCtx,
) {
    tune_socket(&tcp);
    let accepted = tls::accept(
        Arc::clone(&ctx.tls.config),
        ctx.tls.pins.as_ref(),
        &ctx.tls.window,
        tcp,
    )
    .await;
    let (stream, peer) = match accepted {
        Ok(v) => v,
        Err(e) => {
            // Unpinned peers outside "Add device" land here with no UI (protocol.md 6.1).
            tracing::debug!(%remote, error = %e, "TLS accept failed");
            return;
        }
    };
    let unpaired_permit = if peer.trust == PeerTrust::Unpaired {
        if let Ok(p) = ctx.unpaired.try_acquire(remote.ip()) {
            Some(p)
        } else {
            tracing::debug!(%remote, "too many unpaired connections from this address");
            return;
        }
    } else {
        None
    };
    let exporter = match tls::exporter(stream.get_ref().1) {
        Ok(e) => e,
        Err(e) => {
            tracing::debug!(error = %e, "exporter failed");
            return;
        }
    };
    let handshake = server_builder(&ctx.limits).handshake::<_, Bytes>(stream);
    let mut h2conn = match tokio::time::timeout(ctx.limits.handshake_timeout, handshake).await {
        Ok(Ok(c)) => c,
        Ok(Err(e)) => {
            tracing::debug!(error = %e, "HTTP/2 handshake failed");
            return;
        }
        Err(_) => {
            tracing::debug!("HTTP/2 handshake timed out");
            return;
        }
    };
    drop(handshake_permit);
    let ping = h2conn.ping_pong();
    let conn = Connection::new(
        Direction::Inbound,
        peer,
        remote,
        exporter,
        None,
        ping,
        ctx.limits.max_resets_per_second,
    );
    tracing::debug!(peer = %short(&conn.device_id()), trusted = conn.is_trusted(), "inbound connection");
    ctx.router.connection_opened(&conn);
    let mut close_rx = conn.close_requested();
    loop {
        tokio::select! {
            biased;
            () = stopped(&mut ctx.shutdown) => break,
            next = h2conn.accept() => match next {
                Some(Ok((req, respond))) => route(&ctx.router, &conn, req, respond),
                Some(Err(e)) => {
                    tracing::debug!(error = %e, "HTTP/2 connection error");
                    break;
                }
                None => break,
            },
            _ = close_rx.changed() => {
                let reason = *close_rx.borrow();
                match reason {
                    // Host shutdown: drop at once, no grace period.
                    Some(CloseReason::Shutdown) => break,
                    Some(CloseReason::ResetFlood) => h2conn.abrupt_shutdown(Reason::ENHANCE_YOUR_CALM),
                    Some(CloseReason::Protocol) => h2conn.abrupt_shutdown(Reason::PROTOCOL_ERROR),
                    // GOAWAY NO_ERROR. `h2` cannot attach the `duplicate_connection` debug
                    // data of protocol.md 3.3; see the implementation notes.
                    _ => h2conn.graceful_shutdown(),
                }
                let _ = tokio::time::timeout(
                    CLOSE_GRACE,
                    std::future::poll_fn(|cx| h2conn.poll_closed(cx)),
                )
                .await;
                break;
            }
        }
    }
    drop(h2conn);
    drop(unpaired_permit);
    conn.mark_closed();
    ctx.router.connection_closed(&conn);
}

/// Applies the SR-13 gate, then hands the request to the router on its own task.
fn route(
    router: &Arc<dyn Router>,
    conn: &Arc<Connection>,
    req: Request<RecvStream>,
    mut respond: SendResponse<Bytes>,
) {
    let is_session = req.method() == Method::POST && req.uri().path() == SESSION_PATH;
    if !conn.is_trusted() && !is_session {
        tokio::spawn(async move {
            let err = HttpError::new(StatusCode::FORBIDDEN, ErrorCode::NotPaired, "");
            let _ = respond_error(&mut respond, &err).await;
        });
        return;
    }
    tokio::spawn(router.handle(Arc::clone(conn), req, respond));
}

//! The listening side: exclusive bind, connection limits, TLS accept, HTTP/2 accept and
//! request routing. Implements protocol.md section 1 (port rules), 3.1, 3.5 (the probe
//! endpoint, and no connection counted as a replacement before its first other request),
//! 6.1 and section 10 (SR-11, SR-13, SR-15, SR-31).

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use h2::server::SendResponse;
use h2::{Reason, RecvStream};
use http::{Method, Request, StatusCode};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;

use super::connection::{short, ActivityIo, CloseReason, Connection, Direction, DRAIN_GRACE};
use super::path_id::{InterfaceLookup, SystemInterfaceLookup};
use super::probe;
use super::{respond_error, server_builder, stopped, tune_socket, HttpError};
use crate::constants::FORBIDDEN_PORTS;
use crate::identity::DeviceId;
use crate::session::{ConnectionCounter, ErrorCode, HandshakeGate, Limits};
use crate::tls::{self, PeerTrust, PinLookup};
use crate::{Error, Result};

/// Path of the session stream, the only path an unpaired connection may use (protocol.md
/// 5.1, 6.1).
pub const SESSION_PATH: &str = "/v1/session";

/// How long a closing connection may take to flush its GOAWAY before it is dropped.
const CLOSE_GRACE: Duration = Duration::from_secs(1);

/// The same for an unpaired peer. *Choice:* 300 ms: with the 200 ms the host waits for the
/// `unpair` frame to go out, the connection is gone well inside the 1 s of SR-14.
const UNPAIR_CLOSE_GRACE: Duration = Duration::from_millis(300);

/// A boxed request handler future.
pub type HandlerFuture = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

/// What the host plugs into the server.
pub trait Router: Send + Sync + 'static {
    /// A connection finished its TLS and HTTP/2 handshakes. Pinned or unpaired, see
    /// [`Connection::is_trusted`].
    fn connection_opened(&self, conn: &Arc<Connection>);

    /// The connection carried its first request other than a path probe (protocol.md 3.5:
    /// a dialler may hold a second connection for probing only, and that one must not
    /// replace the connection in use under 3.3). Called at most once per connection,
    /// after [`Router::connection_opened`] and before that request is handled; never for
    /// a connection that only probed. A server without path probes calls it right after
    /// the handshake.
    fn connection_in_use(&self, conn: &Arc<Connection>) {
        let _ = conn;
    }

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
    path_probe: bool,
    not_in_use_timeout: Duration,
    interfaces: Arc<dyn InterfaceLookup>,
}

/// The listening socket before it is bound, as [`ListenerSetup::before_bind`] sees it.
pub use socket2::Socket as ListenSocket;

/// A host hook that adjusts the listening socket before it is bound (SR-31). A Windows
/// daemon sets `SO_EXCLUSIVEADDRUSE` through it: that needs a raw `setsockopt`, which is
/// `unsafe`, and this crate forbids `unsafe`.
pub trait ListenerSetup: Send + Sync + fmt::Debug {
    /// Called once per bind, after the core's own options and before `bind`. An error
    /// fails the bind with [`Error::Io`].
    fn before_bind(&self, socket: &ListenSocket) -> std::io::Result<()>;
}

/// True when a bind failed because another socket holds the port. Windows reports a port
/// held with `SO_EXCLUSIVEADDRUSE`, or by another user, as `WSAEACCES` as often as
/// `WSAEADDRINUSE`.
fn is_port_held(e: &std::io::Error) -> bool {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Networking::WinSock::{WSAEACCES, WSAEADDRINUSE};
        if matches!(e.raw_os_error(), Some(c) if c == WSAEACCES || c == WSAEADDRINUSE) {
            return true;
        }
    }
    e.kind() == std::io::ErrorKind::AddrInUse
}

impl Server {
    /// Binds `addr` exclusively (SR-31): never `SO_REUSEPORT`, so a second process cannot
    /// share the port; `SO_REUSEADDR` on Unix only lets a restarted host rebind over
    /// `TIME_WAIT` sockets, which on Linux never admits a second listener. On Windows the
    /// host passes a `setup` that sets `SO_EXCLUSIVEADDRUSE`. A held port is
    /// [`Error::PortHeld`]; there is no fallback port (protocol.md section 1). Forbidden
    /// ports are refused with [`Error::BadRequest`]. An unspecified IPv6 address binds
    /// dual-stack.
    pub fn bind(
        addr: SocketAddr,
        limits: Limits,
        setup: Option<&dyn ListenerSetup>,
    ) -> Result<Self> {
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
        // Exclusive use on Windows (SR-31) comes from the host through this hook
        // (`HostConfig::listener_setup`); socket2 does not expose SO_EXCLUSIVEADDRUSE.
        if let Some(setup) = setup {
            setup.before_bind(&socket)?;
        }
        socket.set_nonblocking(true)?;
        match socket.bind(&addr.into()) {
            Ok(()) => {}
            Err(e) if is_port_held(&e) => return Err(Error::PortHeld { port: addr.port() }),
            Err(e) => return Err(e.into()),
        }
        socket.listen(1024)?;
        let listener = TcpListener::from_std(socket.into())?;
        let local = listener.local_addr()?;
        Ok(Self {
            listener,
            local,
            limits,
            path_probe: true,
            not_in_use_timeout: NOT_IN_USE_TIMEOUT,
            interfaces: Arc::new(SystemInterfaceLookup),
        })
    }

    /// Whether to serve `GET /v1/probe` and report a connection in use
    /// ([`Router::connection_in_use`]) only at its first other request (protocol.md 3.5).
    /// On by default. Off, the server behaves as one from before 3.5: the probe path is
    /// the router's (`404`) and every connection is in use right after its handshake.
    /// Hosts turn it off only to test compatibility.
    #[must_use]
    pub fn with_path_probe(mut self, on: bool) -> Self {
        self.path_probe = on;
        self
    }

    /// How long a connection may stay open without a request other than a probe. The
    /// default is [`NOT_IN_USE_TIMEOUT`]; tests shorten it.
    #[must_use]
    pub fn with_not_in_use_timeout(mut self, timeout: Duration) -> Self {
        self.not_in_use_timeout = timeout;
        self
    }

    /// How to find the interface behind a connection, for the id in a probe's head
    /// (protocol.md 3.5). The default is the system's list; tests supply their own.
    #[must_use]
    pub fn with_interface_lookup(mut self, lookup: Arc<dyn InterfaceLookup>) -> Self {
        self.interfaces = lookup;
        self
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
        let unused = UnusedBook::default();
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
                path_probe: self.path_probe,
                not_in_use_timeout: self.not_in_use_timeout,
                unused: unused.clone(),
                interfaces: Arc::clone(&self.interfaces),
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
    path_probe: bool,
    not_in_use_timeout: Duration,
    unused: UnusedBook,
    interfaces: Arc<dyn InterfaceLookup>,
}

/// How long a connection may stay open without a request other than a probe
/// (protocol.md 3.5, SR-15). *Choice:* a dialler's held alternatives are probed and
/// either put in use or closed within a few seconds; 20 s leaves room for a slow phone.
pub const NOT_IN_USE_TIMEOUT: Duration = Duration::from_secs(20);

/// Most connections from one peer that are open but not yet in use (protocol.md 3.5,
/// SR-15); a newer one closes the oldest. A dialler holds at most two alternatives.
pub const MAX_NOT_IN_USE_PER_PEER: usize = 2;

/// Connections per peer that have carried no request other than a probe, oldest first,
/// so a pinned peer cannot fill the per-address and total limits with them (SR-15).
#[derive(Clone, Default)]
struct UnusedBook(Arc<std::sync::Mutex<HashMap<DeviceId, VecDeque<Arc<Connection>>>>>);

impl UnusedBook {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<DeviceId, VecDeque<Arc<Connection>>>> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Adds a fresh connection; closes the oldest beyond the per-peer cap.
    fn add(&self, conn: &Arc<Connection>) {
        let evicted = {
            let mut book = self.lock();
            let list = book.entry(conn.device_id()).or_default();
            list.push_back(Arc::clone(conn));
            if list.len() > MAX_NOT_IN_USE_PER_PEER {
                list.pop_front()
            } else {
                None
            }
        };
        if let Some(old) = evicted {
            tracing::debug!(peer = %short(&old.device_id()), "too many connections not in use; oldest closed");
            // Nothing in use runs on it, so the short close grace is enough.
            old.close(CloseReason::Dead);
        }
    }

    /// Forgets a connection that went into use or ended.
    fn remove(&self, conn: &Connection) {
        let mut book = self.lock();
        let id = conn.device_id();
        if let Some(list) = book.get_mut(&id) {
            list.retain(|c| c.id() != conn.id());
            if list.is_empty() {
                book.remove(&id);
            }
        }
    }
}

async fn serve_connection(
    tcp: TcpStream,
    remote: SocketAddr,
    handshake_permit: tokio::sync::OwnedSemaphorePermit,
    mut ctx: ConnCtx,
) {
    tune_socket(&tcp);
    let local = tcp.local_addr().ok();
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
    let bytes_read = Arc::new(AtomicU64::new(0));
    let handshake = server_builder(&ctx.limits)
        .handshake::<_, Bytes>(ActivityIo::new(stream, Arc::clone(&bytes_read)));
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
        local,
        exporter,
        None,
        ping,
        ctx.limits.max_resets_per_second,
        bytes_read,
    );
    tracing::debug!(peer = %short(&conn.device_id()), trusted = conn.is_trusted(), "inbound connection");
    accept_requests(&mut h2conn, &conn, &mut ctx).await;
    drop(h2conn);
    drop(unpaired_permit);
    ctx.unused.remove(&conn);
    conn.mark_closed();
    ctx.router.connection_closed(&conn);
}

/// Tells the router about a connection, then hands its requests on until it ends or the host
/// asks to close it.
async fn accept_requests<T>(
    h2conn: &mut h2::server::Connection<T, Bytes>,
    conn: &Arc<Connection>,
    ctx: &mut ConnCtx,
) where
    T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    // Before the router can ask for a close: a `watch` receiver does not report a value set
    // before it subscribed, and `connection_opened` closes a connection unpaired meanwhile.
    let mut close_rx = conn.close_requested();
    ctx.router.connection_opened(conn);
    // protocol.md 3.5: the router hears the connection is in use at its first request
    // other than a probe, so one held only for probing never replaces another (3.3).
    let mut in_use = !ctx.path_probe;
    if in_use {
        ctx.router.connection_in_use(conn);
    } else {
        ctx.unused.add(conn);
    }
    let idle_deadline = tokio::time::Instant::now() + ctx.not_in_use_timeout;
    let mut idle_armed = true;
    loop {
        tokio::select! {
            biased;
            () = stopped(&mut ctx.shutdown) => break,
            () = tokio::time::sleep_until(idle_deadline), if idle_armed && !in_use => {
                tracing::debug!(peer = %short(&conn.device_id()), "connection never used; closed");
                idle_armed = false;
                conn.close(CloseReason::Dead);
            }
            // Before `accept`: once a close is asked for (unpair, SR-14), no further
            // request reaches the router except as `shut_down` allows.
            _ = close_rx.changed() => {
                shut_down(h2conn, &mut close_rx, ctx, conn, &mut in_use).await;
                break;
            }
            next = h2conn.accept() => match next {
                Some(Ok((req, respond))) => dispatch(ctx, conn, &mut in_use, req, respond),
                Some(Err(e)) => {
                    tracing::debug!(error = %e, "HTTP/2 connection error");
                    break;
                }
                None => break,
            },
        }
    }
}

/// Ends a connection the host asked to close: at once at shutdown, with an error after
/// a flood or protocol error, otherwise with GOAWAY and a grace for open streams. A
/// connection replaced by a newer one from the same dialler still serves what the dialler
/// sent before it saw the GOAWAY (protocol.md 3.5), so such a request is answered instead
/// of hanging for the grace; any other closing connection serves nothing more (SR-14).
async fn shut_down<T>(
    h2conn: &mut h2::server::Connection<T, Bytes>,
    close_rx: &mut watch::Receiver<Option<CloseReason>>,
    ctx: &ConnCtx,
    conn: &Arc<Connection>,
    in_use: &mut bool,
) where
    T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let reason = *close_rx.borrow_and_update();
    match reason {
        // Host shutdown: drop at once, no grace period.
        Some(CloseReason::Shutdown) => return,
        Some(CloseReason::ResetFlood) => h2conn.abrupt_shutdown(Reason::ENHANCE_YOUR_CALM),
        Some(CloseReason::Protocol) => h2conn.abrupt_shutdown(Reason::PROTOCOL_ERROR),
        // GOAWAY NO_ERROR. `h2` cannot attach the `duplicate_connection` debug data of
        // protocol.md 3.3; see the implementation notes.
        _ => h2conn.graceful_shutdown(),
    }
    let grace = match reason {
        Some(r) if r.is_graceful() => DRAIN_GRACE,
        Some(CloseReason::Unpaired) => UNPAIR_CLOSE_GRACE,
        _ => CLOSE_GRACE,
    };
    let serving = reason.is_some_and(CloseReason::is_graceful);
    let deadline = tokio::time::sleep(grace);
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            () = &mut deadline => return,
            // A harder close (unpair, shutdown, dead) while open streams finish: drop
            // the connection at once (SR-14).
            r = close_rx.changed() => {
                if r.is_err() || !close_rx.borrow().is_some_and(CloseReason::is_graceful) {
                    return;
                }
            }
            next = h2conn.accept() => match next {
                Some(Ok((req, respond))) => {
                    if serving {
                        dispatch(ctx, conn, in_use, req, respond);
                    }
                }
                Some(Err(_)) | None => return,
            },
        }
    }
}

/// Sends a probe to [`route_probe`] and anything else to the router, telling the router
/// the connection is in use first if this is its first such request (protocol.md 3.5).
fn dispatch(
    ctx: &ConnCtx,
    conn: &Arc<Connection>,
    in_use: &mut bool,
    req: Request<RecvStream>,
    respond: SendResponse<Bytes>,
) {
    if ctx.path_probe && probe::is_probe(&req) {
        route_probe(conn, &ctx.interfaces, req, respond);
        return;
    }
    if !*in_use {
        *in_use = true;
        ctx.unused.remove(conn);
        ctx.router.connection_in_use(conn);
    }
    route(&ctx.router, conn, req, respond);
}

/// `GET /v1/probe` (protocol.md 3.5): pinned connections only (SR-13), then the probe
/// limits of SR-15. So only a pinned peer ever sees a path id.
fn route_probe(
    conn: &Arc<Connection>,
    interfaces: &Arc<dyn InterfaceLookup>,
    req: Request<RecvStream>,
    mut respond: SendResponse<Bytes>,
) {
    if !conn.is_trusted() {
        tokio::spawn(async move {
            let err = HttpError::new(StatusCode::FORBIDDEN, ErrorCode::NotPaired, "");
            let _ = respond_error(&mut respond, &err).await;
        });
        return;
    }
    tokio::spawn(probe::serve(
        Arc::clone(conn),
        Arc::clone(interfaces),
        req,
        respond,
    ));
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

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, SocketAddr};
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;

    /// Records that it ran on a socket not yet bound, or fails on purpose.
    #[derive(Debug, Default)]
    struct Probe {
        fail: bool,
        saw_unbound: AtomicBool,
    }

    impl ListenerSetup for Probe {
        fn before_bind(&self, socket: &ListenSocket) -> std::io::Result<()> {
            let port = socket
                .local_addr()
                .ok()
                .and_then(|a| a.as_socket())
                .map_or(0, |a| a.port());
            self.saw_unbound.store(port == 0, Ordering::SeqCst);
            if self.fail {
                return Err(std::io::Error::other("setup refused"));
            }
            Ok(())
        }
    }

    fn loopback() -> SocketAddr {
        SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 0)
    }

    /// SR-31: the host hook runs before the bind, and its error fails the bind.
    #[tokio::test]
    async fn listener_setup_runs_before_bind_and_can_fail_it() {
        let probe = Probe::default();
        let server = Server::bind(loopback(), Limits::default(), Some(&probe)).unwrap();
        assert!(
            probe.saw_unbound.load(Ordering::SeqCst),
            "hook ran before bind"
        );
        assert_ne!(server.local_addr().port(), 0);

        let failing = Probe {
            fail: true,
            ..Probe::default()
        };
        let r = Server::bind(loopback(), Limits::default(), Some(&failing));
        assert!(
            matches!(&r, Err(Error::Io(e)) if e.to_string().contains("setup refused")),
            "{:?}",
            r.as_ref().err()
        );
    }

    /// A second bind on a held port is `PortHeld` without any hook (Linux: no
    /// `SO_REUSEPORT`).
    #[tokio::test]
    async fn second_bind_is_port_held() {
        let first = Server::bind(loopback(), Limits::default(), None).unwrap();
        let addr = first.local_addr();
        let second = Server::bind(addr, Limits::default(), None);
        assert!(
            matches!(&second, Err(Error::PortHeld { port }) if *port == addr.port()),
            "{:?}",
            second.as_ref().err()
        );
    }
}

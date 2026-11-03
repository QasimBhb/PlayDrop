//! The dialling side: concurrent, staggered attempts over candidate addresses, the TLS
//! client handshake with the expected SPKI, then the HTTP/2 client handshake; and the
//! losing candidates held for a path check. Implements protocol.md 3.1, 3.2, 3.5 and
//! section 10 (SR-1, SR-11).

use std::net::SocketAddr;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use tokio::net::TcpStream;
use tokio::task::JoinSet;

use super::connection::{ActivityIo, CloseReason, Connection, Direction, DRAIN_GRACE};
use super::{client_builder, tune_socket};
use crate::session::Limits;
use crate::tls::{self, PeerIdentity, EXPORTER_LEN};

/// Delay between starting attempts on successive candidates (protocol.md 3.2).
pub const STAGGER: Duration = Duration::from_millis(250);

/// TCP connect timeout per candidate. *Choice:* a LAN answers in milliseconds; 5 s covers
/// a waking phone without stalling the caller for long.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// How to dial.
#[derive(Clone)]
pub struct DialOptions {
    /// From [`tls::build_client_config`] (pinned peer) or
    /// [`tls::build_client_config_unpaired`] (a device picked in "Add device", or a QR code).
    pub config: Arc<rustls::ClientConfig>,
    /// The pinned SPKI hash, or `None` for the unpaired config.
    pub expected_spki: Option<[u8; 32]>,
    /// Gap between attempts (protocol.md 3.2: 250 ms).
    pub stagger: Duration,
    /// TCP connect timeout per attempt.
    pub connect_timeout: Duration,
    /// SR-15 limits (header list size, resets per second).
    pub limits: Limits,
}

impl DialOptions {
    /// Options with the protocol defaults.
    pub fn new(config: Arc<rustls::ClientConfig>, expected_spki: Option<[u8; 32]>) -> Self {
        Self {
            config,
            expected_spki,
            stagger: STAGGER,
            connect_timeout: CONNECT_TIMEOUT,
            limits: Limits::default(),
        }
    }
}

/// Why a dial failed.
#[derive(Debug, thiserror::Error)]
pub enum DialError {
    /// Nothing to dial.
    #[error("no candidate addresses")]
    NoCandidates,
    /// Every candidate failed; the text lists each address and its error, for diagnostics.
    #[error("every candidate failed: {0}")]
    AllFailed(String),
}

impl From<DialError> for crate::Error {
    fn from(e: DialError) -> Self {
        crate::Error::Io(std::io::Error::new(
            std::io::ErrorKind::ConnectionRefused,
            e.to_string(),
        ))
    }
}

type TlsIo = tokio_rustls::client::TlsStream<TcpStream>;

/// How long after a dial starts the other candidates may still finish TLS and be held
/// for a path check (protocol.md 3.5). *Choice:* 1.5 s lets the first six candidates
/// (250 ms apart, 3.2) start and finish a LAN handshake, which takes tens of
/// milliseconds; a path still handshaking after that is not the fast one. Held
/// connections stay well inside the peer's 10 s handshake timeout (section 10).
pub const ALTERNATIVE_GRACE: Duration = Duration::from_millis(1500);

/// Most alternatives held per dial. *Choice:* two covers Wi-Fi, Ethernet and a USB
/// tether at once, and with the connection in use stays under the peer's limit of four
/// connections per source address (SR-15).
pub const MAX_ALTERNATIVES: usize = 2;

/// A candidate that finished TLS with the expected SPKI but has not started HTTP/2.
///
/// Held this way the peer has not yet seen a connection at all (its server waits for
/// the HTTP/2 preface), so a peer from before protocol.md 3.5, which lets a new
/// connection replace the old one as soon as it is up (3.3), is not disturbed when the
/// alternative is dropped. Dropping it closes the socket.
pub struct HeldTls {
    remote: SocketAddr,
    peer: PeerIdentity,
    exporter: [u8; EXPORTER_LEN],
    stream: TlsIo,
}

impl std::fmt::Debug for HeldTls {
    /// Leaves out the exporter value (SR-32).
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HeldTls")
            .field("remote", &self.remote)
            .finish_non_exhaustive()
    }
}

impl HeldTls {
    /// The address this candidate reached.
    pub fn remote_addr(&self) -> SocketAddr {
        self.remote
    }

    /// Runs the HTTP/2 handshake and starts the connection's driver.
    pub async fn finish(self, limits: &Limits) -> Result<Arc<Connection>, String> {
        let bytes_read = Arc::new(AtomicU64::new(0));
        let (send, conn) = tokio::time::timeout(
            limits.handshake_timeout,
            client_builder(limits)
                .handshake::<_, Bytes>(ActivityIo::new(self.stream, Arc::clone(&bytes_read))),
        )
        .await
        .map_err(|_| "HTTP/2 handshake timed out".to_owned())?
        .map_err(|e| format!("HTTP/2 handshake: {e}"))?;
        Ok(spawn_driver(
            Established {
                remote: self.remote,
                peer: self.peer,
                exporter: self.exporter,
                send,
                conn,
                bytes_read,
            },
            limits,
        ))
    }
}

/// The other candidates of a dial, still finishing TLS in the background until the
/// grace ends (protocol.md 3.5). Dropping it stops them.
#[derive(Debug, Default)]
pub struct Alternatives {
    task: Option<tokio::task::JoinHandle<Vec<HeldTls>>>,
}

impl Alternatives {
    /// Waits for the grace to end and returns every candidate that finished TLS by then,
    /// at most [`MAX_ALTERNATIVES`], in the order they finished. Dropping the waiting
    /// future stops the attempts.
    pub async fn collect(mut self) -> Vec<HeldTls> {
        let held = match self.task.as_mut() {
            Some(t) => t.await.unwrap_or_default(),
            None => Vec::new(),
        };
        self.task = None;
        held
    }
}

impl Drop for Alternatives {
    fn drop(&mut self) {
        if let Some(t) = self.task.take() {
            t.abort();
        }
    }
}

struct Established {
    remote: SocketAddr,
    peer: PeerIdentity,
    exporter: [u8; EXPORTER_LEN],
    send: h2::client::SendRequest<Bytes>,
    conn: h2::client::Connection<ActivityIo<TlsIo>, Bytes>,
    bytes_read: Arc<AtomicU64>,
}

/// Dials every candidate at once, staggered by [`DialOptions::stagger`] (protocol.md 3.2):
/// the first to complete TLS with the expected SPKI and then the HTTP/2 handshake wins and
/// the others are dropped. The caller must only pass addresses SR-1 allows: a pinned peer's
/// verified or last-known addresses, a device picked in "Add device", or a scanned QR code.
pub async fn dial(
    candidates: &[SocketAddr],
    opts: &DialOptions,
) -> Result<Arc<Connection>, DialError> {
    dial_with_alternatives(candidates, opts, Duration::ZERO)
        .await
        .map(|(c, _)| c)
}

/// [`dial`], but the losing candidates keep going until `grace` after the start: those
/// that finish TLS by then are returned through [`Alternatives`], held before HTTP/2, for
/// a path check (protocol.md 3.5). The winner is returned as soon as it is up, so a cold
/// send waits no longer than with [`dial`].
pub async fn dial_with_alternatives(
    candidates: &[SocketAddr],
    opts: &DialOptions,
    grace: Duration,
) -> Result<(Arc<Connection>, Alternatives), DialError> {
    let deadline = tokio::time::Instant::now() + grace;
    let mut set = start_attempts(candidates, opts)?;
    let mut failures = Vec::new();
    while let Some(joined) = set.join_next().await {
        match joined {
            Ok(Ok(held)) => match held.finish(&opts.limits).await {
                Ok(conn) => {
                    if grace.is_zero() || set.is_empty() {
                        set.abort_all();
                        return Ok((conn, Alternatives::default()));
                    }
                    let task = tokio::spawn(collect_held(set, deadline));
                    return Ok((conn, Alternatives { task: Some(task) }));
                }
                Err(e) => failures.push(e),
            },
            Ok(Err(e)) => failures.push(e),
            Err(e) => failures.push(format!("attempt task failed: {e}")),
        }
    }
    Err(DialError::AllFailed(failures.join("; ")))
}

/// Dials every candidate as [`dial`] does but keeps none: every candidate that finishes
/// TLS within `grace` is returned held before HTTP/2, at most [`MAX_ALTERNATIVES`], for a
/// path check of a live peer (protocol.md 3.5). SR-1 applies to the candidates as for
/// [`dial`].
pub async fn dial_held(
    candidates: &[SocketAddr],
    opts: &DialOptions,
    grace: Duration,
) -> Vec<HeldTls> {
    let deadline = tokio::time::Instant::now() + grace;
    match start_attempts(candidates, opts) {
        Ok(set) => collect_held(set, deadline).await,
        Err(_) => Vec::new(),
    }
}

type Attempts = JoinSet<Result<HeldTls, String>>;

/// Starts one TLS attempt per distinct candidate, `stagger` apart.
fn start_attempts(candidates: &[SocketAddr], opts: &DialOptions) -> Result<Attempts, DialError> {
    let mut unique: Vec<SocketAddr> = Vec::with_capacity(candidates.len());
    for c in candidates {
        if !unique.contains(c) {
            unique.push(*c);
        }
    }
    if unique.is_empty() {
        return Err(DialError::NoCandidates);
    }
    let mut set = JoinSet::new();
    for (i, addr) in unique.iter().copied().enumerate() {
        let opts = opts.clone();
        let delay = opts.stagger * u32::try_from(i).unwrap_or(u32::MAX);
        set.spawn(async move {
            tokio::time::sleep(delay).await;
            attempt(addr, &opts)
                .await
                .map_err(|e| format!("{addr}: {e}"))
        });
    }
    Ok(set)
}

/// Collects attempts that finish TLS until `deadline`, at most [`MAX_ALTERNATIVES`],
/// then stops the rest.
async fn collect_held(mut set: Attempts, deadline: tokio::time::Instant) -> Vec<HeldTls> {
    let mut held = Vec::new();
    while held.len() < MAX_ALTERNATIVES {
        tokio::select! {
            biased;
            joined = set.join_next() => match joined {
                Some(Ok(Ok(h))) => held.push(h),
                Some(_) => {}
                None => break,
            },
            () = tokio::time::sleep_until(deadline) => break,
        }
    }
    set.abort_all();
    held
}

/// TCP connect and the TLS handshake with the expected SPKI (protocol.md 3.1); HTTP/2
/// comes later, in [`HeldTls::finish`].
async fn attempt(addr: SocketAddr, opts: &DialOptions) -> Result<HeldTls, String> {
    let tcp = tokio::time::timeout(opts.connect_timeout, TcpStream::connect(addr))
        .await
        .map_err(|_| "TCP connect timed out".to_owned())?
        .map_err(|e| format!("TCP connect: {e}"))?;
    tune_socket(&tcp);
    let (stream, peer) = tls::connect_tcp(Arc::clone(&opts.config), opts.expected_spki, tcp)
        .await
        .map_err(|e| e.to_string())?;
    let exporter = tls::exporter(stream.get_ref().1).map_err(|e| e.to_string())?;
    Ok(HeldTls {
        remote: addr,
        peer,
        exporter,
        stream,
    })
}

/// Wraps the winner in a [`Connection`] and drives it on its own task until it ends or a
/// close is requested.
fn spawn_driver(est: Established, limits: &Limits) -> Arc<Connection> {
    let Established {
        remote,
        peer,
        exporter,
        send,
        mut conn,
        bytes_read,
    } = est;
    let ping = conn.ping_pong();
    let connection = Connection::new(
        Direction::Outbound,
        peer,
        remote,
        exporter,
        Some(send),
        ping,
        limits.max_resets_per_second,
        bytes_read,
    );
    let handle = Arc::clone(&connection);
    let mut close_rx = connection.close_requested();
    tokio::spawn(async move {
        let drain = tokio::select! {
            r = &mut conn => {
                if let Err(e) = r {
                    tracing::debug!(error = %e, "outbound connection ended");
                }
                false
            }
            // `h2` gives a client no GOAWAY call; dropping the connection closes the socket.
            _ = close_rx.changed() => *close_rx.borrow() == Some(CloseReason::Drain),
        };
        if drain {
            // The request handle is gone, so `h2` ends the connection once its open
            // streams are done (protocol.md 3.5); past the grace it is dropped anyway, and
            // a harder close (unpair, shutdown) drops it at once.
            let grace = tokio::time::sleep(DRAIN_GRACE);
            tokio::pin!(grace);
            loop {
                tokio::select! {
                    _ = &mut conn => break,
                    () = &mut grace => break,
                    r = close_rx.changed() => {
                        if r.is_err() || !close_rx.borrow().is_some_and(CloseReason::is_graceful) {
                            break;
                        }
                    }
                }
            }
        }
        drop(conn);
        handle.mark_closed();
    });
    connection
}

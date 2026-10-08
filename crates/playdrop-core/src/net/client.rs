//! The dialling side: concurrent, staggered attempts over candidate addresses, the TLS
//! client handshake with the expected SPKI, then the HTTP/2 client handshake.
//! Implements protocol.md 3.1, 3.2 and section 10 (SR-1, SR-11).

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use tokio::net::TcpStream;
use tokio::task::JoinSet;

use super::connection::{Connection, Direction};
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

struct Established {
    remote: SocketAddr,
    peer: PeerIdentity,
    exporter: [u8; EXPORTER_LEN],
    send: h2::client::SendRequest<Bytes>,
    conn: h2::client::Connection<TlsIo, Bytes>,
}

/// Dials every candidate at once, staggered by [`DialOptions::stagger`] (protocol.md 3.2):
/// the first to complete TLS with the expected SPKI and the HTTP/2 handshake wins and the
/// others are dropped. The caller must only pass addresses SR-1 allows: a pinned peer's
/// verified or last-known addresses, a device picked in "Add device", or a scanned QR code.
pub async fn dial(
    candidates: &[SocketAddr],
    opts: &DialOptions,
) -> Result<Arc<Connection>, DialError> {
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
    let mut failures = Vec::new();
    while let Some(joined) = set.join_next().await {
        match joined {
            Ok(Ok(est)) => {
                set.abort_all();
                return Ok(spawn_driver(est, &opts.limits));
            }
            Ok(Err(e)) => failures.push(e),
            Err(e) => failures.push(format!("attempt task failed: {e}")),
        }
    }
    Err(DialError::AllFailed(failures.join("; ")))
}

async fn attempt(addr: SocketAddr, opts: &DialOptions) -> Result<Established, String> {
    let tcp = tokio::time::timeout(opts.connect_timeout, TcpStream::connect(addr))
        .await
        .map_err(|_| "TCP connect timed out".to_owned())?
        .map_err(|e| format!("TCP connect: {e}"))?;
    tune_socket(&tcp);
    let (stream, peer) = tls::connect_tcp(Arc::clone(&opts.config), opts.expected_spki, tcp)
        .await
        .map_err(|e| e.to_string())?;
    let exporter = tls::exporter(stream.get_ref().1).map_err(|e| e.to_string())?;
    let (send, conn) = tokio::time::timeout(
        opts.limits.handshake_timeout,
        client_builder(&opts.limits).handshake::<_, Bytes>(stream),
    )
    .await
    .map_err(|_| "HTTP/2 handshake timed out".to_owned())?
    .map_err(|e| format!("HTTP/2 handshake: {e}"))?;
    Ok(Established {
        remote: addr,
        peer,
        exporter,
        send,
        conn,
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
    );
    let handle = Arc::clone(&connection);
    let mut close_rx = connection.close_requested();
    tokio::spawn(async move {
        tokio::select! {
            r = &mut conn => {
                if let Err(e) = r {
                    tracing::debug!(error = %e, "outbound connection ended");
                }
            }
            // `h2` gives a client no GOAWAY call; dropping the connection closes the socket.
            _ = close_rx.changed() => {}
        }
        drop(conn);
        handle.mark_closed();
    });
    connection
}

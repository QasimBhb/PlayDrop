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
    /// Candidates never held for a path check (protocol.md 3.5): known to be on a path
    /// already measured, so not worth a held slot. They are dialled in their usual turn and
    /// can still win a dial, because one may be the way left to the peer.
    pub never_hold: Vec<SocketAddr>,
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
            never_hold: Vec::new(),
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
    local: Option<SocketAddr>,
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
                local: self.local,
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
    task: Option<tokio::task::JoinHandle<HeldDial>>,
}

impl Alternatives {
    /// Waits for the grace to end and returns every candidate that finished TLS by then,
    /// at most [`MAX_ALTERNATIVES`], in the order they finished. Dropping the waiting
    /// future stops the attempts.
    pub async fn collect(mut self) -> Vec<HeldTls> {
        let held = match self.task.as_mut() {
            Some(t) => t.await.unwrap_or_default(),
            None => HeldDial::default(),
        };
        self.task = None;
        held.held
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
    local: Option<SocketAddr>,
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
    let mut set = start_attempts(&plan_candidates(candidates, true), opts)?;
    let mut failures = Vec::new();
    while let Some(joined) = set.join_next().await {
        match joined {
            Ok(Ok(held)) => match held.finish(&opts.limits).await {
                Ok(conn) => {
                    if grace.is_zero() || set.is_empty() {
                        set.abort_all();
                        return Ok((conn, Alternatives::default()));
                    }
                    let never_hold = opts.never_hold.clone();
                    let task = tokio::spawn(collect_held(set, deadline, never_hold));
                    return Ok((conn, Alternatives { task: Some(task) }));
                }
                Err(e) => failures.push(e),
            },
            Ok(Err((addr, e))) => failures.push(format!("{addr}: {e}")),
            Err(e) => failures.push(format!("attempt task failed: {e}")),
        }
    }
    Err(DialError::AllFailed(failures.join("; ")))
}

/// What a dial for a path check found.
#[derive(Debug, Default)]
pub struct HeldDial {
    /// The candidates that finished TLS, in the order they finished.
    pub held: Vec<HeldTls>,
    /// The candidates whose attempt failed (refused, timed out, wrong key). A candidate in
    /// neither list was stopped first, by the grace or by the slots filling up, so it says
    /// nothing against that address.
    pub failed: Vec<SocketAddr>,
}

/// Dials every candidate as [`dial`] does but keeps none: every candidate that finishes
/// TLS within `grace` is returned held before HTTP/2, at most [`MAX_ALTERNATIVES`], for a
/// path check of a live peer (protocol.md 3.5). SR-1 applies to the candidates as for
/// [`dial`].
pub async fn dial_held(candidates: &[SocketAddr], opts: &DialOptions, grace: Duration) -> HeldDial {
    let deadline = tokio::time::Instant::now() + grace;
    match start_attempts(&plan_candidates(candidates, false), opts) {
        Ok(set) => collect_held(set, deadline, opts.never_hold.clone()).await,
        Err(_) => HeldDial::default(),
    }
}

type Attempts = JoinSet<Result<HeldTls, (SocketAddr, String)>>;

/// The canonical form of an address: an IPv4-mapped IPv6 address as plain IPv4, so
/// `[::ffff:a.b.c.d]:p` and `a.b.c.d:p` are one candidate (protocol.md 3.2). A dual-stack
/// listener reports IPv4 peers in the mapped form, which is how it gets into the last-known
/// list. Anything else comes back untouched, with its scope: `IpAddr::to_canonical` and
/// `SocketAddr::new` would lose the scope a link-local address cannot be dialled without.
pub fn canonical_addr(addr: SocketAddr) -> SocketAddr {
    match addr {
        SocketAddr::V6(v6) => v6
            .ip()
            .to_ipv4_mapped()
            .map_or(addr, |v4| SocketAddr::from((v4, v6.port()))),
        SocketAddr::V4(_) => addr,
    }
}

/// The candidates in canonical form, each once, in the order given.
pub fn canonical_unique(candidates: &[SocketAddr]) -> Vec<SocketAddr> {
    let mut unique: Vec<SocketAddr> = Vec::with_capacity(candidates.len());
    for c in candidates {
        let c = canonical_addr(*c);
        if !unique.contains(&c) {
            unique.push(c);
        }
    }
    unique
}

/// Which attempts start first, lowest first: plain IPv4, other IPv6, link-local IPv6.
fn address_class(addr: SocketAddr) -> u8 {
    match addr {
        SocketAddr::V4(_) => 0,
        SocketAddr::V6(v6) if !v6.ip().is_unicast_link_local() => 1,
        SocketAddr::V6(_) => 2,
    }
}

/// The candidates of a dial in the order they are tried (protocol.md 3.2): canonical, each
/// once, IPv4 ahead of IPv6 ahead of link-local, otherwise in the order given. With
/// `keep_first` the first candidate stays first, so a normal dial still leads with the address
/// most likely to answer. Plan once, on a list newest first: a planned list has lost which
/// address is newest.
///
/// *Choice:* only [`MAX_ALTERNATIVES`] answers are held, and one radio answers on up to three
/// addresses. A radio rarely has two IPv4 addresses, so starting those first, 250 ms apart
/// against handshakes of tens of milliseconds, tends to hand the slots to different radios.
///
/// *Choice:* with more IPv4 addresses than that ahead of it, the newest other address starts
/// third, at 500 ms ([`EARLY_SLOTS`]): stale IPv4 entries must not push a live link-local
/// address back by seconds on a cold dial, or past the grace on a re-check. Two live IPv4
/// addresses have filled the slots by then, so this costs them nothing.
pub fn plan_candidates(candidates: &[SocketAddr], keep_first: bool) -> Vec<SocketAddr> {
    let mut unique = canonical_unique(candidates);
    let skip = usize::from(keep_first).min(unique.len());
    let given = unique.clone();
    unique[skip..].sort_by_key(|a| address_class(*a));
    let early = EARLY_SLOTS.min(unique.len());
    if unique.iter().take(EARLY_SLOTS + 1).all(SocketAddr::is_ipv4) {
        let newest = given.iter().find(|a| !a.is_ipv4());
        let at = newest.and_then(|n| unique.iter().position(|a| a == n));
        if let Some(at) = at.filter(|at| *at > early) {
            unique[early..=at].rotate_right(1);
        }
    }
    unique
}

/// How many attempts may start before the first one that is not IPv4; see
/// [`plan_candidates`].
const EARLY_SLOTS: usize = 2;

/// Starts one TLS attempt per candidate of a [`plan_candidates`] list, `stagger` apart.
fn start_attempts(planned: &[SocketAddr], opts: &DialOptions) -> Result<Attempts, DialError> {
    if planned.is_empty() {
        return Err(DialError::NoCandidates);
    }
    let mut set = JoinSet::new();
    for (i, addr) in planned.iter().copied().enumerate() {
        let opts = opts.clone();
        let delay = opts.stagger * u32::try_from(i).unwrap_or(u32::MAX);
        set.spawn(async move {
            tokio::time::sleep(delay).await;
            attempt(addr, &opts).await.map_err(|e| (addr, e))
        });
    }
    Ok(set)
}

/// Collects attempts that finish TLS until `deadline`, at most [`MAX_ALTERNATIVES`],
/// then stops the rest.
///
/// An attempt to an address in `never_hold` is closed as it finishes and takes no slot.
async fn collect_held(
    mut set: Attempts,
    deadline: tokio::time::Instant,
    never_hold: Vec<SocketAddr>,
) -> HeldDial {
    let mut out = HeldDial::default();
    while out.held.len() < MAX_ALTERNATIVES {
        tokio::select! {
            biased;
            joined = set.join_next() => match joined {
                Some(Ok(Ok(h))) => {
                    if !never_hold.contains(&h.remote_addr()) {
                        out.held.push(h);
                    }
                }
                Some(Ok(Err((addr, _)))) => out.failed.push(addr),
                Some(Err(_)) => {}
                None => break,
            },
            () = tokio::time::sleep_until(deadline) => break,
        }
    }
    set.abort_all();
    out
}

/// TCP connect and the TLS handshake with the expected SPKI (protocol.md 3.1); HTTP/2
/// comes later, in [`HeldTls::finish`].
async fn attempt(addr: SocketAddr, opts: &DialOptions) -> Result<HeldTls, String> {
    let tcp = tokio::time::timeout(opts.connect_timeout, TcpStream::connect(addr))
        .await
        .map_err(|_| "TCP connect timed out".to_owned())?
        .map_err(|e| format!("TCP connect: {e}"))?;
    tune_socket(&tcp);
    let local = tcp.local_addr().ok();
    let (stream, peer) = tls::connect_tcp(Arc::clone(&opts.config), opts.expected_spki, tcp)
        .await
        .map_err(|e| e.to_string())?;
    let exporter = tls::exporter(stream.get_ref().1).map_err(|e| e.to_string())?;
    Ok(HeldTls {
        remote: addr,
        local,
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
        local,
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
        local,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn addrs(list: &[&str]) -> Vec<SocketAddr> {
        list.iter().map(|a| a.parse().unwrap()).collect()
    }

    #[test]
    fn a_mapped_address_is_its_plain_form_and_a_scope_survives() {
        let of = |a: &str| canonical_addr(a.parse().unwrap()).to_string();
        assert_eq!(of("[::ffff:192.168.86.74]:42187"), "192.168.86.74:42187");
        assert_eq!(of("192.168.86.74:42187"), "192.168.86.74:42187");
        assert_eq!(of("[fe80::1%3]:42187"), "[fe80::1%3]:42187");
        assert_eq!(of("[2001:db8::1]:42187"), "[2001:db8::1]:42187");
    }

    /// A live address behind stale IPv4 entries, as a last-known list can have it.
    #[test]
    fn stale_ipv4_cannot_push_the_newest_other_address_back() {
        let given = addrs(&[
            "10.0.0.1:1",
            "[fe80::1%3]:1",
            "10.0.0.2:1",
            "10.0.0.3:1",
            "10.0.0.4:1",
            "10.0.0.5:1",
            "10.0.0.6:1",
            "10.0.0.7:1",
        ]);
        // A cold dial: third, at 500 ms, not eighth at 1.75 s.
        let cold = plan_candidates(&given, true);
        assert_eq!(cold[0], given[0]);
        assert_eq!(cold[2], given[1]);
        assert_eq!(cold[1], given[2]);
        assert_eq!(cold[3..], given[3..], "the rest keep their order");
        // A re-check: well inside the 1.5 s grace.
        let check = plan_candidates(&given, false);
        assert_eq!(check[2], given[1]);
        // The newest of them, whatever its class.
        let given = addrs(&[
            "[fe80::1%3]:1",
            "[2001:db8::1]:1",
            "10.0.0.1:1",
            "10.0.0.2:1",
            "10.0.0.3:1",
        ]);
        assert_eq!(plan_candidates(&given, false)[2], given[0]);
        // Third by the classes already, or none at all: nothing moves.
        let given = addrs(&[
            "10.0.0.1:1",
            "10.0.0.2:1",
            "[2001:db8::1]:1",
            "[fe80::1%3]:1",
        ]);
        assert_eq!(plan_candidates(&given, false), given);
        let v4 = addrs(&["10.0.0.1:1", "10.0.0.2:1", "10.0.0.3:1"]);
        assert_eq!(plan_candidates(&v4, false), v4);
    }

    /// The reading that started this: one radio on three addresses, in stored order.
    #[test]
    fn one_radio_on_three_addresses_is_two_candidates() {
        let plan = plan_candidates(
            &addrs(&[
                "[fe80::1%3]:42187",
                "[::ffff:192.168.86.74]:42187",
                "192.168.86.74:42187",
            ]),
            false,
        );
        assert_eq!(plan, addrs(&["192.168.86.74:42187", "[fe80::1%3]:42187"]));
    }

    #[test]
    fn ipv4_goes_ahead_of_ipv6_ahead_of_link_local_and_order_holds_inside_each() {
        let given = addrs(&[
            "[fe80::2%3]:1",
            "[fe80::1%4]:1",
            "[2001:db8::9]:1",
            "10.0.0.2:1",
            "[2001:db8::1]:1",
            "10.0.0.1:1",
        ]);
        let want = addrs(&[
            "10.0.0.2:1",
            "10.0.0.1:1",
            "[2001:db8::9]:1",
            "[2001:db8::1]:1",
            "[fe80::2%3]:1",
            "[fe80::1%4]:1",
        ]);
        assert_eq!(plan_candidates(&given, false), want);
    }

    #[test]
    fn a_normal_dial_keeps_its_first_candidate_first() {
        let given = addrs(&["[fe80::2%3]:1", "[fe80::1%4]:1", "10.0.0.1:1"]);
        assert_eq!(
            plan_candidates(&given, true),
            addrs(&["[fe80::2%3]:1", "10.0.0.1:1", "[fe80::1%4]:1"])
        );
        // The mapped form of the first counts as the first.
        let given = addrs(&["[::ffff:10.0.0.1]:1", "10.0.0.1:1", "10.0.0.2:1"]);
        assert_eq!(
            plan_candidates(&given, true),
            addrs(&["10.0.0.1:1", "10.0.0.2:1"])
        );
        assert_eq!(plan_candidates(&[], true), Vec::<SocketAddr>::new());
    }
}

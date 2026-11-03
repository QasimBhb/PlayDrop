//! Path choice (protocol.md 3.5): when a peer answers on more than one address (a phone on
//! Wi-Fi with USB tethering on, a desktop on Ethernet and Wi-Fi), the dialler measures
//! each path with a probe and moves to a clearly faster one, instead of keeping whichever
//! finished TLS first.
//!
//! - After a dial, the other candidates that finished TLS within the grace are held
//!   before HTTP/2 ([`after_dial`]).
//! - A verified advert from an address that is not the current one, or a network change,
//!   re-checks a peer that is online over a connection we dialled ([`on_advert`],
//!   [`on_network_changed`]): at most one check per peer per 60 s, pinned and verified
//!   addresses only (SR-1), not counted against the 5 s reconnect budget of the dial
//!   limiter. An advert from an address already tried waits 10 minutes, doubling after
//!   each check that changes nothing.
//! - No check runs on low battery or while either side is asleep ([`should_save_power`]).
//! - A check never runs or switches while a send to the peer, an outbox pull from any peer
//!   (the pull list has no peer) or an input session from the peer is active; it is tried
//!   again 60 s later.
//! - Probes run one at a time, the connection in use first: a `404` means a peer from
//!   before 3.5 and the alternatives are dropped before HTTP/2, so that peer never sees
//!   them.
//! - One radio's addresses are one path: an alternative whose first probe head shows a
//!   path already measured is closed there ([`PathKey`]), and not held again until the
//!   network changes, the path moves, or 160 minutes pass, so it takes no slot meanwhile.
//! - A switch opens the session on the new connection first (it replaces the old one,
//!   3.3: same dialler, newer), then makes it the outbound connection, which drains the
//!   old one, and moves its address to the front of the pin's last-known list. A session
//!   request nobody answers leaves the old path in place if it still answers a PING.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

use super::connect::{close_if_silent, dial_lock, watch_connection};
use super::session::Session;
use super::{lock, session, unix_now, Inner};
use crate::identity::DeviceId;
use crate::net::client::{HeldDial, ALTERNATIVE_GRACE, MAX_ALTERNATIVES};
use crate::net::connection::{short, Heard, REPLACED_PING_DEADLINE};
use crate::net::path_id::{PathId, PathKey};
use crate::net::probe::{probe_in_use_noting, probe_noting, InUseProbe, ProbeOutcome, PROBE_BYTES};
use crate::net::{
    canonical_addr, canonical_unique, dial_held, Alternatives, CloseReason, Connection,
    DialOptions, HeldTls,
};
use crate::session::types::PresenceState;
use crate::tls;

/// Default of [`PathTiming::recheck_every`].
pub const RECHECK_EVERY: Duration = Duration::from_secs(60);

/// The back-off of protocol.md 3.5: how long an advert from an address already tried waits
/// doubles after each check that changes nothing, at most this many times. *Choice:* 4 takes
/// 10 minutes to 160, so a phone advertising on two paths costs at most one 9 MiB check every
/// 160 minutes once nothing changes, not 54 MiB an hour.
const MAX_QUIET_DOUBLINGS: u32 = 4;

/// The intervals of path checks. Rust only; tests shorten them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PathTiming {
    /// Shortest time between two path checks of one peer, and the wait before a check that
    /// found the peer busy runs again (protocol.md 3.5).
    pub recheck_every: Duration,
    /// How long an advert from an address the last check tried is ignored, before the
    /// back-off. See [`SAME_ADDRESS_RECHECK`].
    pub same_address_recheck: Duration,
}

impl Default for PathTiming {
    fn default() -> Self {
        Self {
            recheck_every: RECHECK_EVERY,
            same_address_recheck: SAME_ADDRESS_RECHECK,
        }
    }
}

/// Most addresses set aside per peer. *Choice:* a peer has at most 8 last-known addresses and
/// a few discovered ones; this only stops a peer that keeps changing address from growing the
/// list. The oldest goes first, and costs at most one more check.
const MAX_MERGED: usize = 16;

/// How much faster an alternative must measure before the dialler moves to it.
/// *Choice:* 1.25 keeps two paths of about the same speed from trading places on noise,
/// while the measured case (USB 2.0 tether at 27 MiB/s against Wi-Fi at 45 to 55,
/// `docs/release/throughput.md` 12.3) clears it easily.
pub const SWITCH_RATIO: f64 = 1.25;

/// An advert from an address the last check already probed re-checks only after this
/// long. *Choice:* a phone that adverts on both paths would otherwise cost two probes a
/// minute for a decision that does not change.
const SAME_ADDRESS_RECHECK: Duration = Duration::from_secs(600);

/// How long after a dial's session settles its battery and presence get to arrive, before a
/// check decides whether to probe. *Choice:* the peer sends them in the same breath as its
/// first frames, so one round trip is enough; this leaves room for a slow path.
const POWER_STATE_WAIT: Duration = Duration::from_millis(500);

/// How long the session may take to come up on the new connection during a switch.
const SESSION_MOVE_TIMEOUT: Duration = Duration::from_secs(5);

/// How a path check ended (protocol.md 3.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PathOutcome {
    /// The connection in use measured fastest, or no alternative was clearly faster.
    Kept,
    /// Moved to a faster path.
    Switched,
    /// A send, outbox pull or input session was active; tried again later.
    Busy,
    /// The peer has no probe endpoint (a host from before protocol.md 3.5).
    Unsupported,
    /// No other address finished TLS in time.
    NoAlternative,
    /// Not checked, to save battery: low battery on either side, or either side asleep.
    SavingPower,
    /// The probe of the connection in use, or the switch, failed.
    Failed,
}

impl PathOutcome {
    fn as_str(self) -> &'static str {
        match self {
            Self::Kept => "kept",
            Self::Switched => "switched",
            Self::Busy => "busy, try again later",
            Self::Unsupported => "peer cannot be probed",
            Self::NoAlternative => "no other address answered",
            Self::SavingPower => "not checked, saving battery",
            Self::Failed => "failed",
        }
    }
}

/// One probe of a path check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProbeReport {
    /// The path's remote address.
    pub addr: SocketAddr,
    /// True for the connection that was in use when the check ran.
    pub current: bool,
    /// The measured rate, bytes per second; `None` if the probe failed.
    pub bytes_per_sec: Option<u64>,
    /// Why the probe failed, if it did.
    pub error: Option<String>,
    /// This address turned out to be on the same path as that one, which was measured, so
    /// this one was not (protocol.md 3.5).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub same_path_as: Option<SocketAddr>,
}

/// What [`Host::path_reports`](super::Host::path_reports) lists per peer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PathReport {
    /// The peer's Device ID, truncated to its first 8 hex characters (SR-32).
    pub peer: String,
    /// Short ID (SR-8).
    pub short_id: String,
    /// Remote address of our outbound connection to the peer right now, or the last one.
    pub chosen: Option<SocketAddr>,
    /// How the last check ended; `None` if no check ran yet.
    pub outcome: Option<PathOutcome>,
    /// When the last check ended, Unix seconds.
    pub checked_at: Option<u64>,
    /// The probes of the last check, the connection in use first.
    pub probes: Vec<ProbeReport>,
}

impl PathReport {
    /// One line for a diagnostics screen, for example "note: path to Phone:
    /// 192.168.1.20:42187 (switched 12 s ago; 192.168.42.129:42187 26.9 MiB/s,
    /// 192.168.1.20:42187 48.2 MiB/s)". `now` is Unix seconds. The `note: ` prefix marks
    /// it as information, not a failed layer, for the desktop's Diagnose page.
    pub fn summary_line(&self, label: &str, now: u64) -> String {
        let chosen = self
            .chosen
            .map_or_else(|| "not connected".to_owned(), |a| a.to_string());
        let mut line = format!("note: path to {label}: {chosen}");
        if let (Some(outcome), Some(at)) = (self.outcome, self.checked_at) {
            let probes: Vec<String> = self
                .probes
                .iter()
                .map(|p| match (p.same_path_as, p.bytes_per_sec, &p.error) {
                    (Some(same), _, _) => {
                        format!("{} same path as {same}, not measured", p.addr)
                    }
                    (None, Some(r), _) => format!("{} {}", p.addr, mib_per_s(r)),
                    (None, None, Some(e)) => format!("{} {e}", p.addr),
                    (None, None, None) => p.addr.to_string(),
                })
                .collect();
            let ago = now.saturating_sub(at);
            let _ = write!(line, " ({} {ago} s ago", outcome.as_str());
            if !probes.is_empty() {
                let _ = write!(line, "; {}", probes.join(", "));
            }
            line.push(')');
        }
        line
    }
}

#[allow(clippy::cast_precision_loss, reason = "a display value")]
fn mib_per_s(bytes_per_sec: u64) -> String {
    format!("{:.1} MiB/s", bytes_per_sec as f64 / (1024.0 * 1024.0))
}

/// Per-peer path state.
#[derive(Debug, Default)]
pub(crate) struct PathState {
    /// When the last check started.
    last_check: Option<tokio::time::Instant>,
    /// A check is running.
    running: bool,
    /// Checks this host still owes.
    owed: Owed,
    /// Addresses the last check dialled or probed, answered or not.
    tried: Vec<SocketAddr>,
    /// A switch is under way.
    switching: bool,
    /// Checks in a row that changed nothing, for the back-off.
    quiet_checks: u32,
    /// Addresses that finished TLS in the last check that dialled any.
    answered: Vec<SocketAddr>,
    /// Addresses found to be on a path a check measured, oldest first, with when each was
    /// found: not held for a check until the network changes, the path in use moves or the
    /// entry lapses ([`merged_lapse`]).
    merged: Vec<(SocketAddr, tokio::time::Instant)>,
    /// Raised each time `merged` is cleared, so a check that began before does not write back
    /// what it found on the paths as they were.
    merged_epoch: u64,
    /// `merged_epoch` when the running check began.
    check_epoch: u64,
    /// Switches in a row whose session request got no answer, or no session.
    failed_switches: u32,
    /// After a failed switch, no check before this.
    blocked_until: Option<tokio::time::Instant>,
    /// What the peer said about its power.
    peer_power: PeerPower,
    chosen: Option<SocketAddr>,
    outcome: Option<PathOutcome>,
    checked_at: Option<u64>,
    probes: Vec<ProbeReport>,
}

/// Checks a peer is owed.
#[derive(Debug, Default, Clone, Copy)]
struct Owed {
    /// A re-check is scheduled.
    retry: bool,
    /// A check was skipped to save battery, and has not run since.
    power: bool,
}

/// What the peer said about its power, for [`should_save_power`].
#[derive(Debug, Default, Clone, Copy)]
struct PeerPower {
    /// On low battery and not charging (5.8).
    low_battery: bool,
    /// Asleep (5.10).
    asleep: bool,
}

/// Path state of every peer.
pub(crate) type Paths = HashMap<DeviceId, PathState>;

/// What started a re-check.
#[derive(Debug, Clone, Copy)]
enum Trigger {
    /// A verified advert from this address.
    Advert(SocketAddr),
    /// The OS reported a network change.
    NetworkChanged,
    /// The scheduled retry after a busy check.
    Retry,
    /// A check skipped to save power, now that it has ended.
    Owed,
}

/// The switch decision: the index of the fastest alternative if it measured at least
/// [`SWITCH_RATIO`] times the connection in use, else `None` (keep it). Failed probes are
/// `None` rates and never win.
pub(crate) fn decide(current: f64, alternatives: &[Option<f64>]) -> Option<usize> {
    let (best, rate) = alternatives
        .iter()
        .enumerate()
        .filter_map(|(i, r)| r.map(|r| (i, r)))
        .max_by(|a, b| a.1.total_cmp(&b.1))?;
    (rate >= current * SWITCH_RATIO && rate > 0.0).then_some(best)
}

/// Closes every connection it still holds when dropped, so alternatives never outlive a
/// check, even one cut short by shutdown.
struct Closer(Vec<Arc<Connection>>);

impl Closer {
    fn take(&mut self, i: usize) -> Arc<Connection> {
        self.0.remove(i)
    }
}

impl Drop for Closer {
    fn drop(&mut self) {
        for c in self.0.drain(..) {
            c.close(CloseReason::Duplicate);
        }
    }
}

/// True while a check or a switch would get in the way: a send to the peer (running or
/// waiting to reconnect), a transfer the peer is delivering here, an outbox pull, or an
/// input session from the peer.
fn busy(inner: &Inner, peer: DeviceId) -> bool {
    lock(&inner.sends).live.values().any(|o| o.peer == peer)
        || super::receive::receiving_from(inner, peer)
        || !lock(&inner.pulling).is_empty()
        || lock(&inner.input).open_sessions().contains(&peer)
}

/// Records a dial. On another path than before, what the back-off knows is old: it says how
/// the last check saw paths that have changed. True if the path moved.
fn note_dialled(inner: &Inner, peer: DeviceId, addr: SocketAddr) -> bool {
    let mut paths = lock(&inner.paths);
    let st = paths.entry(peer).or_default();
    let moved = st.chosen.is_some_and(|c| c != addr);
    st.chosen = Some(addr);
    if moved {
        st.quiet_checks = 0;
        st.tried.clear();
        st.answered.clear();
        clear_merged(st);
    }
    moved
}

/// Remembers which addresses answered, for the wait on their adverts. An alternative later
/// found to be on a path already measured stays in here: it did answer, so an advert from it
/// waits the long wait, and a radio advertising on several addresses costs no more for that.
fn note_answered(inner: &Inner, peer: DeviceId, held: &[HeldTls]) {
    note_answered_addrs(inner, peer, held.iter().map(HeldTls::remote_addr));
}

/// [`note_answered`] for bare addresses.
fn note_answered_addrs(inner: &Inner, peer: DeviceId, addrs: impl IntoIterator<Item = SocketAddr>) {
    let mut paths = lock(&inner.paths);
    let Some(st) = pinned_state(inner, &mut paths, peer) else {
        return;
    };
    for a in addrs {
        if !st.answered.contains(&a) {
            st.answered.push(a);
        }
    }
}

/// A switch that could not place the session: a path where this keeps happening would cost
/// an offline each time the peer got the request late (protocol.md 3.5), so checks stop for
/// the back-off's wait, doubling each time.
fn note_failed_switch(inner: &Inner, peer: DeviceId) {
    let base = inner.path_timing.same_address_recheck;
    let mut paths = lock(&inner.paths);
    let Some(st) = pinned_state(inner, &mut paths, peer) else {
        return;
    };
    st.failed_switches = st.failed_switches.saturating_add(1);
    st.blocked_until = Some(tokio::time::Instant::now() + advert_wait(base, st.failed_switches));
}

/// Records the remote address of a fresh outbound connection.
fn record_chosen(inner: &Inner, peer: DeviceId, addr: SocketAddr) {
    lock(&inner.paths).entry(peer).or_default().chosen = Some(addr);
}

/// True while no check may start: one runs, one started less than
/// [`PathTiming::recheck_every`] ago, or a failed switch still blocks them.
fn checked_recently(st: &PathState, every: Duration, now: tokio::time::Instant) -> bool {
    st.running
        || st.blocked_until.is_some_and(|t| now < t)
        || st
            .last_check
            .is_some_and(|t| now.saturating_duration_since(t) < every)
}

/// How long an advert from an address already tried is ignored after `quiet_checks` checks
/// in a row that changed nothing (protocol.md 3.5).
pub(crate) fn advert_wait(base: Duration, quiet_checks: u32) -> Duration {
    base * 2u32.pow(quiet_checks.saturating_sub(1).min(MAX_QUIET_DOUBLINGS))
}

/// True if probing should wait, from what this host and the peer have said: up to 9 MiB per
/// check costs a phone battery on both ends. Path choice needs nothing else, so all that is
/// lost is a possibly faster path until the next trigger.
fn should_save_power(inner: &Inner, peer: DeviceId) -> bool {
    let (own_low, own_asleep) = {
        let st = lock(&inner.status);
        (
            st.battery.as_ref().is_some_and(|b| b.low && !b.charging),
            st.presence
                .as_ref()
                .is_some_and(|p| p.state == PresenceState::Asleep),
        )
    };
    let peer = lock(&inner.paths)
        .get(&peer)
        .map(|s| s.peer_power)
        .unwrap_or_default();
    own_low || own_asleep || peer.low_battery || peer.asleep
}

/// The peer's state, unless it was unpaired: nothing may bring back what unpair cleared. The
/// pin goes before the state does, so testing it under this lock leaves no gap.
fn pinned_state<'a>(
    inner: &Inner,
    paths: &'a mut Paths,
    peer: DeviceId,
) -> Option<&'a mut PathState> {
    inner
        .pins
        .contains(&peer)
        .then(|| paths.entry(peer).or_default())
}

/// The peer reported its battery (5.8).
pub(crate) fn note_peer_battery(inner: &Arc<Inner>, peer: DeviceId, low: bool) {
    if let Some(st) = pinned_state(inner, &mut lock(&inner.paths), peer) {
        st.peer_power.low_battery = low;
    }
    run_skipped_check(inner, peer);
}

/// The peer reported its presence (5.10).
pub(crate) fn note_peer_presence(inner: &Arc<Inner>, peer: DeviceId, asleep: bool) {
    if let Some(st) = pinned_state(inner, &mut lock(&inner.paths), peer) {
        st.peer_power.asleep = asleep;
    }
    run_skipped_check(inner, peer);
}

/// This host's battery or presence changed: a check skipped to save power may run now.
pub(crate) fn on_own_power_changed(inner: &Arc<Inner>) {
    let peers: Vec<DeviceId> = lock(&inner.paths).keys().copied().collect();
    for peer in peers {
        run_skipped_check(inner, peer);
    }
}

/// Power saving ended and a check had been skipped for it: nothing else would bring that
/// check back, so it runs now. The back-off keeps its count: a phone that sleeps and wakes
/// all day must not earn a check at the shortest wait each time.
fn run_skipped_check(inner: &Arc<Inner>, peer: DeviceId) {
    let skipped = lock(&inner.paths).get(&peer).is_some_and(|s| s.owed.power);
    if skipped && !should_save_power(inner, peer) {
        if let Some(st) = lock(&inner.paths).get_mut(&peer) {
            st.owed.power = false;
        }
        recheck(inner, peer, Trigger::Owed);
    }
}

/// A session with the peer was placed: its next `battery` and `presence` frames are current.
pub(crate) fn on_session_placed(inner: &Inner, peer: DeviceId) {
    lock(&inner.paths).entry(peer).or_default().peer_power = PeerPower::default();
}

/// Records that a check did not run, to save battery. It does not count as a check: the
/// next trigger tries again.
fn note_saving_power(inner: &Inner, peer: DeviceId) {
    let mut paths = lock(&inner.paths);
    let Some(st) = pinned_state(inner, &mut paths, peer) else {
        return;
    };
    st.outcome = Some(PathOutcome::SavingPower);
    st.checked_at = Some(unix_now());
    st.probes = Vec::new();
    st.owed.power = true;
}

/// The grace a dial to `peer` gives its other candidates (protocol.md 3.2, 3.5): none
/// when no check could run on them anyway, so losers are not kept for nothing.
pub(crate) fn dial_grace(inner: &Inner, peer: DeviceId) -> Duration {
    if !inner.path_choice {
        return Duration::ZERO;
    }
    let now = tokio::time::Instant::now();
    if should_save_power(inner, peer) {
        return Duration::ZERO;
    }
    match lock(&inner.paths).get(&peer) {
        Some(st) if checked_recently(st, inner.path_timing.recheck_every, now) => Duration::ZERO,
        _ => ALTERNATIVE_GRACE,
    }
}

/// Claims the peer's check slot: false if one runs or one started less than
/// [`PathTiming::recheck_every`] ago. `tried` are the addresses this check dials besides the
/// one in use; an advert from one of them waits (`limits_allow`).
fn begin(inner: &Inner, peer: DeviceId, tried: &[SocketAddr]) -> bool {
    let now = tokio::time::Instant::now();
    let mut paths = lock(&inner.paths);
    let st = paths.entry(peer).or_default();
    if checked_recently(st, inner.path_timing.recheck_every, now) {
        return false;
    }
    st.running = true;
    st.last_check = Some(now);
    st.tried = tried.to_vec();
    st.check_epoch = st.merged_epoch;
    true
}

/// Releases the check slot and records how it ended. The probe list is replaced, so a
/// check that probed nothing shows no rates from an older one.
fn end(inner: &Inner, peer: DeviceId, outcome: PathOutcome, probes: Vec<ProbeReport>) {
    let mut paths = lock(&inner.paths);
    let Some(st) = pinned_state(inner, &mut paths, peer) else {
        return;
    };
    st.running = false;
    if outcome == PathOutcome::Switched {
        st.failed_switches = 0;
        st.blocked_until = None;
    }
    st.quiet_checks = match outcome {
        PathOutcome::Switched => 0,
        // Says nothing about the paths.
        PathOutcome::Busy | PathOutcome::SavingPower => st.quiet_checks,
        _ => st.quiet_checks.saturating_add(1),
    };
    st.outcome = Some(outcome);
    st.checked_at = Some(unix_now());
    if outcome == PathOutcome::Switched {
        clear_merged(st);
    }
    let still_true = outcome != PathOutcome::Switched && st.check_epoch == st.merged_epoch;
    let now = tokio::time::Instant::now();
    for p in &probes {
        if !st.tried.contains(&p.addr) {
            st.tried.push(p.addr);
        }
        if still_true && p.same_path_as.is_some() {
            note_merged(st, p.addr, now);
        }
    }
    st.probes = probes;
}

/// True while a path switch to `peer` is under way; [`connect::outbound`] then waits for
/// it instead of handing out the connection being replaced.
///
/// [`connect::outbound`]: super::connect::outbound
pub(crate) fn switching(inner: &Inner, peer: DeviceId) -> bool {
    lock(&inner.paths).get(&peer).is_some_and(|s| s.switching)
}

/// Clears the switching mark when the switch ends, however it ends.
struct SwitchMark<'a>(&'a Inner, DeviceId);

impl Drop for SwitchMark<'_> {
    fn drop(&mut self) {
        if let Some(st) = lock(&self.0.paths).get_mut(&self.1) {
            st.switching = false;
        }
    }
}

/// After a dial (protocol.md 3.5): waits for the dial's own session attempt to settle,
/// so the check knows where the session lives, then for the alternatives, and if any
/// finished TLS, checks the paths.
pub(crate) fn after_dial(
    inner: &Arc<Inner>,
    conn: &Arc<Connection>,
    alts: Alternatives,
    session_attempt: Option<oneshot::Receiver<()>>,
) {
    let peer = conn.device_id();
    let moved = note_dialled(inner, peer, conn.remote_addr());
    if !inner.path_choice {
        return;
    }
    let i2 = Arc::clone(inner);
    let c = Arc::clone(conn);
    inner.spawn(async move {
        if let Some(rx) = session_attempt {
            let _ = tokio::time::timeout(SESSION_MOVE_TIMEOUT * 3, rx).await;
        }
        // Unpaired meanwhile: dropping the alternatives closes them (SR-14).
        if !i2.pins.contains(&peer) {
            return;
        }
        // Unpair closes `c` at once; dropping `collect` closes the held sockets (SR-14).
        let held = tokio::select! {
            held = alts.collect() => held,
            () = c.closed() => return,
        };
        let tried: Vec<SocketAddr> = held.iter().map(HeldTls::remote_addr).collect();
        if c.is_closed() || !i2.pins.contains(&peer) {
            return;
        }
        // The peer repeats its battery and presence right after its `hello`: give them time
        // to arrive, or a sleeping phone gets probed.
        tokio::time::sleep(POWER_STATE_WAIT).await;
        // Checked first: a dial while saving power gives no grace, so `held` is empty, and the
        // check it did without is owed all the same (3.5).
        if should_save_power(&i2, peer) {
            note_saving_power(&i2, peer);
            return;
        }
        // No check can run now on a new path (no other address answered, or one ran less
        // than a minute ago): one follows, or nothing would look for the better path again.
        if held.is_empty() {
            if moved {
                defer(&i2, peer);
            }
            return;
        }
        if !begin(&i2, peer, &tried) {
            if moved {
                defer(&i2, peer);
            }
            return;
        }
        check(&i2, peer, &c, held).await;
    });
}

/// Waits until `sess` is the peer's installed session (its `hello` arrived and it was
/// placed, protocol.md 3.3), it ended, or `timeout` passed. True if installed.
pub(crate) async fn wait_installed(inner: &Inner, sess: &Session, timeout: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    let _ = sess.wait_hello(timeout).await;
    loop {
        let peer = sess.peer();
        if inner
            .registry
            .session(&peer)
            .is_some_and(|s| s.id == sess.id)
        {
            return true;
        }
        if !sess.is_live() || tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// A verified advert for a peer that is online (protocol.md 3.5, 8.3). `addr` is canonical
/// ([`connect::on_verified_advert`]), like every address the path state holds.
///
/// [`connect::on_verified_advert`]: super::connect::on_verified_advert
pub(crate) fn on_advert(inner: &Arc<Inner>, peer: DeviceId, addr: SocketAddr) {
    recheck(inner, peer, Trigger::Advert(addr));
}

/// The addresses of `peer` not to hold for a check.
pub(crate) fn merged_addresses(inner: &Inner, peer: DeviceId) -> Vec<SocketAddr> {
    let lapse_after = merged_lapse(inner.path_timing);
    let now = tokio::time::Instant::now();
    let mut paths = lock(&inner.paths);
    let Some(st) = paths.get_mut(&peer) else {
        return Vec::new();
    };
    lapse_merged(st, lapse_after, now);
    st.merged.iter().map(|(a, _)| *a).collect()
}

/// How long an address stays set aside. *Choice:* the back-off's longest wait (160 minutes
/// by default): not every platform reports a network change (Windows and `playdrop host` have
/// no watcher, Linux sees only overall state), so a faster path that appears on a known
/// address must still get found. An address that lapsed costs one check of about 4 MiB, and
/// only when something triggers one.
fn merged_lapse(timing: PathTiming) -> Duration {
    advert_wait(timing.same_address_recheck, u32::MAX)
}

/// Drops the entries older than `lapse_after`. Each stays known as tried, so its next advert
/// waits like any other tried address's, rather than resetting the back-off as a new one does.
fn lapse_merged(st: &mut PathState, lapse_after: Duration, now: tokio::time::Instant) {
    let tried = &mut st.tried;
    st.merged.retain(|(addr, at)| {
        let keep = now.saturating_duration_since(*at) < lapse_after;
        if !keep && !tried.contains(addr) {
            tried.push(*addr);
        }
        keep
    });
}

/// Sets `addr` aside from `now`, within [`MAX_MERGED`].
fn note_merged(st: &mut PathState, addr: SocketAddr, now: tokio::time::Instant) {
    st.merged.retain(|(a, _)| *a != addr);
    if st.merged.len() >= MAX_MERGED {
        st.merged.remove(0);
    }
    st.merged.push((addr, now));
}

/// Forgets what was set aside; a check under way will not bring it back.
fn clear_merged(st: &mut PathState) {
    st.merged.clear();
    st.merged_epoch = st.merged_epoch.wrapping_add(1);
}

/// What a re-check may dial out of `known`, newest first: canonical, each once, without the
/// address in use or those set aside. Only [`dial_held`] plans the order, as planning loses
/// which address is newest.
fn recheck_candidates(
    known: &[SocketAddr],
    in_use: SocketAddr,
    merged: &[SocketAddr],
) -> Vec<SocketAddr> {
    let mut addrs = canonical_unique(known);
    addrs.retain(|a| *a != in_use && !merged.contains(a));
    addrs
}

/// The OS reported a network change: what was one path may not be any more, and every online
/// peer we dialled is re-checked.
pub(crate) fn on_network_changed(inner: &Arc<Inner>) {
    for st in lock(&inner.paths).values_mut() {
        clear_merged(st);
    }
    for s in inner.registry.sessions() {
        recheck(inner, s.peer(), Trigger::NetworkChanged);
    }
}

/// Applies a trigger to the back-off state: false if the limits say no check now
/// (protocol.md 3.5). A check owed to a change comes later instead of being lost.
fn limits_allow(inner: &Arc<Inner>, peer: DeviceId, why: Trigger) -> bool {
    let now = tokio::time::Instant::now();
    let timing = inner.path_timing;
    let mut paths = lock(&inner.paths);
    if let Some(st) = paths.get_mut(&peer) {
        lapse_merged(st, merged_lapse(timing), now);
        if matches!(why, Trigger::NetworkChanged) {
            st.blocked_until = None;
            st.failed_switches = 0;
        }
        if checked_recently(st, timing.recheck_every, now) {
            // Nothing else would bring back a check owed to a change.
            if !matches!(why, Trigger::Owed) {
                return false;
            }
            drop(paths);
            defer(inner, peer);
            return false;
        }
        match why {
            // An advert from an address the last check already tried waits. If that
            // address answered, the wait is the back-off; if it did not, the device says
            // it is there now, so it is tried again much sooner, and only costs a dial
            // (below). An address not tried yet means the paths changed.
            Trigger::Advert(addr)
                if st.tried.contains(&addr) || st.merged.iter().any(|(a, _)| *a == addr) =>
            {
                let base = if st.answered.contains(&addr) {
                    timing.same_address_recheck
                } else {
                    timing.recheck_every
                };
                let wait = advert_wait(base, st.quiet_checks);
                if st
                    .last_check
                    .is_some_and(|t| now.saturating_duration_since(t) < wait)
                {
                    return false;
                }
            }
            Trigger::Advert(_) | Trigger::NetworkChanged => st.quiet_checks = 0,
            Trigger::Retry | Trigger::Owed => {}
        }
    }
    true
}

/// Re-checks the paths of `peer` if it is online over a connection we dialled and the
/// limits allow (protocol.md 3.5).
fn recheck(inner: &Arc<Inner>, peer: DeviceId, why: Trigger) {
    if !inner.path_choice || inner.is_shutting_down() {
        return;
    }
    if inner.registry.session(&peer).is_none() {
        return;
    }
    let Some(current) = inner.registry.outbound(&peer) else {
        return;
    };
    let in_use = canonical_addr(current.remote_addr());
    if let Trigger::Advert(addr) = why {
        if addr == in_use {
            return;
        }
    }
    if !limits_allow(inner, peer, why) {
        return;
    }
    if should_save_power(inner, peer) {
        note_saving_power(inner, peer);
        return;
    }
    if busy(inner, peer) {
        defer(inner, peer);
        return;
    }
    let Some(pin) = inner.pins.get(&peer) else {
        return;
    };
    // Pinned last-known and verified advert addresses only (SR-1, protocol.md 3.2).
    let mut addrs = pin.last_known.clone();
    if let Some(r) = lock(&inner.reach).get(&peer) {
        for a in &r.discovered {
            if !addrs.contains(a) {
                addrs.push(*a);
            }
        }
    }
    if let Trigger::Advert(addr) = why {
        if !addrs.contains(&addr) {
            addrs.push(addr);
        }
    }
    // Canonical, so a stored mapped form neither dials the address in use again nor makes the
    // back-off take one address for two. Addresses on a path already measured would only take
    // the held slots from one that is not, check after check (protocol.md 3.5). With nothing
    // else to dial nothing is measured, and the last report stands.
    let merged = merged_addresses(inner, peer);
    let addrs = recheck_candidates(&addrs, in_use, &merged);
    if addrs.is_empty() && !merged.is_empty() {
        return;
    }
    // A re-check dials at most once per peer per 60 s (`begin`), within the rate SR-1
    // and protocol.md 8.3 rule 1 allow. It does not take the 5 s reconnect budget of the
    // dial limiter: after a network change the session may be about to die, and its
    // reconnect must not wait for this check's dial.
    if !begin(inner, peer, &addrs) {
        return;
    }
    if addrs.is_empty() {
        end(inner, peer, PathOutcome::NoAlternative, Vec::new());
        return;
    }
    tracing::debug!(peer = %short(&peer), ?why, candidates = addrs.len(), "path re-check");
    let opts = DialOptions::new(
        tls::build_client_config(&inner.identity, pin.spki_hash),
        Some(pin.spki_hash),
    );
    let i2 = Arc::clone(inner);
    let earlier = lock(&inner.paths)
        .get(&peer)
        .map(|s| s.answered.clone())
        .unwrap_or_default();
    inner.spawn(async move {
        // Unpair closes `current`; dropping `dial_held` closes the held sockets (SR-14).
        let HeldDial { held, failed } = tokio::select! {
            dialled = dial_held(&addrs, &opts, ALTERNATIVE_GRACE) => dialled,
            () = current.closed() => HeldDial::default(),
        };
        let advertised = match why {
            Trigger::Advert(a) if !held.iter().any(|h| h.remote_addr() == a) => Some(a),
            _ => None,
        };
        // Every slot went to an address ahead of the advertised one, so it was stopped before
        // it could answer. That says nothing against it, and the others are still worth
        // measuring. It counts as answered, or each of its adverts would bring a check at
        // the short wait. An attempt that failed did get its chance.
        let lost_its_slot =
            held.len() >= MAX_ALTERNATIVES && advertised.is_some_and(|a| !failed.contains(&a));
        if let (true, Some(a)) = (lost_its_slot, advertised) {
            note_answered_addrs(&i2, peer, [a]);
        }
        // Retried because an address that did not answer advertised again: it still does
        // not answer, so nothing new is known and nothing is probed.
        let retry_in_vain = !lost_its_slot && advertised.is_some_and(|a| !earlier.contains(&a));
        if held.is_empty() || retry_in_vain {
            end(&i2, peer, PathOutcome::NoAlternative, Vec::new());
            return;
        }
        check(&i2, peer, &current, held).await;
    });
}

/// Schedules one re-check [`PathTiming::recheck_every`] from now, after a check found the peer busy.
fn defer(inner: &Arc<Inner>, peer: DeviceId) {
    {
        let mut paths = lock(&inner.paths);
        let Some(st) = pinned_state(inner, &mut paths, peer) else {
            return;
        };
        if st.owed.retry {
            return;
        }
        st.owed.retry = true;
    }
    let i2 = Arc::clone(inner);
    inner.spawn(async move {
        tokio::time::sleep(i2.path_timing.recheck_every).await;
        if let Some(st) = lock(&i2.paths).get_mut(&peer) {
            st.owed.retry = false;
        }
        recheck(&i2, peer, Trigger::Retry);
    });
}

/// This host's id for `conn`, given up on if the connection in use closes: unpair and
/// shutdown close it, and a check must not outlast them for a listing.
async fn own_id(inner: &Inner, conn: &Connection, current: &Connection) -> Option<PathId> {
    tokio::select! {
        id = conn.own_path_id(&inner.path_interfaces) => id,
        () = current.closed() => None,
    }
}

/// Probes one alternative and records the result. It ends at its first head if that shows a
/// path in `measured`, and is then closed: it is not eligible for [`decide`], and has cost
/// one empty response rather than a whole probe. With an id missing on either side there is
/// no key, and it is measured as any other.
async fn probe_alternative(
    conn: &Connection,
    own: Option<PathId>,
    measured: &mut Vec<(PathKey, SocketAddr)>,
    probes: &mut Vec<ProbeReport>,
) -> Option<f64> {
    let addr = conn.remote_addr();
    let mut key = None;
    let mut same_as = None;
    let outcome = probe_noting(conn, PROBE_BYTES, &mut |peer| {
        key = PathKey::new(peer, own);
        same_as = measured
            .iter()
            .find(|(m, _)| Some(*m) == key)
            .map(|(_, a)| *a);
        same_as.is_none()
    })
    .await;
    match outcome {
        ProbeOutcome::Rate(r) => {
            if let Some(k) = key {
                measured.push((k, addr));
            }
            probes.push(report(addr, false, Some(r), None));
            Some(r)
        }
        ProbeOutcome::Stopped => {
            conn.close(CloseReason::Duplicate);
            probes.push(ProbeReport {
                same_path_as: same_as,
                ..report(addr, false, None, None)
            });
            None
        }
        ProbeOutcome::Unsupported => {
            probes.push(report(addr, false, None, Some("404".into())));
            None
        }
        ProbeOutcome::Failed(e) => {
            probes.push(report(addr, false, None, Some(e)));
            None
        }
    }
}

/// Runs a claimed check: probes the connection in use, then each alternative, one at a
/// time, and switches if one is clearly faster and nothing is active.
async fn check(inner: &Arc<Inner>, peer: DeviceId, current: &Arc<Connection>, held: Vec<HeldTls>) {
    note_answered(inner, peer, &held);
    if should_save_power(inner, peer) {
        drop(held);
        end(inner, peer, PathOutcome::SavingPower, Vec::new());
        if let Some(st) = lock(&inner.paths).get_mut(&peer) {
            st.owed.power = true;
        }
        return;
    }
    if busy(inner, peer) {
        drop(held);
        end(inner, peer, PathOutcome::Busy, Vec::new());
        defer(inner, peer);
        return;
    }
    let mut probes = Vec::new();
    if !inner.pins.contains(&peer) {
        return end(inner, peer, PathOutcome::Failed, probes);
    }
    let addr = current.remote_addr();
    // The paths measured so far, by key, with the address that measured each.
    let mut measured = Vec::with_capacity(MAX_ALTERNATIVES + 1);
    let mut current_peer_id = None;
    let on_head = &mut |peer| {
        current_peer_id = peer;
        true
    };
    let current_rate = match probe_in_use_noting(current, PROBE_BYTES, on_head).await {
        InUseProbe::Rate(r) => {
            probes.push(report(addr, true, Some(r), None));
            if let Some(key) = PathKey::new(current_peer_id, own_id(inner, current, current).await)
            {
                measured.push((key, addr));
            }
            r
        }
        InUseProbe::Dead(e) => {
            // Any alternative that measures wins, on this radio too: a connection can die
            // while its radio lives on (a roam, a new address), so a dead one merges nothing.
            probes.push(report(addr, true, None, Some(e)));
            0.0
        }
        InUseProbe::Unsupported => {
            // A peer from before 3.5: the alternatives never reach HTTP/2, so it never
            // sees them and the connection in use is left alone.
            drop(held);
            tracing::debug!(peer = %short(&peer), "peer has no probe endpoint; path kept");
            return end(inner, peer, PathOutcome::Unsupported, Vec::new());
        }
        InUseProbe::Unmeasured(e) => {
            // It answers PINGs but not probes: a passing stall, not a dead path.
            drop(held);
            probes.push(report(addr, true, None, Some(e)));
            return end(inner, peer, PathOutcome::Failed, probes);
        }
    };
    if !inner.pins.contains(&peer) {
        end(inner, peer, PathOutcome::Failed, probes);
        return;
    }
    // The peer serves probes, so it counts a connection only from its first other
    // request (3.5): finishing HTTP/2 on the alternatives does not replace the one in
    // use. Each is tracked, so unpair and shutdown close it (SR-14).
    let limits = crate::session::Limits::default();
    let mut alts = Closer(Vec::new());
    for h in held {
        let addr = h.remote_addr();
        match h.finish(&limits).await {
            Ok(c) => {
                watch_connection(inner, &c);
                alts.0.push(c);
            }
            Err(e) => probes.push(report(addr, false, None, Some(e))),
        }
    }
    let mut rates = Vec::with_capacity(alts.0.len());
    for c in &alts.0 {
        if !inner.pins.contains(&peer) {
            end(inner, peer, PathOutcome::Failed, probes);
            return;
        }
        let own = own_id(inner, c, current).await;
        rates.push(probe_alternative(c, own, &mut measured, &mut probes).await);
    }
    let Some(best) = decide(current_rate, &rates) else {
        tracing::debug!(peer = %short(&peer), "path kept");
        end(inner, peer, PathOutcome::Kept, probes);
        return;
    };
    let new = alts.take(best);
    drop(alts);
    let outcome = switch(inner, peer, current, &new).await;
    after_switch(inner, peer, current, &new, outcome, probes);
}

/// Records how a switch ended.
fn after_switch(
    inner: &Arc<Inner>,
    peer: DeviceId,
    current: &Connection,
    new: &Connection,
    outcome: Result<(), SwitchError>,
    probes: Vec<ProbeReport>,
) {
    match outcome {
        Ok(()) => {
            tracing::info!(
                peer = %short(&peer),
                from = %current.remote_addr(),
                to = %new.remote_addr(),
                "moved to a faster path"
            );
            end(inner, peer, PathOutcome::Switched, probes);
        }
        Err(SwitchError::Busy) => {
            new.close(CloseReason::Duplicate);
            end(inner, peer, PathOutcome::Busy, probes);
            defer(inner, peer);
        }
        Err(SwitchError::NotStarted(e)) => {
            tracing::debug!(peer = %short(&peer), error = %e, "path switch not started; kept");
            new.close(CloseReason::Duplicate);
            end(inner, peer, PathOutcome::Failed, probes);
        }
        Err(SwitchError::Unanswered(e)) => {
            tracing::debug!(peer = %short(&peer), error = %e, "session request not answered; old path kept");
            note_failed_switch(inner, peer);
            end(inner, peer, PathOutcome::Failed, probes);
        }
        Err(SwitchError::SessionNotMoved(e)) => {
            tracing::debug!(peer = %short(&peer), error = %e, "session did not move; new path kept for requests");
            note_failed_switch(inner, peer);
            end(inner, peer, PathOutcome::Failed, probes);
        }
    }
}

fn report(
    addr: SocketAddr,
    current: bool,
    rate: Option<f64>,
    error: Option<String>,
) -> ProbeReport {
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a positive rate far below u64::MAX"
    )]
    let bytes_per_sec = rate.map(|r| r.max(0.0).round() as u64);
    ProbeReport {
        addr,
        current,
        bytes_per_sec,
        error,
        same_path_as: None,
    }
}

/// Why a switch did not complete.
enum SwitchError {
    /// Something became active before the point of no return; nothing was sent.
    Busy,
    /// Refused before anything reached the peer; the old path stays as it was.
    NotStarted(String),
    /// The first request on the new path got no answer and the old path is still heard
    /// from, so the old path stays.
    Unanswered(String),
    /// The session open reached the peer, which then retires the old connection, so the
    /// new one was kept as the outbound connection; the session recovers on it.
    SessionNotMoved(String),
}

/// Moves the peer from `old` to `new` (protocol.md 3.3, 3.5).
///
/// Holds the per-peer dial lock and the switching mark until `new` is the outbound
/// connection, so no new request starts on `old` ([`connect::outbound`] waits). The last
/// busy check comes right before the first request on `new`, the point of no return at the
/// peer: from that request on it retires the old connection. That request opens the session
/// on `new` if it lives on `old`, else it is [`keep`]. A moved session replaces the old one
/// on both sides (3.3: same dialler, newer), and neither ends the old stream before the
/// other has placed the new one, so neither reports the other offline. Then `new` replaces
/// `old` as the outbound connection and its address goes to the front of the last-known
/// list.
///
/// [`connect::outbound`]: super::connect::outbound
async fn switch(
    inner: &Arc<Inner>,
    peer: DeviceId,
    old: &Arc<Connection>,
    new: &Arc<Connection>,
) -> Result<(), SwitchError> {
    let started = {
        let gate = dial_lock(inner, peer);
        let _held = gate.lock().await;
        lock(&inner.paths).entry(peer).or_default().switching = true;
        let _mark = SwitchMark(inner, peer);
        match inner.registry.outbound(&peer) {
            Some(c) if c.id() == old.id() => {}
            _ => {
                return Err(SwitchError::NotStarted(
                    "the connection in use changed during the check".into(),
                ))
            }
        }
        if new.is_closed() || !inner.pins.contains(&peer) {
            return Err(SwitchError::NotStarted("the new connection closed".into()));
        }
        if busy(inner, peer) {
            return Err(SwitchError::Busy);
        }
        let moving = inner
            .registry
            .session(&peer)
            .filter(|s| s.conn.id() == old.id());
        // Point of no return for the peer: it retires `old` at our first request on `new`.
        // `old` stays usable until that request is answered; new requests wait on the dial
        // lock, and the peer still answers any that were already on their way (3.5).
        let answer = match &moving {
            Some(_) => open_on_new(inner, new).await,
            None => match keep(new).await {
                Ok(()) => Answer::Placed,
                Err(e) => Answer::Silent(e),
            },
        };
        if new.is_closed() {
            return Err(SwitchError::SessionNotMoved(
                "the new connection closed".into(),
            ));
        }
        // Unpaired meanwhile: attaching would bring back a link for a forgotten peer.
        if !inner.pins.contains(&peer) {
            new.close(CloseReason::Unpaired);
            return Err(SwitchError::NotStarted("the peer was unpaired".into()));
        }
        // No answer means the new path is not proven. A rare peer that did get the request
        // retires `old` regardless, and a redial then heals it in seconds; a new path that
        // never delivered it is far likelier, and giving up a live `old` for it would leave
        // the peer offline until keep-alive noticed, about a minute.
        if let Answer::Silent(e) = &answer {
            if matches!(
                old.heard_from(REPLACED_PING_DEADLINE).await,
                Heard::Ack | Heard::Bytes
            ) {
                new.close(CloseReason::Duplicate);
                return Err(SwitchError::Unanswered(e.clone()));
            }
        }
        // A newer connection from the same dialler replaces the older one (3.3); ours drains.
        if let Some(replaced) = inner.registry.attach(new).replaced {
            close_if_silent(inner, replaced);
        }
        record_chosen(inner, peer, new.remote_addr());
        answer.into_result()
    };
    let addr = new.remote_addr();
    if let Err(e) = inner
        .pins
        .update(move |s| s.push_last_known(&peer, addr))
        .await
    {
        tracing::debug!(error = %e, "last-known address not stored");
    }
    started.map_err(SwitchError::SessionNotMoved)
}

/// How the first request on a new path went.
enum Answer {
    /// Answered, and the session (if it had to move) is placed.
    Placed,
    /// Answered, so the path works and the peer is retiring the old one, but the session is
    /// not placed.
    NotPlaced(String),
    /// No answer: the path is not proven.
    Silent(String),
}

impl Answer {
    fn into_result(self) -> Result<(), String> {
        match self {
            Self::Placed => Ok(()),
            Self::NotPlaced(e) | Self::Silent(e) => Err(e),
        }
    }
}

/// Opens the session on `new` and waits for it to be placed here, at most
/// [`SESSION_MOVE_TIMEOUT`] in all: the dial lock is held meanwhile and sends wait on it.
async fn open_on_new(inner: &Arc<Inner>, new: &Arc<Connection>) -> Answer {
    let deadline = tokio::time::Instant::now() + SESSION_MOVE_TIMEOUT;
    let sess = match tokio::time::timeout_at(deadline, session::open_outbound(inner, new)).await {
        Ok(Ok(sess)) => sess,
        // A status other than `200` is an answer; an I/O error is not.
        Ok(Err(e @ crate::Error::Protocol(_))) => return Answer::NotPlaced(e.to_string()),
        Ok(Err(e)) => return Answer::Silent(e.to_string()),
        Err(_) => {
            // Cancelled mid-request: the claim is ours to give back so a later attempt
            // can open the session here.
            new.release_session();
            return Answer::Silent("the session request was not answered".to_owned());
        }
    };
    let left = deadline.saturating_duration_since(tokio::time::Instant::now());
    if wait_installed(inner, &sess, left).await {
        Answer::Placed
    } else {
        Answer::NotPlaced("the session did not move to the new connection".to_owned())
    }
}

/// Marks a connection we keep as in use at the peer with a cheap request other than a
/// probe (protocol.md 3.5): `GET /v1/outbox`, which only lists what the peer holds for
/// us. A connection that carried only probes may be closed by the peer after 20 s.
pub(crate) async fn keep(conn: &Connection) -> Result<(), String> {
    let req = http::Request::get("/v1/outbox")
        .body(())
        .map_err(|e| e.to_string())?;
    let (resp, _send) = conn.request(req, true).await.map_err(|e| e.to_string())?;
    match tokio::time::timeout(SESSION_MOVE_TIMEOUT, resp).await {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(e)) => Err(e.to_string()),
        Err(_) => Err("no answer".into()),
    }
}

/// The path report of every paired peer that has one.
pub(crate) fn reports(inner: &Inner) -> Vec<PathReport> {
    let pins: Vec<DeviceId> = inner.pins.read().iter().map(|p| p.device_id).collect();
    let paths = lock(&inner.paths);
    pins.into_iter()
        .filter_map(|id| {
            let live = inner.registry.outbound(&id).map(|c| c.remote_addr());
            let st = paths.get(&id);
            if st.is_none() && live.is_none() {
                return None;
            }
            Some(PathReport {
                peer: short(&id),
                short_id: id.short_id(),
                chosen: live.or_else(|| st.and_then(|s| s.chosen)),
                outcome: st.and_then(|s| s.outcome),
                checked_at: st.and_then(|s| s.checked_at),
                probes: st.map(|s| s.probes.clone()).unwrap_or_default(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIB: f64 = 1024.0 * 1024.0;

    #[test]
    fn the_advert_wait_doubles_while_nothing_changes_up_to_a_cap() {
        let base = Duration::from_secs(600);
        // No check yet, and after the first that changed nothing: the base.
        assert_eq!(advert_wait(base, 0), base);
        assert_eq!(advert_wait(base, 1), base);
        assert_eq!(advert_wait(base, 2), base * 2);
        assert_eq!(advert_wait(base, 3), base * 4);
        assert_eq!(advert_wait(base, 5), base * 16);
        // 160 minutes is the most: nine checks a day at worst.
        assert_eq!(advert_wait(base, 6), base * 16);
        assert_eq!(advert_wait(base, u32::MAX), base * 16);
    }

    #[test]
    fn switches_only_when_clearly_faster() {
        // The 2026-10-28 case: USB tether in use at 27 MiB/s, Wi-Fi measures 50.
        assert_eq!(decide(27.0 * MIB, &[Some(50.0 * MIB)]), Some(0));
        // 20% faster is noise, not a reason to move.
        assert_eq!(decide(40.0 * MIB, &[Some(48.0 * MIB)]), None);
        // Exactly 1.25 times switches.
        assert_eq!(decide(40.0 * MIB, &[Some(50.0 * MIB)]), Some(0));
        // Slower alternatives never win.
        assert_eq!(decide(50.0 * MIB, &[Some(27.0 * MIB)]), None);
    }

    #[test]
    fn picks_the_fastest_and_ignores_failed_probes() {
        let alts = [None, Some(30.0 * MIB), Some(60.0 * MIB), None];
        assert_eq!(decide(10.0 * MIB, &alts), Some(2));
        assert_eq!(decide(10.0 * MIB, &[None, None]), None);
        assert_eq!(decide(10.0 * MIB, &[]), None);
        // A dead current path (nothing measured) loses to any working one.
        assert_eq!(decide(0.0, &[Some(1.0)]), Some(0));
        assert_eq!(decide(0.0, &[Some(0.0)]), None);
    }

    #[test]
    fn summary_line_names_the_path_and_the_probes() {
        let r = PathReport {
            peer: "abcdef01".into(),
            short_id: "K3QD".into(),
            chosen: Some("192.168.1.20:42187".parse().unwrap()),
            outcome: Some(PathOutcome::Switched),
            checked_at: Some(100),
            probes: vec![
                ProbeReport {
                    addr: "192.168.42.129:42187".parse().unwrap(),
                    current: true,
                    bytes_per_sec: Some(27 * 1024 * 1024),
                    error: None,
                    same_path_as: None,
                },
                ProbeReport {
                    addr: "192.168.1.20:42187".parse().unwrap(),
                    current: false,
                    bytes_per_sec: Some(48 * 1024 * 1024),
                    error: None,
                    same_path_as: None,
                },
            ],
        };
        assert_eq!(
            r.summary_line("Phone", 112),
            "note: path to Phone: 192.168.1.20:42187 (switched 12 s ago; \
             192.168.42.129:42187 27.0 MiB/s, 192.168.1.20:42187 48.0 MiB/s)"
        );
        let fresh = PathReport {
            outcome: None,
            checked_at: None,
            probes: Vec::new(),
            ..r
        };
        assert_eq!(
            fresh.summary_line("Phone", 112),
            "note: path to Phone: 192.168.1.20:42187"
        );
    }

    fn addr(a: &str) -> SocketAddr {
        a.parse().unwrap()
    }

    #[test]
    fn set_aside_addresses_are_bounded_and_lapse_into_tried() {
        let mut st = PathState::default();
        let t0 = tokio::time::Instant::now();
        for i in 0..MAX_MERGED + 3 {
            let a = SocketAddr::from(([10, 0, 0, u8::try_from(i).unwrap()], 1));
            note_merged(&mut st, a, t0);
        }
        assert_eq!(st.merged.len(), MAX_MERGED);
        assert_eq!(st.merged[0].0, addr("10.0.0.3:1"), "the oldest went first");
        // Found again: not twice, and its time starts over.
        let later = t0 + Duration::from_secs(10);
        note_merged(&mut st, addr("10.0.0.3:1"), later);
        assert_eq!(st.merged.len(), MAX_MERGED);
        lapse_merged(&mut st, Duration::from_secs(5), later);
        assert_eq!(st.merged.len(), 1);
        assert_eq!(st.merged[0].0, addr("10.0.0.3:1"));
        assert!(
            st.tried.contains(&addr("10.0.0.4:1")),
            "stays known as tried"
        );
    }

    /// Planning here as well would hand `dial_held` a list that no longer says which of the
    /// others is newest.
    #[test]
    fn a_recheck_keeps_the_addresses_newest_first_without_those_left_out() {
        let known = [
            addr("[::ffff:10.0.0.1]:1"),
            addr("[fe80::9%3]:1"),
            addr("[fe80::1%3]:1"),
            addr("[2001:db8::1]:1"),
            addr("10.0.0.2:1"),
            addr("10.0.0.3:1"),
            addr("10.0.0.2:1"),
            addr("10.0.0.4:1"),
        ];
        let got = recheck_candidates(&known, addr("10.0.0.1:1"), &[addr("[fe80::9%3]:1")]);
        assert_eq!(
            got,
            [
                addr("[fe80::1%3]:1"),
                addr("[2001:db8::1]:1"),
                addr("10.0.0.2:1"),
                addr("10.0.0.3:1"),
                addr("10.0.0.4:1"),
            ]
        );
        // So the newest of the others is the one that gets the early start.
        assert_eq!(
            crate::net::plan_candidates(&got, false)[2],
            addr("[fe80::1%3]:1")
        );
    }

    #[test]
    fn summary_line_says_an_address_was_the_same_path() {
        let in_use: SocketAddr = "192.168.86.74:42187".parse().unwrap();
        let r = PathReport {
            peer: "abcdef01".into(),
            short_id: "K3QD".into(),
            chosen: Some(in_use),
            outcome: Some(PathOutcome::Kept),
            checked_at: Some(100),
            probes: vec![
                ProbeReport {
                    addr: in_use,
                    current: true,
                    bytes_per_sec: Some(35 * 1024 * 1024),
                    error: None,
                    same_path_as: None,
                },
                ProbeReport {
                    addr: "[fe80::1%3]:42187".parse().unwrap(),
                    current: false,
                    bytes_per_sec: None,
                    error: None,
                    same_path_as: Some(in_use),
                },
            ],
        };
        assert_eq!(
            r.summary_line("Phone", 100),
            "note: path to Phone: 192.168.86.74:42187 (kept 0 s ago; \
             192.168.86.74:42187 35.0 MiB/s, \
             [fe80::1%3]:42187 same path as 192.168.86.74:42187, not measured)"
        );
        // Older reports have no such field, and a report without one writes none.
        let json = serde_json::to_string(&r.probes[0]).unwrap();
        assert!(!json.contains("same_path_as"), "{json}");
        let back: ProbeReport = serde_json::from_str(&json).unwrap();
        assert_eq!(back, r.probes[0]);
    }
}

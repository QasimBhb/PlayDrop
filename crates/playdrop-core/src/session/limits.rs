//! Connection and HTTP/2 limits, protocol.md section 10 and 6.1 (SR-15, SR-6).
//!
//! [`Limits`] holds the numbers; [`HandshakeGate`], [`ConnectionCounter`] and
//! [`ResetRateCounter`] are the counters the TLS and HTTP/2 layers apply.

use std::collections::{HashMap, VecDeque};
use std::net::IpAddr;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::constants::MAX_FRAME_LEN;

/// SR-15 limits with the protocol.md section 10 defaults, plus the 6.1 unpaired limits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Limits {
    /// TLS handshakes in progress at once (16).
    pub max_pending_handshakes: usize,
    /// Time allowed for a TLS handshake (10 s).
    pub handshake_timeout: Duration,
    /// Live connections per source IP (4).
    pub max_connections_per_ip: usize,
    /// Live connections in total (64).
    pub max_connections_total: usize,
    /// HTTP/2 `MAX_CONCURRENT_STREAMS` (128).
    pub max_concurrent_streams: u32,
    /// HTTP/2 `MAX_HEADER_LIST_SIZE`, CONTINUATION frames included (16 KiB).
    pub max_header_list_size: u32,
    /// Stream resets per second above which the connection closes (100).
    pub max_resets_per_second: usize,
    /// Session frame maximum (64 KiB, protocol.md 5.2).
    pub max_frame_len: usize,
    /// Unpaired connections per source IP during "Add device" (3, protocol.md 6.1).
    pub max_unpaired_per_ip: usize,
    /// `pair_request` frames per minute per source IP (5, protocol.md 6.1).
    pub max_pair_requests_per_minute: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_pending_handshakes: 16,
            handshake_timeout: Duration::from_secs(10),
            max_connections_per_ip: 4,
            max_connections_total: 64,
            max_concurrent_streams: 128,
            max_header_list_size: 16 * 1024,
            max_resets_per_second: 100,
            max_frame_len: MAX_FRAME_LEN,
            max_unpaired_per_ip: 3,
            max_pair_requests_per_minute: 5,
        }
    }
}

/// Bounds concurrent TLS handshakes (SR-15). Hold the permit until the handshake ends.
#[derive(Debug, Clone)]
pub struct HandshakeGate {
    sem: Arc<Semaphore>,
}

impl HandshakeGate {
    /// A gate admitting `max` handshakes at once.
    pub fn new(max: usize) -> Self {
        Self {
            sem: Arc::new(Semaphore::new(max)),
        }
    }

    /// A permit if a slot is free; `None` means drop the TCP connection at once.
    pub fn try_acquire(&self) -> Option<OwnedSemaphorePermit> {
        Arc::clone(&self.sem).try_acquire_owned().ok()
    }
}

/// Which limit refused a connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ConnectionRefused {
    /// The per-IP limit is reached.
    #[error("too many connections from this address")]
    PerIp,
    /// The total limit is reached.
    #[error("too many connections")]
    Total,
}

#[derive(Debug, Default)]
struct ConnState {
    per_ip: HashMap<IpAddr, usize>,
    total: usize,
}

/// Counts live connections per source IP and in total (SR-15). Cheap to clone and share
/// between tasks; each admitted connection holds a [`ConnectionPermit`] that releases its
/// slot on drop. IPv4-mapped IPv6 addresses count as their IPv4 address.
///
/// A second instance with `per_ip` = 3 serves the unpaired limit of protocol.md 6.1.
#[derive(Debug, Clone)]
pub struct ConnectionCounter {
    state: Arc<Mutex<ConnState>>,
    per_ip: usize,
    total: usize,
}

/// A live connection's slot in a [`ConnectionCounter`]. Dropping it frees the slot.
#[derive(Debug)]
pub struct ConnectionPermit {
    state: Arc<Mutex<ConnState>>,
    ip: IpAddr,
}

fn lock(state: &Mutex<ConnState>) -> MutexGuard<'_, ConnState> {
    // The counters stay consistent even if a holder panicked: every update is a single step.
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

impl ConnectionCounter {
    /// A counter with the given per-IP and total limits.
    pub fn new(per_ip: usize, total: usize) -> Self {
        Self {
            state: Arc::new(Mutex::new(ConnState::default())),
            per_ip,
            total,
        }
    }

    /// A counter using [`Limits::max_connections_per_ip`] and
    /// [`Limits::max_connections_total`].
    pub fn from_limits(limits: &Limits) -> Self {
        Self::new(limits.max_connections_per_ip, limits.max_connections_total)
    }

    /// Admits a connection from `ip`, or says which limit refused it.
    pub fn try_acquire(&self, ip: IpAddr) -> Result<ConnectionPermit, ConnectionRefused> {
        let ip = ip.to_canonical();
        let mut s = lock(&self.state);
        if s.total >= self.total {
            return Err(ConnectionRefused::Total);
        }
        let n = s.per_ip.entry(ip).or_insert(0);
        if *n >= self.per_ip {
            return Err(ConnectionRefused::PerIp);
        }
        *n += 1;
        s.total += 1;
        Ok(ConnectionPermit {
            state: Arc::clone(&self.state),
            ip,
        })
    }

    /// Live connections from `ip`.
    pub fn count_for(&self, ip: IpAddr) -> usize {
        lock(&self.state)
            .per_ip
            .get(&ip.to_canonical())
            .copied()
            .unwrap_or(0)
    }

    /// Live connections in total.
    pub fn total(&self) -> usize {
        lock(&self.state).total
    }
}

impl Drop for ConnectionPermit {
    fn drop(&mut self) {
        let mut s = lock(&self.state);
        s.total = s.total.saturating_sub(1);
        if let Some(n) = s.per_ip.get_mut(&self.ip) {
            *n -= 1;
            if *n == 0 {
                s.per_ip.remove(&self.ip);
            }
        }
    }
}

/// Sliding one-second window of HTTP/2 stream resets for one connection (SR-15, the 2023
/// rapid-reset attack). [`ResetRateCounter::record`] returns true once the rate is exceeded.
#[derive(Debug, Clone)]
pub struct ResetRateCounter {
    max_per_second: usize,
    events: VecDeque<Instant>,
}

impl ResetRateCounter {
    /// A counter that trips above `max_per_second` resets in any one-second window.
    pub fn new(max_per_second: usize) -> Self {
        Self {
            max_per_second,
            events: VecDeque::with_capacity(max_per_second + 1),
        }
    }

    /// Records a reset at `now`; true means close the connection.
    pub fn record(&mut self, now: Instant) -> bool {
        let window = Duration::from_secs(1);
        while let Some(&oldest) = self.events.front() {
            if now.saturating_duration_since(oldest) >= window {
                self.events.pop_front();
            } else {
                break;
            }
        }
        self.events.push_back(now);
        self.events.len() > self.max_per_second
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    #[test]
    fn defaults_match_section_10() {
        let l = Limits::default();
        assert_eq!(l.max_pending_handshakes, 16);
        assert_eq!(l.handshake_timeout, Duration::from_secs(10));
        assert_eq!(l.max_connections_per_ip, 4);
        assert_eq!(l.max_connections_total, 64);
        assert_eq!(l.max_concurrent_streams, 128);
        assert_eq!(l.max_header_list_size, 16384);
        assert_eq!(l.max_resets_per_second, 100);
        assert_eq!(l.max_frame_len, 65536);
    }

    #[test]
    fn per_ip_and_total_limits() {
        let c = ConnectionCounter::new(2, 3);
        let a = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 2));
        let mapped = IpAddr::V6(Ipv4Addr::new(192, 168, 1, 2).to_ipv6_mapped());
        let b = IpAddr::V6(Ipv6Addr::LOCALHOST);
        let p1 = c.try_acquire(a).unwrap();
        let _p2 = c.try_acquire(mapped).unwrap();
        assert_eq!(c.try_acquire(a).unwrap_err(), ConnectionRefused::PerIp);
        let _p3 = c.try_acquire(b).unwrap();
        assert_eq!(c.try_acquire(b).unwrap_err(), ConnectionRefused::Total);
        drop(p1);
        assert_eq!(c.count_for(a), 1);
        assert_eq!(c.total(), 2);
        assert!(c.try_acquire(a).is_ok());
    }

    #[test]
    fn handshake_gate() {
        let g = HandshakeGate::new(2);
        let a = g.try_acquire().unwrap();
        let _b = g.try_acquire().unwrap();
        assert!(g.try_acquire().is_none());
        drop(a);
        assert!(g.try_acquire().is_some());
    }

    #[test]
    fn reset_rate() {
        let mut r = ResetRateCounter::new(100);
        let t0 = Instant::now();
        for i in 0..100u64 {
            assert!(!r.record(t0 + Duration::from_millis(i * 5)));
        }
        assert!(r.record(t0 + Duration::from_millis(600)));
        // Spread over more than a second the same count is fine.
        let mut r = ResetRateCounter::new(100);
        for i in 0..300u64 {
            assert!(!r.record(t0 + Duration::from_millis(i * 11)));
        }
    }
}

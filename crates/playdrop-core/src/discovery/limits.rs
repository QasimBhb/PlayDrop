//! Input rate limits and the dial limiter, `docs/protocol.md` section 8.3 rules 1 and 3
//! (SR-1, SR-2).
//!
//! All limiters take the current time as an argument so tests can drive them with
//! `tokio::time::pause` and `advance`.

use std::collections::HashMap;
use std::net::IpAddr;
use std::time::Duration;

use tokio::time::Instant;

/// Packets accepted per second from one source IP (protocol.md 8.3 rule 3, SR-2).
pub const PER_SOURCE_PER_SEC: u32 = 5;

/// Packets accepted per second in total (protocol.md 8.3 rule 3, SR-2).
pub const GLOBAL_PER_SEC: u32 = 200;

/// Shortest gap between two dials to the same peer (protocol.md 8.3 rule 1, SR-1).
pub const DIAL_INTERVAL: Duration = Duration::from_secs(5);

/// Fixed one-second window limiter: at most `per_source` packets per source IP and `global`
/// in total per window; the rest are to be dropped unparsed (protocol.md 8.3 rule 3, SR-2).
///
/// The per-source table only gains an entry for a packet that passed the global limit, so it
/// never holds more than `global` entries, however many source addresses a flood uses.
#[derive(Debug)]
pub struct RateLimiter {
    per_source: u32,
    global: u32,
    window: Duration,
    window_start: Option<Instant>,
    total: u32,
    per_ip: HashMap<IpAddr, u32>,
}

impl RateLimiter {
    /// A limiter with the protocol limits (5 per source, 200 overall, per second).
    pub fn new() -> Self {
        Self::with_limits(PER_SOURCE_PER_SEC, GLOBAL_PER_SEC)
    }

    /// A limiter with custom per-second limits.
    pub fn with_limits(per_source: u32, global: u32) -> Self {
        Self {
            per_source,
            global,
            window: Duration::from_secs(1),
            window_start: None,
            total: 0,
            per_ip: HashMap::new(),
        }
    }

    /// Records one packet from `src` and says whether it may be parsed.
    pub fn allow(&mut self, src: IpAddr, now: Instant) -> bool {
        let fresh = self
            .window_start
            .is_none_or(|start| now.duration_since(start) >= self.window);
        if fresh {
            self.window_start = Some(now);
            self.total = 0;
            self.per_ip.clear();
        }
        if self.total >= self.global {
            return false;
        }
        let count = self.per_ip.entry(src).or_insert(0);
        if *count >= self.per_source {
            return false;
        }
        *count += 1;
        self.total += 1;
        true
    }
}

impl Default for RateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

/// Allows at most one dial per peer per [`DIAL_INTERVAL`] (protocol.md 8.3 rule 1, SR-1).
///
/// Discovery itself never dials; the host's connection manager owns one of these and asks it
/// before acting on a `DiscoveryEvent::Verified`.
#[derive(Debug, Default)]
pub struct DialLimiter {
    last: HashMap<String, Instant>,
}

impl DialLimiter {
    /// Entries kept before stale ones are swept. Peers are pinned devices, so this is ample.
    const SWEEP_AT: usize = 256;

    /// A limiter with no history.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns `true` and records the dial if `device_id` was not dialled in the last
    /// [`DIAL_INTERVAL`]; returns `false` otherwise.
    pub fn try_dial(&mut self, device_id: &str, now: Instant) -> bool {
        if let Some(prev) = self.last.get(device_id) {
            if now.duration_since(*prev) < DIAL_INTERVAL {
                return false;
            }
        }
        if self.last.len() >= Self::SWEEP_AT {
            self.last
                .retain(|_, t| now.duration_since(*t) < DIAL_INTERVAL);
        }
        self.last.insert(device_id.to_owned(), now);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::test_clock::TestClock;
    use std::net::Ipv4Addr;

    fn ip(last: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(192, 168, 1, last))
    }

    fn ip_n(n: u32) -> IpAddr {
        IpAddr::V4(Ipv4Addr::from(0x0a00_0000 + n))
    }

    #[test]
    fn per_source_limit_resets_each_second() {
        let clock = TestClock::default();
        let mut rl = RateLimiter::new();
        let now = clock.now();
        assert_eq!((0..10).filter(|_| rl.allow(ip(1), now)).count(), 5);
        // Another source is not affected.
        assert!(rl.allow(ip(2), now));
        clock.advance(Duration::from_millis(999));
        assert!(!rl.allow(ip(1), clock.now()));
        clock.advance(Duration::from_millis(1));
        assert_eq!((0..10).filter(|_| rl.allow(ip(1), clock.now())).count(), 5);
    }

    #[test]
    fn global_limit_caps_floods_and_table_size() {
        let clock = TestClock::default();
        let mut rl = RateLimiter::new();
        let now = clock.now();
        let passed = (0..100_000).filter(|i| rl.allow(ip_n(*i), now)).count();
        assert_eq!(passed, 200);
        assert!(rl.per_ip.len() <= 200);
        clock.advance(Duration::from_secs(1));
        assert!(rl.allow(ip_n(0), clock.now()));
        assert_eq!(rl.per_ip.len(), 1);
    }

    #[test]
    fn dial_limiter_once_per_five_seconds() {
        let clock = TestClock::default();
        let mut dl = DialLimiter::new();
        assert!(dl.try_dial("a", clock.now()));
        assert!(!dl.try_dial("a", clock.now()));
        assert!(dl.try_dial("b", clock.now()));
        clock.advance(Duration::from_millis(4999));
        assert!(!dl.try_dial("a", clock.now()));
        clock.advance(Duration::from_millis(1));
        assert!(dl.try_dial("a", clock.now()));
    }

    #[test]
    fn dial_limiter_sweeps_stale_entries() {
        let clock = TestClock::default();
        let mut dl = DialLimiter::new();
        for i in 0..300 {
            assert!(dl.try_dial(&i.to_string(), clock.now()));
        }
        clock.advance(DIAL_INTERVAL);
        assert!(dl.try_dial("x", clock.now()));
        assert!(dl.last.len() <= 2);
    }
}

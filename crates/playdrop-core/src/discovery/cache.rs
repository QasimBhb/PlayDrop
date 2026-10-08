//! Bounded discovery state, `docs/protocol.md` section 8.3 rule 3 (SR-2).
//!
//! - Advert cache: 256 entries, 120 s TTL, least recently used evicted first.
//! - "Add device" list: 32 entries, at most 10 new entries per minute.
//!
//! Both sit on [`LruTtl`], a small map with O(log n) operations and a hard size cap, so a
//! flood of unique adverts cannot grow memory past the caps.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::hash::Hash;
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use tokio::time::Instant;

use super::tag::{DeviceIdString, NONCE_LEN, TAG_LEN};

/// Advert cache capacity (protocol.md 8.3 rule 3).
pub const ADVERT_CACHE_CAP: usize = 256;

/// Advert cache entry lifetime (protocol.md 8.3 rule 3).
pub const ADVERT_CACHE_TTL: Duration = Duration::from_secs(120);

/// "Add device" list capacity (protocol.md 8.3 rule 3).
pub const ADD_DEVICE_CAP: usize = 32;

/// New "Add device" entries admitted per [`ADD_DEVICE_RATE_WINDOW`] (protocol.md 8.3 rule 3).
pub const ADD_DEVICE_NEW_PER_MINUTE: usize = 10;

/// Window for [`ADD_DEVICE_NEW_PER_MINUTE`].
pub const ADD_DEVICE_RATE_WINDOW: Duration = Duration::from_secs(60);

/// "Add device" entry lifetime. *Choice:* the window itself lasts at most 120 s
/// (protocol.md 6.1), so an entry never needs to outlive it.
pub const ADD_DEVICE_TTL: Duration = Duration::from_secs(120);

/// A size-capped map whose entries expire `ttl` after their last insert and whose least
/// recently used entry is evicted when full (protocol.md 8.3 rule 3, SR-2).
#[derive(Debug)]
pub struct LruTtl<K, V> {
    cap: usize,
    ttl: Duration,
    tick: u64,
    map: HashMap<K, Slot<V>>,
    order: BTreeMap<u64, K>,
}

#[derive(Debug)]
struct Slot<V> {
    value: V,
    inserted: Instant,
    tick: u64,
}

impl<K: Eq + Hash + Clone, V> LruTtl<K, V> {
    /// An empty map holding at most `cap` entries for `ttl` each.
    pub fn new(cap: usize, ttl: Duration) -> Self {
        Self {
            cap,
            ttl,
            tick: 0,
            map: HashMap::new(),
            order: BTreeMap::new(),
        }
    }

    /// Number of live or not yet swept entries.
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// `true` when the map holds nothing.
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// `true` if `key` is present and unexpired. Does not touch recency.
    pub fn contains(&self, key: &K, now: Instant) -> bool {
        self.map
            .get(key)
            .is_some_and(|s| now.duration_since(s.inserted) < self.ttl)
    }

    /// Returns the value for `key` if unexpired and marks it most recently used. An expired
    /// entry is removed.
    pub fn get(&mut self, key: &K, now: Instant) -> Option<&V> {
        let expired = now.duration_since(self.map.get(key)?.inserted) >= self.ttl;
        if expired {
            self.remove(key);
            return None;
        }
        self.tick += 1;
        let tick = self.tick;
        let slot = self.map.get_mut(key)?;
        self.order.remove(&slot.tick);
        slot.tick = tick;
        self.order.insert(tick, key.clone());
        Some(&self.map.get(key)?.value)
    }

    /// Inserts or replaces `key`, restarting its TTL. When full, expired entries are swept
    /// first and then the least recently used entry is evicted.
    pub fn insert(&mut self, key: K, value: V, now: Instant) {
        self.remove(&key);
        if self.map.len() >= self.cap {
            self.sweep(now);
        }
        while self.map.len() >= self.cap {
            let Some((_, oldest)) = self.order.pop_first() else {
                break;
            };
            self.map.remove(&oldest);
        }
        self.tick += 1;
        self.order.insert(self.tick, key.clone());
        self.map.insert(
            key,
            Slot {
                value,
                inserted: now,
                tick: self.tick,
            },
        );
    }

    /// Removes `key`, returning its value.
    pub fn remove(&mut self, key: &K) -> Option<V> {
        let slot = self.map.remove(key)?;
        self.order.remove(&slot.tick);
        Some(slot.value)
    }

    /// Drops every expired entry.
    pub fn sweep(&mut self, now: Instant) {
        let ttl = self.ttl;
        let order = &mut self.order;
        self.map.retain(|_, s| {
            let keep = now.duration_since(s.inserted) < ttl;
            if !keep {
                order.remove(&s.tick);
            }
            keep
        });
    }

    /// Drops everything.
    pub fn clear(&mut self) {
        self.map.clear();
        self.order.clear();
    }

    /// Iterates over unexpired entries in no particular order.
    pub fn iter(&self, now: Instant) -> impl Iterator<Item = (&K, &V)> {
        self.map
            .iter()
            .filter(move |(_, s)| now.duration_since(s.inserted) < self.ttl)
            .map(|(k, s)| (k, &s.value))
    }
}

/// What a cached advert turned out to be (protocol.md 8.3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TagOutcome {
    /// The tag verified against this pinned peer's announce key.
    Peer(DeviceIdString),
    /// The tag matched no pinned peer.
    Unknown,
}

/// Cache key: who sent which nonce and tag. A replay from another address is a new entry, so
/// the dial target is always the packet's own source (protocol.md 8.3 rule 1).
pub type AdvertKey = (IpAddr, [u8; NONCE_LEN], [u8; TAG_LEN]);

/// Advert cache: remembers tag outcomes so repeated adverts (mDNS repeats the same `n` for up
/// to 15 minutes) skip the HMAC over every pin (protocol.md 8.3 rule 3, SR-2).
#[derive(Debug)]
pub struct AdvertCache {
    inner: LruTtl<AdvertKey, TagOutcome>,
    generation: u64,
}

impl AdvertCache {
    /// A cache with the protocol limits (256 entries, 120 s).
    pub fn new() -> Self {
        Self {
            inner: LruTtl::new(ADVERT_CACHE_CAP, ADVERT_CACHE_TTL),
            generation: 0,
        }
    }

    /// Clears the cache if the pin set `generation` changed since the last call, so a new or
    /// removed pin takes effect at once (SR-5).
    pub fn sync_generation(&mut self, generation: u64) {
        if generation != self.generation {
            self.generation = generation;
            self.inner.clear();
        }
    }

    /// Cached outcome for `key`, if any.
    pub fn get(&mut self, key: &AdvertKey, now: Instant) -> Option<TagOutcome> {
        self.inner.get(key, now).cloned()
    }

    /// Stores the outcome for `key`.
    pub fn insert(&mut self, key: AdvertKey, outcome: TagOutcome, now: Instant) {
        self.inner.insert(key, outcome, now);
    }

    /// Drops every entry.
    pub fn clear(&mut self) {
        self.inner.clear();
    }

    /// Number of entries.
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// `true` when empty.
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }
}

impl Default for AdvertCache {
    fn default() -> Self {
        Self::new()
    }
}

/// One unpaired device shown in the "Add device" list (protocol.md 8.3 rules 1 and 4).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AddDeviceEntry {
    /// Sanitised display name.
    pub name: String,
    /// Device type token.
    pub device_type: String,
}

/// The "Add device" list: 32 entries, at most 10 new per minute (protocol.md 8.3 rule 3).
///
/// Keyed by source IP and advertised port, since named adverts carry no stable identifier.
/// Refreshing an entry that is already listed is always allowed; only new entries count
/// against the admission rate.
#[derive(Debug)]
pub struct AddDeviceList {
    inner: LruTtl<SocketAddr, AddDeviceEntry>,
    admitted: VecDeque<Instant>,
}

impl AddDeviceList {
    /// A list with the protocol limits.
    pub fn new() -> Self {
        Self {
            inner: LruTtl::new(ADD_DEVICE_CAP, ADD_DEVICE_TTL),
            admitted: VecDeque::new(),
        }
    }

    /// Adds or refreshes `addr`. Returns `false` if it is new and the admission rate is spent.
    /// When full, the least recently seen entry makes room.
    pub fn offer(&mut self, addr: SocketAddr, entry: AddDeviceEntry, now: Instant) -> bool {
        if !self.inner.contains(&addr, now) {
            while self
                .admitted
                .front()
                .is_some_and(|t| now.duration_since(*t) >= ADD_DEVICE_RATE_WINDOW)
            {
                self.admitted.pop_front();
            }
            if self.admitted.len() >= ADD_DEVICE_NEW_PER_MINUTE {
                return false;
            }
            self.admitted.push_back(now);
        }
        self.inner.insert(addr, entry, now);
        true
    }

    /// Current entries.
    pub fn entries(&self, now: Instant) -> Vec<(SocketAddr, AddDeviceEntry)> {
        self.inner.iter(now).map(|(a, e)| (*a, e.clone())).collect()
    }

    /// Empties the list, for when the "Add device" window closes. The admission history is
    /// kept, so closing and reopening the window cannot be used to skip the rate limit.
    pub fn clear(&mut self) {
        self.inner.clear();
    }

    /// Number of entries.
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// `true` when empty.
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }
}

impl Default for AddDeviceList {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::test_clock::TestClock;
    use std::net::Ipv4Addr;

    fn sa(n: u8) -> SocketAddr {
        SocketAddr::from((Ipv4Addr::new(192, 168, 1, n), 42187))
    }

    fn entry() -> AddDeviceEntry {
        AddDeviceEntry {
            name: "x".into(),
            device_type: "phone".into(),
        }
    }

    #[test]
    fn lru_evicts_least_recently_used() {
        let clock = TestClock::default();
        let now = clock.now();
        let mut c: LruTtl<u32, u32> = LruTtl::new(3, Duration::from_secs(120));
        c.insert(1, 10, now);
        c.insert(2, 20, now);
        c.insert(3, 30, now);
        assert_eq!(c.get(&1, now), Some(&10)); // 1 is now most recent
        c.insert(4, 40, now); // evicts 2
        assert_eq!(c.get(&2, now), None);
        assert_eq!(c.get(&1, now), Some(&10));
        assert_eq!(c.get(&3, now), Some(&30));
        assert_eq!(c.get(&4, now), Some(&40));
        assert_eq!(c.len(), 3);
        assert_eq!(c.order.len(), 3);
    }

    #[test]
    fn ttl_expires_entries() {
        let clock = TestClock::default();
        let mut c: LruTtl<u32, u32> = LruTtl::new(3, ADVERT_CACHE_TTL);
        c.insert(1, 10, clock.now());
        clock.advance(Duration::from_secs(119));
        assert_eq!(c.get(&1, clock.now()), Some(&10));
        clock.advance(Duration::from_secs(1));
        assert_eq!(c.get(&1, clock.now()), None);
        assert!(c.is_empty());
        assert!(c.order.is_empty());
    }

    #[test]
    fn full_cache_sweeps_expired_before_evicting_live() {
        let clock = TestClock::default();
        let mut c: LruTtl<u32, u32> = LruTtl::new(2, Duration::from_secs(10));
        c.insert(1, 1, clock.now());
        clock.advance(Duration::from_secs(5));
        c.insert(2, 2, clock.now());
        c.get(&1, clock.now());
        clock.advance(Duration::from_secs(6));
        // 1 has expired even though it is most recent; it goes, 2 stays.
        c.insert(3, 3, clock.now());
        assert!(c.contains(&2, clock.now()));
        assert!(c.contains(&3, clock.now()));
        assert_eq!(c.len(), 2);
    }

    #[test]
    fn advert_cache_is_capped_under_flood() {
        let clock = TestClock::default();
        let mut c = AdvertCache::new();
        let now = clock.now();
        for i in 0u32..10_000 {
            let mut n = [0u8; 16];
            n[..4].copy_from_slice(&i.to_be_bytes());
            c.insert(
                (IpAddr::V4(Ipv4Addr::LOCALHOST), n, [0; 16]),
                TagOutcome::Unknown,
                now,
            );
        }
        assert_eq!(c.len(), ADVERT_CACHE_CAP);
        assert_eq!(c.inner.order.len(), ADVERT_CACHE_CAP);
    }

    #[test]
    fn advert_cache_clears_on_generation_change() {
        let clock = TestClock::default();
        let mut c = AdvertCache::new();
        let key = (IpAddr::V4(Ipv4Addr::LOCALHOST), [1; 16], [2; 16]);
        c.insert(key, TagOutcome::Unknown, clock.now());
        c.sync_generation(0);
        assert_eq!(c.len(), 1);
        c.sync_generation(1);
        assert!(c.is_empty());
    }

    #[test]
    fn add_device_admits_ten_new_per_minute() {
        let clock = TestClock::default();
        let mut l = AddDeviceList::new();
        let now = clock.now();
        assert_eq!(
            (0..20).filter(|i| l.offer(sa(*i), entry(), now)).count(),
            10
        );
        // Refreshing a listed entry still works.
        assert!(l.offer(sa(0), entry(), now));
        clock.advance(Duration::from_secs(59));
        assert!(!l.offer(sa(50), entry(), clock.now()));
        clock.advance(Duration::from_secs(1));
        assert!(l.offer(sa(50), entry(), clock.now()));
    }

    #[test]
    fn add_device_caps_at_32() {
        let clock = TestClock::default();
        let mut l = AddDeviceList::new();
        // One new device every 6 s stays within 10 per minute; refreshing the listed ones
        // keeps them alive past the TTL, so only the size cap can bound the list.
        for i in 0..40u8 {
            assert!(l.offer(sa(i), entry(), clock.now()));
            for (addr, e) in l.entries(clock.now()) {
                assert!(l.offer(addr, e, clock.now()));
            }
            clock.advance(Duration::from_secs(6));
        }
        assert_eq!(l.len(), ADD_DEVICE_CAP);
    }
}

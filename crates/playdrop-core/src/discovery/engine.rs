//! Socket-free advert handling shared by the beacon and mDNS paths, `docs/protocol.md`
//! section 8.3 (SR-1 to SR-5).
//!
//! [`Processor::handle`] turns one parsed advert plus its source address into at most one
//! [`DiscoveryEvent`]. It holds the advert cache and the "Add device" list. Rate limiting
//! and size checks happen before parsing, in the carriers.

use std::collections::VecDeque;
use std::net::{IpAddr, SocketAddr, SocketAddrV6};
use std::sync::Mutex;

use tokio::time::Instant;

use super::advert::Advert;
use super::cache::{AddDeviceEntry, AddDeviceList, AdvertCache, TagOutcome};
use super::tag::{verify, PeerKeys, NONCE_LEN};
use super::{acceptable_source, DiscoveryEvent, Ipv4Interface, Via};

/// How many recently sent beacon nonces are remembered for own-advert detection
/// (protocol.md 8.3 rule 5). Loopback copies arrive within milliseconds; 256 covers many
/// bursts on many interfaces.
pub const OWN_NONCE_MEMORY: usize = 256;

/// Nonces this host has sent, so its own adverts are dropped (protocol.md 8.3 rule 5).
#[derive(Debug, Default)]
pub struct OwnNonces {
    inner: Mutex<OwnInner>,
}

#[derive(Debug, Default)]
struct OwnInner {
    recent: VecDeque<[u8; NONCE_LEN]>,
    /// The nonce in the current mDNS TXT record, which lives for up to 15 minutes and so is
    /// kept outside the ring.
    mdns: Option<[u8; NONCE_LEN]>,
}

impl OwnNonces {
    /// Remembers a beacon nonce about to be sent.
    pub fn record(&self, n: [u8; NONCE_LEN]) {
        let mut g = self.lock();
        if g.recent.len() >= OWN_NONCE_MEMORY {
            g.recent.pop_front();
        }
        g.recent.push_back(n);
    }

    /// Sets the nonce of the currently registered mDNS record.
    pub fn set_mdns(&self, n: Option<[u8; NONCE_LEN]>) {
        self.lock().mdns = n;
    }

    /// `true` if `n` was sent by this host.
    pub fn contains(&self, n: &[u8; NONCE_LEN]) -> bool {
        let g = self.lock();
        g.mdns.as_ref() == Some(n) || g.recent.contains(n)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, OwnInner> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Everything [`Processor::handle`] needs to know about the host at this moment.
pub struct Context<'a> {
    /// Pinned peers and their announce keys.
    pub keys: &'a dyn PeerKeys,
    /// Whether the local "Add device" window is open (protocol.md 6.1).
    pub add_device_window: bool,
    /// Current usable IPv4 interfaces, for the on-link check (SR-3).
    pub interfaces: &'a [Ipv4Interface],
    /// Nonces this host sent.
    pub own: &'a OwnNonces,
}

/// One received advert and where it came from.
#[derive(Clone, Copy, Debug)]
pub struct Inbound<'a> {
    /// The parsed advert.
    pub advert: &'a Advert,
    /// Packet source (beacon) or resolved address (mDNS).
    pub src: IpAddr,
    /// IPv6 scope (interface index); 0 for IPv4.
    pub scope_id: u32,
    /// Carrier.
    pub via: Via,
}

/// Decision logic for adverts (protocol.md 8.3).
#[derive(Debug, Default)]
pub struct Processor {
    cache: AdvertCache,
    add_list: AddDeviceList,
    window_was_open: bool,
}

impl Processor {
    /// A processor with empty state.
    pub fn new() -> Self {
        Self::default()
    }

    /// Drops all cached tag outcomes, for when pins change and `PeerKeys::generation` is not
    /// implemented (SR-5).
    pub fn peers_changed(&mut self) {
        self.cache.clear();
    }

    /// The current "Add device" list.
    pub fn add_device_entries(&self, now: Instant) -> Vec<(SocketAddr, AddDeviceEntry)> {
        self.add_list.entries(now)
    }

    /// Handles one advert (protocol.md 8.3):
    ///
    /// 1. Own adverts are dropped (rule 5).
    /// 2. The source must be on-link unicast, never multicast, broadcast or loopback
    ///    (rules 1 and 2).
    /// 3. A tag that verifies against a pinned key yields `Verified` with the stored port
    ///    (rule 1). This wins even if the advert also carries "Add device" fields.
    /// 4. Otherwise a named advert yields `Unpaired` only while the local window is open and
    ///    the "Add device" list admits it (rule 3).
    pub fn handle(
        &mut self,
        input: Inbound<'_>,
        ctx: &Context<'_>,
        now: Instant,
    ) -> Option<DiscoveryEvent> {
        let Inbound {
            advert,
            src,
            scope_id,
            via,
        } = input;
        if ctx.own.contains(&advert.n) {
            return None;
        }
        if !acceptable_source(src, scope_id, ctx.interfaces) {
            return None;
        }
        self.cache.sync_generation(ctx.keys.generation());

        let key = (src, advert.n, advert.t);
        let outcome = if let Some(hit) = self.cache.get(&key, now) {
            hit
        } else {
            let keys = ctx.keys.announce_keys();
            let outcome = verify(&keys, &advert.n, &advert.t)
                .map_or(TagOutcome::Unknown, |id| TagOutcome::Peer(id.to_owned()));
            self.cache.insert(key, outcome.clone(), now);
            outcome
        };

        if let TagOutcome::Peer(device_id) = outcome {
            let port = ctx.keys.stored_port(&device_id);
            return Some(DiscoveryEvent::Verified {
                addr: socket_addr(src, port, scope_id),
                device_id,
                seen_at: now,
                via,
            });
        }

        if !ctx.add_device_window {
            if self.window_was_open {
                self.add_list.clear();
                self.window_was_open = false;
            }
            return None;
        }
        self.window_was_open = true;
        let fields = advert.add_device.as_ref()?;
        let addr = socket_addr(src, fields.port, scope_id);
        let entry = AddDeviceEntry {
            name: fields.name.clone(),
            device_type: fields.device_type.clone(),
        };
        if !self.add_list.offer(addr, entry, now) {
            return None;
        }
        Some(DiscoveryEvent::Unpaired {
            name: fields.name.clone(),
            device_type: fields.device_type.clone(),
            addr,
            seen_at: now,
            via,
        })
    }
}

fn socket_addr(ip: IpAddr, port: u16, scope_id: u32) -> SocketAddr {
    match ip {
        IpAddr::V4(v4) => SocketAddr::from((v4, port)),
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => SocketAddr::from((v4, port)),
            None => SocketAddr::V6(SocketAddrV6::new(v6, port, 0, scope_id)),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::advert::AddDeviceFields;
    use crate::discovery::tag::{tag, DeviceIdString};
    use crate::discovery::test_clock::TestClock;
    use std::net::Ipv4Addr;
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

    struct Keys {
        list: Vec<(DeviceIdString, [u8; 32])>,
        calls: AtomicUsize,
        generation: AtomicU64,
    }

    impl PeerKeys for Keys {
        fn announce_keys(&self) -> Vec<(DeviceIdString, [u8; 32])> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            self.list.clone()
        }
        fn stored_port(&self, _: &str) -> u16 {
            40000
        }
        fn generation(&self) -> u64 {
            self.generation.load(Ordering::Relaxed)
        }
    }

    fn keys() -> Keys {
        Keys {
            list: vec![("a".repeat(64), [1; 32])],
            calls: AtomicUsize::new(0),
            generation: AtomicU64::new(0),
        }
    }

    fn lan() -> Vec<Ipv4Interface> {
        vec![Ipv4Interface {
            index: 2,
            addr: Ipv4Addr::new(10, 0, 0, 1),
            netmask: Ipv4Addr::new(255, 255, 255, 0),
        }]
    }

    fn peer_ip() -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(10, 0, 0, 7))
    }

    fn signed(key: [u8; 32], n: u8) -> Advert {
        let n = [n; 16];
        Advert {
            v: 1,
            n,
            t: tag(&key, &n),
            add_device: None,
        }
    }

    fn inbound(advert: &Advert, src: IpAddr) -> Inbound<'_> {
        Inbound {
            advert,
            src,
            scope_id: 0,
            via: Via::Beacon,
        }
    }

    #[test]
    fn verified_uses_source_ip_and_stored_port() {
        let clock = TestClock::default();
        let pk = keys();
        let own = OwnNonces::default();
        let ifs = lan();
        let ctx = Context {
            keys: &pk,
            add_device_window: false,
            interfaces: &ifs,
            own: &own,
        };
        let mut p = Processor::new();
        let a = signed([1; 32], 1);
        let ev = p.handle(inbound(&a, peer_ip()), &ctx, clock.now()).unwrap();
        match ev {
            DiscoveryEvent::Verified {
                device_id, addr, ..
            } => {
                assert_eq!(device_id, "a".repeat(64));
                assert_eq!(addr, SocketAddr::from(([10, 0, 0, 7], 40000)));
            }
            DiscoveryEvent::Unpaired { .. } => panic!("expected Verified"),
        }
        // A repeat is served from the cache, without recomputing tags.
        assert!(p
            .handle(inbound(&a, peer_ip()), &ctx, clock.now())
            .is_some());
        assert_eq!(pk.calls.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn forged_and_misplaced_adverts_produce_nothing() {
        let clock = TestClock::default();
        let pk = keys();
        let own = OwnNonces::default();
        let ifs = lan();
        let ctx = Context {
            keys: &pk,
            add_device_window: false,
            interfaces: &ifs,
            own: &own,
        };
        let mut p = Processor::new();
        // 10,000 forged tags (SR-1 test).
        for i in 0..10_000u32 {
            let mut n = [0u8; 16];
            n[..4].copy_from_slice(&i.to_be_bytes());
            let a = Advert {
                v: 1,
                n,
                t: [0xee; 16],
                add_device: None,
            };
            assert!(p
                .handle(inbound(&a, peer_ip()), &ctx, clock.now())
                .is_none());
        }
        // A genuine tag from an off-link, loopback or broadcast source is dropped too.
        let a = signed([1; 32], 2);
        for src in [
            [10, 0, 1, 7],
            [127, 0, 0, 1],
            [10, 0, 0, 255],
            [255, 255, 255, 255],
        ] {
            assert!(p
                .handle(
                    inbound(&a, IpAddr::V4(Ipv4Addr::from(src))),
                    &ctx,
                    clock.now()
                )
                .is_none());
        }
    }

    #[test]
    fn own_adverts_are_dropped() {
        let clock = TestClock::default();
        let pk = Keys {
            list: vec![("me".into(), [1; 32])],
            calls: AtomicUsize::new(0),
            generation: AtomicU64::new(0),
        };
        let own = OwnNonces::default();
        let ifs = lan();
        let ctx = Context {
            keys: &pk,
            add_device_window: true,
            interfaces: &ifs,
            own: &own,
        };
        let a = signed([1; 32], 3);
        own.record(a.n);
        assert!(Processor::new()
            .handle(inbound(&a, peer_ip()), &ctx, clock.now())
            .is_none());
        let b = signed([1; 32], 4);
        own.set_mdns(Some(b.n));
        assert!(Processor::new()
            .handle(inbound(&b, peer_ip()), &ctx, clock.now())
            .is_none());
    }

    #[test]
    fn generation_change_invalidates_cached_outcomes() {
        let clock = TestClock::default();
        let pk = keys();
        let own = OwnNonces::default();
        let ifs = lan();
        let ctx = Context {
            keys: &pk,
            add_device_window: false,
            interfaces: &ifs,
            own: &own,
        };
        let mut p = Processor::new();
        let a = signed([1; 32], 5);
        p.handle(inbound(&a, peer_ip()), &ctx, clock.now());
        pk.generation.store(1, Ordering::Relaxed);
        p.handle(inbound(&a, peer_ip()), &ctx, clock.now());
        assert_eq!(pk.calls.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn unpaired_only_while_window_open() {
        let clock = TestClock::default();
        let pk = keys();
        let own = OwnNonces::default();
        let ifs = lan();
        let mut p = Processor::new();
        let named = Advert {
            v: 1,
            n: [9; 16],
            t: [9; 16],
            add_device: Some(AddDeviceFields {
                name: "Pixel".into(),
                device_type: "phone".into(),
                port: 42187,
            }),
        };
        let closed = Context {
            keys: &pk,
            add_device_window: false,
            interfaces: &ifs,
            own: &own,
        };
        assert!(p
            .handle(inbound(&named, peer_ip()), &closed, clock.now())
            .is_none());
        let open = Context {
            keys: &pk,
            add_device_window: true,
            interfaces: &ifs,
            own: &own,
        };
        let ev = p
            .handle(inbound(&named, peer_ip()), &open, clock.now())
            .unwrap();
        assert!(matches!(ev, DiscoveryEvent::Unpaired { ref name, addr, .. }
            if name == "Pixel" && addr == SocketAddr::from(([10, 0, 0, 7], 42187))));
        // A private advert with an unknown tag never shows up, window or not.
        let anon = Advert {
            add_device: None,
            ..named.clone()
        };
        assert!(p
            .handle(inbound(&anon, peer_ip()), &open, clock.now())
            .is_none());
        // Closing the window empties the list.
        p.handle(inbound(&anon, peer_ip()), &closed, clock.now());
        assert_eq!(p.add_device_entries(clock.now()), Vec::new());
    }

    #[test]
    fn pinned_peer_in_add_device_mode_is_verified_not_unpaired() {
        let clock = TestClock::default();
        let pk = keys();
        let own = OwnNonces::default();
        let ifs = lan();
        let ctx = Context {
            keys: &pk,
            add_device_window: true,
            interfaces: &ifs,
            own: &own,
        };
        let mut a = signed([1; 32], 6);
        a.add_device = Some(AddDeviceFields {
            name: "x".into(),
            device_type: "phone".into(),
            port: 42187,
        });
        let ev = Processor::new()
            .handle(inbound(&a, peer_ip()), &ctx, clock.now())
            .unwrap();
        assert!(matches!(ev, DiscoveryEvent::Verified { .. }));
    }
}

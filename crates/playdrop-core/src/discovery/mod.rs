//! Discovery: private mDNS adverts, multicast beacon, announce-key tags, rate limits.
//! Implements `docs/protocol.md` section 8, SR-1 to SR-5.
//!
//! The beacon sender, beacon receiver and mDNS run as concurrent tasks under one
//! [`Discovery`] handle (protocol.md 8: concurrent paths, never sequential). Discovery never
//! dials and never answers a beacon. It only emits [`DiscoveryEvent`]s:
//!
//! - [`DiscoveryEvent::Verified`] when an advert's tag verifies against a pinned peer's
//!   announce key (SR-1, SR-5). The address is the packet source IP and the peer's stored
//!   port. The host still applies [`DialLimiter`] before dialling.
//! - [`DiscoveryEvent::Unpaired`] for named "Add device" adverts while the local window is
//!   open. These are listed for the user, never dialled automatically (protocol.md 8.3 rule 1).
//!
//! Module map: [`tag`](mod@tag) (8.2), [`advert`] (8.1), [`limits`] and [`cache`] (8.3 rule 3),
//! [`engine`] (the socket-free decision logic shared by both carriers), [`beacon`] and
//! [`mdns`] (the carriers), [`service`] (the facade).

pub mod advert;
pub mod beacon;
pub mod cache;
pub mod engine;
pub mod limits;
pub mod mdns;
pub mod service;
pub mod tag;

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use tokio::time::Instant;

pub use advert::{sanitize_name, AddDeviceFields, Advert};
pub use beacon::BeaconSchedule;
pub use limits::{DialLimiter, RateLimiter};
pub use service::{Discovery, DiscoveryConfig};
pub use tag::{tag, verify, DeviceIdString, PeerKeys};

/// Which carrier delivered an advert (protocol.md 8.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Via {
    /// UDP multicast beacon on `239.255.42.187:42187`.
    Beacon,
    /// mDNS service `_playdrop._tcp.local.`.
    Mdns,
}

/// What discovery reports to the host (protocol.md 8.3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DiscoveryEvent {
    /// A pinned peer is reachable (protocol.md 8.3 rule 1, SR-1). `addr` is the advert's
    /// source IP with the peer's stored port, never an address taken from a payload.
    Verified {
        /// The pinned peer whose announce key produced the tag.
        device_id: DeviceIdString,
        /// Dial candidate: source IP plus stored port.
        addr: SocketAddr,
        /// When the advert arrived.
        seen_at: Instant,
        /// Which carrier delivered it.
        via: Via,
    },
    /// An unpaired device in its own "Add device" window, reported only while the local
    /// window is open (protocol.md 6.1, 8.1, 8.3 rules 1 and 4). Display only.
    Unpaired {
        /// Sanitised name; never feeds a trust decision.
        name: String,
        /// Device type token.
        device_type: String,
        /// Source IP plus the advertised port.
        addr: SocketAddr,
        /// When the advert arrived.
        seen_at: Instant,
        /// Which carrier delivered it.
        via: Via,
    },
}

/// One up, non-loopback IPv4 interface address (protocol.md 8.1 "every up, non-loopback
/// IPv4 interface").
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Ipv4Interface {
    /// OS interface index, used for multicast membership.
    pub index: u32,
    /// The interface's own address.
    pub addr: Ipv4Addr,
    /// The subnet mask, used for the on-link check (SR-3).
    pub netmask: Ipv4Addr,
}

impl Ipv4Interface {
    /// `true` if `ip` is in this interface's subnet.
    pub fn contains(&self, ip: Ipv4Addr) -> bool {
        let mask = u32::from(self.netmask);
        u32::from(ip) & mask == u32::from(self.addr) & mask
    }

    /// The subnet's directed broadcast address.
    pub fn broadcast(&self) -> Ipv4Addr {
        Ipv4Addr::from(u32::from(self.addr) | !u32::from(self.netmask))
    }
}

/// Lists the host's up IPv4 interfaces (protocol.md 8.1, SR-3).
///
/// The core cannot enumerate interfaces without a platform crate (no `unsafe` here), so the
/// host supplies this. On desktop it wraps `if-addrs`; on Android and iOS the platform's
/// network callbacks feed it. Loopback entries are ignored by discovery either way.
pub trait InterfaceSource: Send + Sync {
    /// Current up IPv4 interfaces.
    fn ipv4_interfaces(&self) -> Vec<Ipv4Interface>;
}

/// A fixed interface list, for tests and for hosts that push changes themselves.
#[derive(Clone, Debug, Default)]
pub struct StaticInterfaces(pub std::sync::Arc<std::sync::RwLock<Vec<Ipv4Interface>>>);

impl StaticInterfaces {
    /// A source returning `list`.
    pub fn new(list: Vec<Ipv4Interface>) -> Self {
        Self(std::sync::Arc::new(std::sync::RwLock::new(list)))
    }

    /// Replaces the list. Call `Discovery::network_changed` afterwards, or wait for the
    /// periodic interface check to notice.
    pub fn set(&self, list: Vec<Ipv4Interface>) {
        *self
            .0
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = list;
    }
}

impl InterfaceSource for StaticInterfaces {
    fn ipv4_interfaces(&self) -> Vec<Ipv4Interface> {
        self.0
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

/// Keeps only usable interfaces: up is the source's job; loopback, unspecified and
/// multicast addresses are removed here (protocol.md 8.1).
pub(crate) fn usable_interfaces(list: Vec<Ipv4Interface>) -> Vec<Ipv4Interface> {
    let mut list: Vec<_> = list
        .into_iter()
        .filter(|i| !i.addr.is_loopback() && !i.addr.is_unspecified() && !i.addr.is_multicast())
        .collect();
    list.sort_by_key(|i| (i.index, u32::from(i.addr)));
    list.dedup();
    list
}

/// Is `ip` an acceptable advert source, and so an acceptable dial target
/// (protocol.md 8.3 rules 1 and 2, SR-1, SR-3)?
///
/// - IPv4: unicast, not loopback, and inside the subnet of one of `interfaces` without being
///   that subnet's broadcast or network address (on-link). With no known interface nothing
///   is on-link, so nothing passes.
/// - IPv6: only unicast link-local (`fe80::/10`) with a non-zero scope, which is on-link by
///   definition. v1 beacons are IPv4 only; IPv6 reaches here through mDNS.
pub(crate) fn acceptable_source(ip: IpAddr, scope_id: u32, interfaces: &[Ipv4Interface]) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            if v4.is_unspecified() || v4.is_loopback() || v4.is_multicast() || v4.is_broadcast() {
                return false;
            }
            interfaces.iter().any(|i| {
                let network = Ipv4Addr::from(u32::from(i.addr) & u32::from(i.netmask));
                i.contains(v4)
                    && (i.netmask == Ipv4Addr::BROADCAST || (v4 != i.broadcast() && v4 != network))
            })
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return acceptable_source(IpAddr::V4(v4), 0, interfaces);
            }
            (v6.segments()[0] & 0xffc0) == 0xfe80 && scope_id != 0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv6Addr;

    fn lan() -> Vec<Ipv4Interface> {
        vec![Ipv4Interface {
            index: 2,
            addr: Ipv4Addr::new(192, 168, 1, 10),
            netmask: Ipv4Addr::new(255, 255, 255, 0),
        }]
    }

    #[test]
    fn source_rules() {
        let l = lan();
        let ok = |ip: Ipv4Addr| acceptable_source(IpAddr::V4(ip), 0, &l);
        assert!(ok(Ipv4Addr::new(192, 168, 1, 20)));
        assert!(!ok(Ipv4Addr::new(192, 168, 2, 20)), "off-link");
        assert!(!ok(Ipv4Addr::new(192, 168, 1, 255)), "subnet broadcast");
        assert!(!ok(Ipv4Addr::new(192, 168, 1, 0)), "network address");
        assert!(!ok(Ipv4Addr::LOCALHOST));
        assert!(!ok(Ipv4Addr::BROADCAST));
        assert!(!ok(Ipv4Addr::UNSPECIFIED));
        assert!(!ok(Ipv4Addr::new(239, 255, 42, 187)));
        assert!(!acceptable_source(
            IpAddr::V4(Ipv4Addr::new(192, 168, 1, 20)),
            0,
            &[]
        ));
        let ll: Ipv6Addr = "fe80::1".parse().unwrap();
        assert!(acceptable_source(IpAddr::V6(ll), 3, &l));
        assert!(!acceptable_source(IpAddr::V6(ll), 0, &l));
        assert!(!acceptable_source(IpAddr::V6(Ipv6Addr::LOCALHOST), 1, &l));
        assert!(!acceptable_source(
            IpAddr::V6("2001:db8::1".parse().unwrap()),
            1,
            &l
        ));
        assert!(acceptable_source(
            IpAddr::V6(Ipv4Addr::new(192, 168, 1, 20).to_ipv6_mapped()),
            0,
            &l
        ));
    }

    #[test]
    fn usable_interfaces_drops_loopback_and_duplicates() {
        let mut l = lan();
        l.push(l[0].clone());
        l.push(Ipv4Interface {
            index: 1,
            addr: Ipv4Addr::LOCALHOST,
            netmask: Ipv4Addr::new(255, 0, 0, 0),
        });
        assert_eq!(usable_interfaces(l), lan());
    }
}

/// A manual clock for tests: limiters and caches take `now` as an argument, so tests step
/// time explicitly instead of sleeping.
#[cfg(test)]
pub(crate) mod test_clock {
    use std::cell::Cell;
    use std::time::Duration;

    use tokio::time::Instant;

    /// Starts at the real current instant and only moves when told to.
    pub(crate) struct TestClock(Cell<Instant>);

    impl Default for TestClock {
        fn default() -> Self {
            Self(Cell::new(Instant::now()))
        }
    }

    impl TestClock {
        pub(crate) fn now(&self) -> Instant {
            self.0.get()
        }

        pub(crate) fn advance(&self, d: Duration) {
            self.0.set(self.0.get() + d);
        }
    }
}

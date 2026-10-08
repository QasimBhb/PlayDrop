//! Multicast beacon on `239.255.42.187:42187`, `docs/protocol.md` sections 8.1 and 8.3
//! (SR-1 to SR-4).
//!
//! - Sender: one datagram per up, non-loopback IPv4 interface, `IP_MULTICAST_TTL` 1, a fresh
//!   nonce per datagram, at 0, 1 and 3 s after start or a network change and then every 60 s
//!   on desktop. Mobile hosts send only bursts they ask for.
//! - Receiver: joins the group on every interface, applies the rate limits before parsing,
//!   drops oversized and own datagrams, and never sends anything back (SR-3).

use std::collections::HashSet;
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::Arc;
use std::time::Duration;

use socket2::{Domain, Protocol, Socket, Type};
use tokio::net::UdpSocket;
use tokio::time::Instant;

use super::advert::{Advert, MAX_BEACON_LEN};
use super::engine::Inbound;
use super::limits::RateLimiter;
use super::service::{stopped, Shared};
use super::{DiscoveryEvent, Ipv4Interface, Via};

/// When the beacon is sent (protocol.md 8.1).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BeaconSchedule {
    /// 0, 1 and 3 s after start or a network change, then every 60 s.
    #[default]
    Desktop,
    /// 0, 1 and 3 s after start, a network change or `Discovery::announce_now`, then
    /// silence. Mobile hosts send only during discovery windows.
    DiscoveryWindowsOnly,
}

/// Burst offsets after start or a network change (protocol.md 8.1).
const BURST: [Duration; 3] = [
    Duration::ZERO,
    Duration::from_secs(1),
    Duration::from_secs(3),
];

/// Desktop steady-state interval (protocol.md 8.1).
pub const STEADY_INTERVAL: Duration = Duration::from_secs(60);

impl BeaconSchedule {
    /// Offset from the burst start of send number `step` (0-based), or `None` when the
    /// schedule has nothing more to send until the next burst.
    pub fn offset(self, step: u32) -> Option<Duration> {
        if let Some(d) = usize::try_from(step).ok().and_then(|i| BURST.get(i)) {
            return Some(*d);
        }
        match self {
            Self::Desktop => Some(BURST[2] + STEADY_INTERVAL * (step - 2)),
            Self::DiscoveryWindowsOnly => None,
        }
    }
}

/// Decides what to do with one received datagram, without touching a socket
/// (protocol.md 8.3 rules 2, 3 and 5, SR-2):
///
/// 1. Rate limit by source IP and overall; excess is dropped unparsed.
/// 2. Datagrams over 512 bytes are dropped unparsed.
/// 3. Malformed JSON is dropped silently.
/// 4. The parsed advert goes to `handle` (the shared processor), which drops own adverts and
///    verifies tags.
///
/// Nothing is ever sent in reply.
pub fn receive_datagram<F>(
    limiter: &mut RateLimiter,
    data: &[u8],
    src: IpAddr,
    now: Instant,
    handle: F,
) -> Option<DiscoveryEvent>
where
    F: FnOnce(Inbound<'_>) -> Option<DiscoveryEvent>,
{
    if !limiter.allow(src, now) {
        return None;
    }
    if data.len() > MAX_BEACON_LEN {
        return None;
    }
    let advert = Advert::from_beacon(data)?;
    handle(Inbound {
        advert: &advert,
        src,
        scope_id: 0,
        via: Via::Beacon,
    })
}

/// The beacon receive socket (protocol.md 8.1).
pub struct BeaconReceiver {
    socket: UdpSocket,
    group: Ipv4Addr,
    joined: HashSet<Ipv4Addr>,
}

impl BeaconReceiver {
    /// Binds the group port with `SO_REUSEADDR` and, where available, `SO_REUSEPORT`, so a
    /// second instance on the same host can listen too.
    ///
    /// On Unix the socket binds the group address itself, so only datagrams sent to the
    /// group arrive, not unicast to the port. Windows cannot bind a multicast address and
    /// binds the wildcard instead.
    pub fn bind(group: SocketAddrV4) -> io::Result<Self> {
        let s = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
        s.set_reuse_address(true)?;
        #[cfg(all(
            unix,
            not(any(target_os = "solaris", target_os = "illumos", target_os = "cygwin"))
        ))]
        s.set_reuse_port(true)?;
        let bind_ip = if cfg!(unix) {
            *group.ip()
        } else {
            Ipv4Addr::UNSPECIFIED
        };
        s.bind(&SocketAddr::from((bind_ip, group.port())).into())?;
        s.set_nonblocking(true)?;
        let socket = UdpSocket::from_std(s.into())?;
        Ok(Self {
            socket,
            group: *group.ip(),
            joined: HashSet::new(),
        })
    }

    /// Joins the group on every interface in `interfaces` and leaves it on any no longer
    /// listed. Failures are logged and skipped so one bad interface does not stop the rest.
    pub fn sync_membership(&mut self, interfaces: &[Ipv4Interface]) {
        let want: HashSet<Ipv4Addr> = interfaces.iter().map(|i| i.addr).collect();
        for gone in self.joined.difference(&want).copied().collect::<Vec<_>>() {
            // The interface may already be gone, in which case the kernel dropped it too.
            let _ = self.socket.leave_multicast_v4(self.group, gone);
            self.joined.remove(&gone);
        }
        for addr in want {
            if self.joined.contains(&addr) {
                continue;
            }
            match self.socket.join_multicast_v4(self.group, addr) {
                Ok(()) => {
                    self.joined.insert(addr);
                }
                Err(e) if e.kind() == io::ErrorKind::AddrInUse => {
                    self.joined.insert(addr);
                }
                Err(e) => tracing::debug!(%addr, error = %e, "beacon: join failed"),
            }
        }
    }
}

/// Per-interface send sockets (protocol.md 8.1).
struct BeaconSender {
    sockets: Vec<(Ipv4Addr, UdpSocket)>,
    dest: SocketAddr,
}

impl BeaconSender {
    fn new(interfaces: &[Ipv4Interface], group: SocketAddrV4) -> Self {
        let sockets = interfaces
            .iter()
            .filter_map(|i| match sender_socket(i.addr) {
                Ok(s) => Some((i.addr, s)),
                Err(e) => {
                    tracing::debug!(addr = %i.addr, error = %e, "beacon: send socket failed");
                    None
                }
            })
            .collect();
        Self {
            sockets,
            dest: SocketAddr::V4(group),
        }
    }

    /// Sends one datagram per interface, each with a fresh nonce recorded as our own first.
    async fn send_round(&self, shared: &Shared) {
        for (addr, socket) in &self.sockets {
            let nonce: [u8; 16] = rand::random();
            shared.own.record(nonce);
            let Some(wire) = shared.advert(nonce).to_beacon() else {
                continue;
            };
            if let Err(e) = socket.send_to(&wire, self.dest).await {
                tracing::debug!(%addr, error = %e, "beacon: send failed");
            }
        }
    }
}

/// A send socket bound to one interface address with `IP_MULTICAST_TTL` 1 (SR-3) and
/// multicast loop on, so other instances on this host hear it; this process drops
/// its own copies by nonce.
fn sender_socket(iface: Ipv4Addr) -> io::Result<UdpSocket> {
    let s = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    s.set_multicast_if_v4(&iface)?;
    s.set_multicast_ttl_v4(1)?;
    s.set_multicast_loop_v4(true)?;
    s.bind(&SocketAddr::from((iface, 0)).into())?;
    s.set_nonblocking(true)?;
    UdpSocket::from_std(s.into())
}

/// Sender task: follows [`BeaconSchedule`], rebuilding sockets and restarting the burst on
/// every refresh (protocol.md 8.1).
pub(crate) async fn run_sender(shared: Arc<Shared>) {
    let mut refresh = shared.refresh.subscribe();
    let mut burst = shared.burst.subscribe();
    let mut stop = shared.stop_requested();
    loop {
        refresh.borrow_and_update();
        let sender = BeaconSender::new(&shared.interfaces(), shared.group);
        let mut start = Instant::now();
        let mut sent = 0u32;
        loop {
            let deadline = shared.schedule.offset(sent).map(|d| start + d);
            tokio::select! {
                () = sleep_until_opt(deadline) => {
                    sender.send_round(&shared).await;
                    sent += 1;
                }
                r = refresh.changed() => {
                    if r.is_err() { return; }
                    break;
                }
                r = burst.changed() => {
                    if r.is_err() { return; }
                    start = Instant::now();
                    sent = 0;
                }
                () = stopped(&mut stop) => return,
            }
        }
    }
}

/// Receiver task: reads datagrams, keeps group membership in step with the interfaces,
/// and never replies (SR-3).
pub(crate) async fn run_receiver(shared: Arc<Shared>, mut rx: BeaconReceiver) {
    let mut refresh = shared.refresh.subscribe();
    let mut stop = shared.stop_requested();
    let mut limiter = RateLimiter::new();
    // One byte over the limit, so an oversized datagram is seen as such instead of being
    // silently truncated to a legal length.
    let mut buf = vec![0u8; MAX_BEACON_LEN + 1];
    rx.sync_membership(&shared.interfaces());
    loop {
        tokio::select! {
            r = rx.socket.recv_from(&mut buf) => match r {
                Ok((len, src)) => {
                    let now = Instant::now();
                    let ev = receive_datagram(&mut limiter, &buf[..len], src.ip(), now, |inb| shared.handle(inb, now));
                    if let Some(ev) = ev {
                        shared.emit(ev);
                    }
                }
                Err(e) => {
                    // Windows reports oversized datagrams and ICMP errors here; neither is
                    // fatal. Back off briefly so a persistent error cannot spin.
                    tracing::debug!(error = %e, "beacon: receive error");
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            },
            r = refresh.changed() => {
                if r.is_err() { return; }
                rx.sync_membership(&shared.interfaces());
            }
            () = stopped(&mut stop) => return,
        }
    }
}

async fn sleep_until_opt(deadline: Option<Instant>) {
    match deadline {
        Some(d) => tokio::time::sleep_until(d).await,
        None => std::future::pending().await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::engine::{Context, OwnNonces, Processor};
    use crate::discovery::tag::{tag, DeviceIdString, PeerKeys};
    use crate::discovery::test_clock::TestClock;

    struct OnePeer;
    impl PeerKeys for OnePeer {
        fn announce_keys(&self) -> Vec<(DeviceIdString, [u8; 32])> {
            vec![("p".repeat(64), [5; 32])]
        }
    }

    fn lan() -> Vec<Ipv4Interface> {
        vec![Ipv4Interface {
            index: 2,
            addr: Ipv4Addr::new(192, 168, 7, 1),
            netmask: Ipv4Addr::new(255, 255, 255, 0),
        }]
    }

    fn src(last: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(192, 168, 7, last))
    }

    fn genuine(n: u8) -> Vec<u8> {
        let n = [n; 16];
        Advert {
            v: 1,
            n,
            t: tag(&[5; 32], &n),
            add_device: None,
        }
        .to_beacon()
        .unwrap()
    }

    #[test]
    fn schedule_offsets() {
        let d = BeaconSchedule::Desktop;
        let secs: Vec<u64> = (0..6).map(|s| d.offset(s).unwrap().as_secs()).collect();
        assert_eq!(secs, [0, 1, 3, 63, 123, 183]);
        let m = BeaconSchedule::DiscoveryWindowsOnly;
        assert_eq!(m.offset(2), Some(Duration::from_secs(3)));
        assert_eq!(m.offset(3), None);
    }

    /// Datagrams fed straight into the receive path, no sockets (protocol.md 8.3).
    #[test]
    fn datagrams_through_parser_and_processor() {
        let clock = TestClock::default();
        let keys = OnePeer;
        let own = OwnNonces::default();
        let l = lan();
        let ctx = Context {
            keys: &keys,
            add_device_window: false,
            interfaces: &l,
            own: &own,
        };
        let mut p = Processor::new();
        let mut rl = RateLimiter::new();
        let mut run = |data: &[u8], from: IpAddr, rl: &mut RateLimiter| {
            let now = clock.now();
            receive_datagram(rl, data, from, now, |inb| p.handle(inb, &ctx, now))
        };

        let ev = run(&genuine(1), src(9), &mut rl).unwrap();
        assert!(
            matches!(ev, DiscoveryEvent::Verified { addr, via: Via::Beacon, .. }
            if addr == SocketAddr::from(([192, 168, 7, 9], crate::constants::TCP_PORT)))
        );

        assert!(run(b"garbage", src(10), &mut rl).is_none());
        assert!(run(&[b' '; MAX_BEACON_LEN + 1], src(10), &mut rl).is_none());

        // Own nonce is dropped.
        own.record([2; 16]);
        assert!(run(&genuine(2), src(11), &mut rl).is_none());

        // The sixth packet in a second from one source is dropped before parsing, even if
        // genuine.
        for i in 0..4 {
            assert!(run(&genuine(20 + i), src(12), &mut rl).is_some());
        }
        assert!(run(&genuine(30), src(12), &mut rl).is_some());
        assert!(run(&genuine(31), src(12), &mut rl).is_none());
        clock.advance(Duration::from_secs(1));
        assert!(run(&genuine(32), src(12), &mut rl).is_some());
    }

    #[test]
    fn global_rate_limit_drops_flood_unparsed() {
        let clock = TestClock::default();
        let mut rl = RateLimiter::new();
        let now = clock.now();
        // 1,000 genuine datagrams from 1,000 sources in one second: only 200 are parsed.
        let mut handled = 0;
        for i in 0..1000u32 {
            let from = IpAddr::V4(Ipv4Addr::from(0x0a00_0000 + i));
            receive_datagram(&mut rl, &genuine(1), from, now, |_| {
                handled += 1;
                None
            });
        }
        assert_eq!(handled, 200);
    }

    /// Two discovery instances on a real interface see each other's beacons.
    ///
    /// Needs a multicast-capable interface. Run with, for example:
    /// `PLAYDROP_TEST_IFACE=192.168.1.10/24 cargo test -p playdrop-core beacon_loopback -- --ignored`
    #[tokio::test]
    #[ignore = "needs a real multicast interface; set PLAYDROP_TEST_IFACE=addr/prefix"]
    async fn beacon_loopback() {
        struct Pins(Vec<(DeviceIdString, [u8; 32])>);
        impl PeerKeys for Pins {
            fn announce_keys(&self) -> Vec<(DeviceIdString, [u8; 32])> {
                self.0.clone()
            }
        }

        use crate::discovery::{Discovery, DiscoveryConfig, StaticInterfaces};
        use std::sync::atomic::AtomicBool;

        let spec =
            std::env::var("PLAYDROP_TEST_IFACE").expect("set PLAYDROP_TEST_IFACE=addr/prefix");
        let (addr, prefix) = spec.split_once('/').expect("addr/prefix");
        let addr: Ipv4Addr = addr.parse().unwrap();
        let prefix: u32 = prefix.parse().unwrap();
        let netmask = Ipv4Addr::from(u32::MAX.checked_shl(32 - prefix).unwrap_or(0));
        let ifaces = StaticInterfaces::new(vec![Ipv4Interface {
            index: 0,
            addr,
            netmask,
        }]);

        let group = SocketAddrV4::new(crate::constants::MULTICAST_GROUP, 42999);
        let mk = |key: [u8; 32], pins: Vec<(DeviceIdString, [u8; 32])>| {
            let mut c = DiscoveryConfig::new(key, 42188, Arc::new(ifaces.clone()));
            c.mdns = false;
            c.beacon_group = group;
            Discovery::start(c, Arc::new(Pins(pins)), Arc::new(AtomicBool::new(false))).unwrap()
        };
        let (a, _a_rx) = mk([1; 32], vec![]);
        let (b, mut b_rx) = mk([2; 32], vec![("a".repeat(64), [1; 32])]);
        let ev = tokio::time::timeout(Duration::from_secs(5), b_rx.recv())
            .await
            .expect("no beacon")
            .unwrap();
        assert!(
            matches!(ev, DiscoveryEvent::Verified { ref device_id, addr: got, .. }
            if *device_id == "a".repeat(64) && got.ip() == IpAddr::V4(addr))
        );
        a.stop().await;
        b.stop().await;
    }
}

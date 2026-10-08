//! Private mDNS adverts on `_playdrop._tcp.local.`, `docs/protocol.md` section 8.1
//! (SR-2 to SR-4), using the `mdns-sd` daemon.
//!
//! - Register: instance name and host name are 16 random lowercase hex characters each,
//!   TXT carries `v`, `n`, `t` (plus `name` and `type` in "Add device" mode), the SRV port
//!   is the listening port. Instance name, host name and nonce rotate every 15 minutes and
//!   on every refresh (network change, "Add device" edge, key rotation).
//! - Browse: resolved records go through the same rate limits, size cap and processor as
//!   beacons, once per resolved address.
//!
//! Gaps in `mdns-sd` 0.21 against protocol.md 8.1 and 8.3, and what this module does:
//!
//! - **Off-link drop (8.3 rule 2, SR-3).** The daemon does not expose a packet's source
//!   address and does not drop off-link responses itself. This module instead accepts only
//!   resolved addresses that are on-link (inside a local IPv4 subnet, or IPv6 link-local),
//!   which is the address that would be dialled.
//! - **Rate limit and size cap before parsing (8.3 rule 3, SR-2).** The daemon parses every
//!   packet and caches records before this module sees them. The limits here apply to
//!   resolved services, and the 400-byte TXT cap is checked on the decoded record. The
//!   daemon caps packets at 9000 bytes (RFC 6762) but its record cache is not bounded by us.
//! - **Source IP for dialling (8.3 rule 1).** With no packet source available, the dial
//!   address is the resolved A or AAAA address, filtered as above, with the peer's stored
//!   port. A forged response can therefore point a verified (replayed) tag at another
//!   on-link host; the TLS pin check fails and the dial limiter caps it at one per 5 s.
//! - **Never answer (8.3 rule 2)** applies to beacons. The daemon does answer mDNS queries
//!   for our own record, which is standard mDNS behaviour.

use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use mdns_sd::{ResolvedService, ScopedIp, ServiceDaemon, ServiceEvent, ServiceInfo};
use tokio::time::Instant;

use super::advert::{hex, Advert};
use super::engine::Inbound;
use super::limits::RateLimiter;
use super::service::{stopped, Shared};
use super::Via;
use crate::constants::MDNS_SERVICE;
use crate::{Error, Result};

/// Instance name and nonce lifetime (protocol.md 8.1, SR-4).
pub const ROTATE_EVERY: Duration = Duration::from_mins(15);

/// Creates the mDNS daemon. It runs on its own thread inside `mdns-sd`.
pub(crate) fn new_daemon() -> Result<ServiceDaemon> {
    ServiceDaemon::new().map_err(mdns_err)
}

fn mdns_err(e: impl std::fmt::Display) -> Error {
    Error::Io(std::io::Error::other(format!("mdns: {e}")))
}

/// 16 random lowercase hex characters (protocol.md 8.1, SR-4).
pub fn random_label() -> String {
    hex(&rand::random::<[u8; 8]>())
}

/// Our current registration.
struct Registration {
    fullname: String,
}

/// Registers a fresh record: new instance name, host name and nonce (SR-4).
fn register(daemon: &ServiceDaemon, shared: &Shared) -> Result<Registration> {
    let instance = random_label();
    let host = format!("{}.local.", random_label());
    let nonce: [u8; 16] = rand::random();
    let txt = shared.advert(nonce).to_txt();
    let info = ServiceInfo::new(
        MDNS_SERVICE,
        &instance,
        &host,
        "",
        shared.listen_port,
        &txt[..],
    )
    .map_err(mdns_err)?
    .enable_addr_auto();
    let fullname = info.get_fullname().to_owned();
    shared.own.set_mdns(Some(nonce));
    daemon.register(info).map_err(mdns_err)?;
    Ok(Registration { fullname })
}

fn unregister(daemon: &ServiceDaemon, reg: Option<Registration>) {
    if let Some(reg) = reg {
        // Sends goodbye packets; the status reply is not needed.
        if let Err(e) = daemon.unregister(&reg.fullname) {
            tracing::debug!(error = %e, "mdns: unregister failed");
        }
    }
}

fn reregister(daemon: &ServiceDaemon, shared: &Shared, current: &mut Option<Registration>) {
    unregister(daemon, current.take());
    match register(daemon, shared) {
        Ok(r) => *current = Some(r),
        Err(e) => {
            shared.own.set_mdns(None);
            tracing::warn!(error = %e, "mdns: register failed");
        }
    }
}

/// Parses a resolved record into an advert, then yields one processor input per resolved
/// address (protocol.md 8.1). Returns `None` if the TXT record is unusable.
pub fn resolved_advert(svc: &ResolvedService) -> Option<(Advert, Vec<(IpAddr, u32)>)> {
    let entries = svc.txt_properties.iter().map(|p| (p.key(), p.val()));
    let advert = Advert::from_txt(entries, svc.port)?;
    let addrs = svc
        .addresses
        .iter()
        .filter_map(|a| match a {
            ScopedIp::V4(v4) => Some((IpAddr::V4(*v4.addr()), 0)),
            ScopedIp::V6(v6) => Some((IpAddr::V6(*v6.addr()), v6.scope_id().index)),
            _ => None,
        })
        .collect();
    Some((advert, addrs))
}

fn on_resolved(
    shared: &Shared,
    limiter: &mut RateLimiter,
    svc: &ResolvedService,
    own: Option<&str>,
) {
    if own == Some(svc.fullname.as_str()) {
        return;
    }
    let Some((advert, addrs)) = resolved_advert(svc) else {
        return;
    };
    for (ip, scope_id) in addrs {
        let now = Instant::now();
        if !limiter.allow(ip, now) {
            continue;
        }
        if let Some(ev) = shared.handle(
            Inbound {
                advert: &advert,
                src: ip,
                scope_id,
                via: Via::Mdns,
            },
            now,
        ) {
            shared.emit(ev);
        }
    }
}

/// mDNS task: register, browse, rotate every 15 minutes and on refresh, withdraw on stop.
pub(crate) async fn run(shared: Arc<Shared>, daemon: ServiceDaemon) {
    let mut refresh = shared.refresh.subscribe();
    let mut stop = shared.stop_requested();
    let browse = match daemon.browse(MDNS_SERVICE) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(error = %e, "mdns: browse failed");
            let _ = daemon.shutdown();
            return;
        }
    };
    let mut current = None;
    refresh.borrow_and_update();
    reregister(&daemon, &shared, &mut current);
    let mut rotate = tokio::time::interval_at(Instant::now() + ROTATE_EVERY, ROTATE_EVERY);
    rotate.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut limiter = RateLimiter::new();

    loop {
        tokio::select! {
            ev = browse.recv_async() => match ev {
                Ok(ServiceEvent::ServiceResolved(svc)) => {
                    on_resolved(&shared, &mut limiter, &svc, current.as_ref().map(|r| r.fullname.as_str()));
                }
                Ok(_) => {}
                Err(_) => break,
            },
            _ = rotate.tick() => reregister(&daemon, &shared, &mut current),
            r = refresh.changed() => {
                if r.is_err() { break; }
                reregister(&daemon, &shared, &mut current);
                rotate.reset();
            }
            () = stopped(&mut stop) => break,
        }
    }

    unregister(&daemon, current.take());
    shared.own.set_mdns(None);
    let _ = daemon.stop_browse(MDNS_SERVICE);
    if let Ok(done) = daemon.shutdown() {
        // Give the daemon a moment to flush goodbye packets.
        let _ = tokio::time::timeout(Duration::from_secs(1), done.recv_async()).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::tag::{DeviceIdString, PeerKeys};
    use crate::discovery::{
        Discovery, DiscoveryConfig, DiscoveryEvent, Ipv4Interface, StaticInterfaces,
    };
    use std::net::Ipv4Addr;
    use std::sync::atomic::AtomicBool;

    #[test]
    fn labels_are_16_lowercase_hex_and_fresh() {
        let a = random_label();
        assert_eq!(a.len(), 16);
        assert!(a
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)));
        assert_ne!(a, random_label());
    }

    #[test]
    fn txt_fits_mdns_sd_service_info() {
        // The TXT we build must be accepted by mdns-sd (keys ASCII, entries under 255 bytes).
        let advert = Advert {
            v: 1,
            n: [1; 16],
            t: [2; 16],
            add_device: Some(crate::discovery::AddDeviceFields {
                name: "é".repeat(31),
                device_type: "desktop".into(),
                port: 42187,
            }),
        };
        let txt = advert.to_txt();
        let info = ServiceInfo::new(
            MDNS_SERVICE,
            &random_label(),
            "abc.local.",
            "",
            42187,
            &txt[..],
        )
        .unwrap();
        let props = info.get_properties();
        let parsed = Advert::from_txt(props.iter().map(|p| (p.key(), p.val())), 42187);
        assert_eq!(parsed, Some(advert));
    }

    /// Register on one daemon and browse from another on this host.
    ///
    /// Needs working multicast on a real interface (mdns-sd skips loopback). Run with, for
    /// example:
    /// `PLAYDROP_TEST_IFACE=192.168.1.10/24 cargo test -p playdrop-core mdns_register_and_browse -- --ignored`
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "needs a real multicast interface; set PLAYDROP_TEST_IFACE=addr/prefix"]
    async fn mdns_register_and_browse() {
        struct Pins(Vec<(DeviceIdString, [u8; 32])>);
        impl PeerKeys for Pins {
            fn announce_keys(&self) -> Vec<(DeviceIdString, [u8; 32])> {
                self.0.clone()
            }
            fn stored_port(&self, _: &str) -> u16 {
                42190
            }
        }

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

        let mk = |key: [u8; 32], pins| {
            let mut c = DiscoveryConfig::new(key, 42190, Arc::new(ifaces.clone()));
            c.beacon = false;
            Discovery::start(c, Arc::new(Pins(pins)), Arc::new(AtomicBool::new(false))).unwrap()
        };
        let (a, _a_rx) = mk([1; 32], vec![]);
        let (b, mut b_rx) = mk([2; 32], vec![("a".repeat(64), [1; 32])]);
        let ev = tokio::time::timeout(Duration::from_secs(10), b_rx.recv())
            .await
            .expect("no mDNS advert")
            .unwrap();
        assert!(
            matches!(ev, DiscoveryEvent::Verified { ref device_id, addr: got, via: Via::Mdns, .. }
            if *device_id == "a".repeat(64) && got.port() == 42190)
        );
        a.stop().await;
        b.stop().await;
    }
}

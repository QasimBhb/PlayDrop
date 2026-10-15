//! On-demand diagnostics (Phase 1 handoff gap 6; PLAN.md: tell the user which layer
//! failed). [`Host::diagnose`](super::Host::diagnose) gathers what the host knows about its
//! own socket, interfaces, both discovery carriers and every paired peer.
//!
//! What the core cannot see: a firewall dropping inbound TCP on the port looks exactly
//! like "nobody dialled". The daemon tells the two apart with its own probe, using the
//! fields here: connect to `127.0.0.1:port` (proves the listener), then to each
//! `interfaces[].addr:port` from a LAN-facing socket or ask a peer to dial back. Loopback
//! fine but LAN refused or silent, with `bind_ok` true, means a host firewall. Likewise
//! "multicast blocked" reads as: `multicast.joined` non-empty but
//! `multicast.beacon_recently` false while a peer is known to be on the same subnet.

use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::events::DiscoveryPath;
use super::{lock, Inner};
use crate::net::connection::short;

/// How recent a received beacon must be to count as "multicast works".
pub const BEACON_RECENT: Duration = Duration::from_secs(60);

/// Everything [`Host::diagnose`](super::Host::diagnose) reports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostics {
    /// The bound address.
    pub addr: SocketAddr,
    /// This device's ID, truncated to its first 8 hex characters like every log line
    /// (SR-32: the report is meant to be shared in bug reports).
    pub device_id: String,
    /// This device's Short ID (SR-8).
    pub short_id: String,
    /// The TCP port (protocol.md section 1).
    pub port: u16,
    /// The listening socket was bound and the server is still running.
    pub bind_ok: bool,
    /// Up, non-loopback IPv4 interfaces discovery uses (protocol.md 8.1).
    pub interfaces: Vec<InterfaceInfo>,
    /// The multicast beacon carrier.
    pub multicast: MulticastStatus,
    /// The mDNS carrier.
    pub mdns: MdnsStatus,
    /// Every paired peer.
    pub peers: Vec<PeerDiagnostic>,
}

/// One IPv4 interface.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InterfaceInfo {
    /// OS interface index.
    pub index: u32,
    /// The interface address.
    pub addr: Ipv4Addr,
    /// Its subnet mask.
    pub netmask: Ipv4Addr,
}

/// Beacon carrier health (protocol.md 8.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MulticastStatus {
    /// Discovery and the beacon are turned on.
    pub enabled: bool,
    /// Interface addresses where joining `239.255.42.187` succeeded.
    pub joined: Vec<Ipv4Addr>,
    /// Interface addresses where the join failed, with the OS error.
    pub join_failed: Vec<JoinFailure>,
    /// A beacon from another device arrived within the last 60 s.
    pub beacon_recently: bool,
    /// Seconds since the last beacon from another device; `None` if none since start.
    pub last_beacon_secs_ago: Option<u64>,
}

/// A failed multicast group join.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinFailure {
    /// Interface address.
    pub addr: Ipv4Addr,
    /// The OS error.
    pub error: String,
}

/// mDNS carrier health (protocol.md 8.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MdnsStatus {
    /// Discovery and mDNS are turned on.
    pub enabled: bool,
    /// This host's `_playdrop._tcp` record is registered.
    pub registered: bool,
    /// Why registration or browsing failed, if it did.
    pub error: Option<String>,
    /// An mDNS advert from another device was seen since start.
    pub peer_seen: bool,
    /// Seconds since the last mDNS advert from another device.
    pub last_seen_secs_ago: Option<u64>,
}

/// One paired peer's reachability.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerDiagnostic {
    /// The peer's Device ID, truncated to its first 8 hex characters (SR-32).
    pub peer: String,
    /// Short ID.
    pub short_id: String,
    /// Local label.
    pub label: String,
    /// A session is up.
    pub online: bool,
    /// Last-known addresses, newest first.
    pub last_known: Vec<SocketAddr>,
    /// A verified mDNS advert from this peer was seen since start.
    pub mdns_seen: bool,
    /// A verified beacon from this peer was seen since start.
    pub beacon_seen: bool,
    /// The most recent failed dial to this peer, kept until the next failure.
    pub last_result: Option<DialFailure>,
}

/// A failed dial and which discovery path supplied the addresses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DialFailure {
    /// `last_known` when only stored addresses were dialled, otherwise the carrier that
    /// most recently supplied an address (`mdns` or `beacon`).
    pub path: DiscoveryPath,
    /// What went wrong.
    pub failure: String,
    /// When, Unix seconds.
    pub at: u64,
}

/// Builds the report.
pub(crate) fn build(inner: &Inner) -> Diagnostics {
    let bind_ok = lock(&inner.server_task)
        .as_ref()
        .is_some_and(|t| !t.is_finished());
    let (status, interfaces) = match lock(&inner.discovery).as_ref() {
        Some(d) => (Some(d.status()), d.interfaces()),
        None => (
            None,
            crate::discovery::usable_interfaces(inner.interfaces.ipv4_interfaces()),
        ),
    };
    let ago = |t: Option<tokio::time::Instant>| t.map(|t| t.elapsed().as_secs());
    let multicast = MulticastStatus {
        enabled: status.as_ref().is_some_and(|s| s.beacon_enabled),
        joined: status
            .as_ref()
            .map(|s| s.beacon_joined.clone())
            .unwrap_or_default(),
        join_failed: status
            .as_ref()
            .map(|s| {
                s.beacon_join_failed
                    .iter()
                    .map(|(addr, error)| JoinFailure {
                        addr: *addr,
                        error: error.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default(),
        beacon_recently: status
            .as_ref()
            .and_then(|s| s.last_beacon)
            .is_some_and(|t| t.elapsed() <= BEACON_RECENT),
        last_beacon_secs_ago: ago(status.as_ref().and_then(|s| s.last_beacon)),
    };
    let mdns = MdnsStatus {
        enabled: status.as_ref().is_some_and(|s| s.mdns_enabled),
        registered: status.as_ref().is_some_and(|s| s.mdns_registered),
        error: status.as_ref().and_then(|s| s.mdns_error.clone()),
        peer_seen: status.as_ref().is_some_and(|s| s.last_mdns.is_some()),
        last_seen_secs_ago: ago(status.as_ref().and_then(|s| s.last_mdns)),
    };
    let reach = lock(&inner.reach).clone();
    let peers = inner
        .pins
        .read()
        .iter()
        .map(|p| {
            let r = reach.get(&p.device_id);
            PeerDiagnostic {
                peer: short(&p.device_id),
                short_id: p.device_id.short_id(),
                label: p.label.clone(),
                online: inner.registry.session(&p.device_id).is_some(),
                last_known: p.last_known.clone(),
                mdns_seen: r.is_some_and(|r| r.mdns),
                beacon_seen: r.is_some_and(|r| r.beacon),
                last_result: r.and_then(|r| r.last_failure.clone()),
            }
        })
        .collect();
    Diagnostics {
        addr: inner.local_addr,
        device_id: short(&inner.local_id),
        short_id: inner.local_id.short_id(),
        port: inner.port(),
        bind_ok,
        interfaces: interfaces
            .into_iter()
            .map(|i| InterfaceInfo {
                index: i.index,
                addr: i.addr,
                netmask: i.netmask,
            })
            .collect(),
        multicast,
        mdns,
        peers,
    }
}

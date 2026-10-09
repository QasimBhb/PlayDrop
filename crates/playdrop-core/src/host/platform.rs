//! Platform adapters the core needs from the host: free space for the SR-20 reserve and the
//! IPv4 interface list for discovery (protocol.md 8.1, SR-3).

use std::io;
use std::net::{IpAddr, Ipv4Addr};
use std::path::Path;

use crate::discovery::{InterfaceSource, Ipv4Interface};
use crate::transfer::{FreeSpace, SpaceInfo};

/// Free space of the volume holding a path (SR-20), from `statvfs` on Unix.
#[derive(Debug, Clone, Copy, Default)]
pub struct VolumeSpace;

impl FreeSpace for VolumeSpace {
    fn space(&self, path: &Path) -> io::Result<SpaceInfo> {
        // The receive root may not exist yet; ask about its nearest existing ancestor.
        let mut probe = path;
        while !probe.exists() {
            match probe.parent() {
                Some(p) if !p.as_os_str().is_empty() => probe = p,
                _ => break,
            }
        }
        statvfs_space(probe)
    }
}

#[cfg(unix)]
fn statvfs_space(path: &Path) -> io::Result<SpaceInfo> {
    let st = rustix::fs::statvfs(path).map_err(io::Error::from)?;
    let frag = st.f_frsize.max(1);
    Ok(SpaceInfo {
        available: st.f_bavail.saturating_mul(frag),
        total: st.f_blocks.saturating_mul(frag),
    })
}

#[cfg(not(unix))]
#[allow(
    clippy::unnecessary_wraps,
    reason = "same signature as the Unix version"
)]
fn statvfs_space(_path: &Path) -> io::Result<SpaceInfo> {
    // TODO(SR-20): GetDiskFreeSpaceExW on Windows (Phase 2). Until then report a generous
    // fixed volume so offers are not refused; writes still fail cleanly on a full disk.
    Ok(SpaceInfo {
        available: 1 << 50,
        total: 1 << 50,
    })
}

/// The host's up, non-loopback IPv4 interfaces from `if-addrs` (protocol.md 8.1, SR-3).
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemInterfaces;

impl InterfaceSource for SystemInterfaces {
    fn ipv4_interfaces(&self) -> Vec<Ipv4Interface> {
        let list = match if_addrs::get_if_addrs() {
            Ok(l) => l,
            Err(e) => {
                tracing::debug!(error = %e, "interface list unavailable");
                return Vec::new();
            }
        };
        list.into_iter()
            .filter(|i| i.is_oper_up() && !i.is_loopback())
            .filter_map(|i| match i.addr {
                if_addrs::IfAddr::V4(v4) => Some(Ipv4Interface {
                    index: i.index.unwrap_or(0),
                    addr: v4.ip,
                    netmask: v4.netmask,
                }),
                if_addrs::IfAddr::V6(_) => None,
            })
            .collect()
    }
}

/// This host's own unicast addresses, for the QR payload (protocol.md 6.3): every up,
/// non-loopback IPv4 address, or loopback when nothing else exists (single-machine tests).
pub fn local_addresses(source: &dyn InterfaceSource) -> Vec<IpAddr> {
    let mut out: Vec<IpAddr> = source
        .ipv4_interfaces()
        .into_iter()
        .map(|i| IpAddr::V4(i.addr))
        .filter(|ip| !ip.is_loopback() && !ip.is_unspecified())
        .collect();
    if out.is_empty() {
        out.push(IpAddr::V4(Ipv4Addr::LOCALHOST));
    }
    out
}

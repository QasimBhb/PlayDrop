import Darwin
import Foundation
import PlayDropCore

// The iPhone's IPv4 interfaces for discovery (docs/core-api.md section 7), read with
// `getifaddrs`. The core binds its multicast beacon and mDNS responder to each address and
// uses the netmask to judge "same network" in diagnostics.
//
// Kept: interfaces that are up and running, not loopback, with an AF_INET address. Skipped
// by name, because no LAN peer can be reached through them and joining multicast there only
// produces noise in diagnostics:
// - `pdp_ip*`: cellular data.
// - `utun*`, `ipsec*`: VPN and system tunnels (iCloud Private Relay, Wi-Fi calling).
// - `awdl*`, `llw*`: Apple Wireless Direct Link and its low-latency companion (AirDrop),
//   link-local IPv6 only in practice and never a path to a PlayDrop peer.
// Personal Hotspot (`bridge*`) and Wi-Fi or Ethernet (`en*`) stay in.

/// `NetworkInterfaces` backed by `getifaddrs`.
final class SystemInterfaces: NetworkInterfaces {
    /// Interface name prefixes that never carry LAN peers; see the file comment.
    static let skippedPrefixes = ["utun", "pdp_ip", "ipsec", "llw", "awdl"]

    /// A reader with no state; `getifaddrs` is called afresh on every request.
    init() {}

    /// Every usable IPv4 address with its interface index and dotted netmask. A failing
    /// `getifaddrs` is logged and yields an empty list, which diagnostics reports as "no
    /// network interface" rather than failing the core.
    func ipv4Interfaces() throws -> [Ipv4Interface] {
        var head: UnsafeMutablePointer<ifaddrs>?
        guard getifaddrs(&head) == 0 else {
            let code = errno
            Log.net.error("getifaddrs failed: errno \(code, privacy: .public)")
            return []
        }
        defer { freeifaddrs(head) }

        var out: [Ipv4Interface] = []
        var cursor = head
        while let entry = cursor {
            cursor = entry.pointee.ifa_next
            if let iface = Self.usable(entry.pointee) {
                out.append(iface)
            }
        }
        return out
    }

    /// The interface described by `ifa`, or nil when it is down, loopback, not IPv4 or of a
    /// skipped kind.
    private static func usable(_ ifa: ifaddrs) -> Ipv4Interface? {
        let flags = UInt32(ifa.ifa_flags)
        guard flags & UInt32(IFF_UP) != 0, flags & UInt32(IFF_RUNNING) != 0, flags & UInt32(IFF_LOOPBACK) == 0 else {
            return nil
        }
        guard let addrPtr = ifa.ifa_addr, addrPtr.pointee.sa_family == sa_family_t(AF_INET) else {
            return nil
        }
        let name = String(cString: ifa.ifa_name)
        if skippedPrefixes.contains(where: { name.hasPrefix($0) }) {
            return nil
        }
        let index = if_nametoindex(ifa.ifa_name)
        guard index > 0, let addr = dotted(addrPtr) else {
            return nil
        }
        // A missing netmask is treated as a host route; the core then sees a /32.
        let netmask = ifa.ifa_netmask.flatMap(dotted) ?? "255.255.255.255"
        return Ipv4Interface(index: index, addr: addr, netmask: netmask)
    }

    /// Dotted-quad text of an AF_INET socket address.
    private static func dotted(_ sa: UnsafeMutablePointer<sockaddr>) -> String? {
        guard sa.pointee.sa_family == sa_family_t(AF_INET) else {
            return nil
        }
        var sin = sa.withMemoryRebound(to: sockaddr_in.self, capacity: 1) { $0.pointee }
        var buffer = [CChar](repeating: 0, count: Int(INET_ADDRSTRLEN))
        guard inet_ntop(AF_INET, &sin.sin_addr, &buffer, socklen_t(INET_ADDRSTRLEN)) != nil else {
            return nil
        }
        let bytes = buffer.prefix { $0 != 0 }.map { UInt8(bitPattern: $0) }
        return String(decoding: bytes, as: UTF8.self)
    }
}

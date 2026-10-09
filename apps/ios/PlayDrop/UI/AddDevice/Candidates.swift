import Foundation
import PlayDropCore
import PlayDropShared

// Collapses the core's "Add device" candidates, which have one entry per address, into one
// row per device. Before pairing the announced name plus type is the only identity there is,
// so rows group by both; the row lists every address and pairs with the one most likely to
// work (private IPv4 first, then other IPv4, then IPv6), like the desktop's
// `merge_candidate` and Android `Candidates.collapse`. Plain logic for unit tests.

/// One device seen in its "Add device" window.
struct CandidateRow: Identifiable, Equatable, Sendable {
    /// Name plus type, stable while the window is open.
    var id: String { "\(deviceType)\u{1F}\(name)" }
    /// Announced name (display only, SR-8).
    let name: String
    /// Announced type token.
    let deviceType: String
    /// Every address seen, best first.
    let addrs: [String]

    /// The address to pair with.
    var bestAddr: String { addrs[0] }
}

/// See the file comment.
enum Candidates {
    /// One row per name and type, in first-seen order, addresses best first.
    static func collapse(_ list: [AddDeviceCandidate]) -> [CandidateRow] {
        var order: [String] = []
        var byKey: [String: (name: String, type: String, addrs: [String])] = [:]
        for c in list {
            let key = "\(c.deviceType)\u{1F}\(c.name)"
            if var entry = byKey[key] {
                if !entry.addrs.contains(c.addr) { entry.addrs.append(c.addr) }
                byKey[key] = entry
            } else {
                order.append(key)
                byKey[key] = (c.name, c.deviceType, [c.addr])
            }
        }
        return order.compactMap { key in
            guard let e = byKey[key] else { return nil }
            let sorted = e.addrs.enumerated()
                .sorted { a, b in
                    let ra = rank(a.element), rb = rank(b.element)
                    return ra != rb ? ra < rb : a.offset < b.offset
                }
                .map(\.element)
            return CandidateRow(name: e.name, deviceType: e.type, addrs: sorted)
        }
    }

    /// Lower is better: 0 private IPv4, 1 other IPv4, 2 IPv6 or unparsable.
    static func rank(_ addrPort: String) -> Int {
        guard let host = Addresses.host(of: addrPort) else { return 2 }
        let parts = host.split(separator: ".").compactMap { UInt8($0) }
        guard parts.count == 4 else { return 2 }
        switch (parts[0], parts[1]) {
        case (10, _), (192, 168): return 0
        case (172, 16...31): return 0
        default: return 1
        }
    }
}

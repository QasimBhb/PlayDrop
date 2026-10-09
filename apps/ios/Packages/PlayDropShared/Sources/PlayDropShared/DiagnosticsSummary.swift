// Turns a diagnostics snapshot into plain-language findings, most important first, naming
// the layer that failed, like the desktop daemon's `probe::summary`. Ported from the Android
// app's `ui/diagnostics/DiagnosticsSummary.kt`; the screen words each finding. A phone
// cannot probe its own port from the LAN, so the port line comes only from the listener.

/// One finding about why devices may not see each other.
public enum DiagIssue: Equatable, Sendable {
    /// The listener is down: the port is held or the bind failed.
    case notListening(port: UInt16)
    /// No usable network interface (Wi-Fi off).
    case noInterface
    /// Joining the multicast group failed on an interface.
    case multicastJoinFailed(addr: String, error: String)
    /// No beacon heard although a peer is connected on that network: multicast is filtered.
    case multicastBlocked(ifaceAddr: String, peerLabel: String)
    /// Both discovery paths are off.
    case discoveryOff
    /// mDNS registration or browsing failed.
    case mdnsFailed(error: String)
    /// An offline peer was last seen on a network this phone is not on.
    case otherNetwork(peerLabel: String)
    /// An offline peer whose last dial failed: the discovery path and the reason.
    case peerNotReached(peerLabel: String, path: String, failure: String)
    /// Nothing is wrong.
    case allFine
}

/// Builds the findings list from a snapshot.
public enum DiagnosticsSummary {
    /// The findings in priority order; exactly `[.allFine]` when nothing is wrong.
    public static func of(_ d: DiagSnapshot) -> [DiagIssue] {
        var out: [DiagIssue] = []
        if !d.bindOk { out.append(.notListening(port: d.port)) }
        if d.interfaces.isEmpty { out.append(.noInterface) }

        if d.multicastEnabled {
            out.append(contentsOf: d.multicastJoinFailed.map(splitJoinFailure))
            if !d.beaconRecently {
                for iface in d.interfaces {
                    let silent = d.peers.first { p in
                        p.online && !p.beaconSeen &&
                            p.lastKnown.contains { Addresses.sameSubnet($0, iface.addr, iface.netmask) }
                    }
                    if let silent {
                        out.append(.multicastBlocked(ifaceAddr: iface.addr, peerLabel: silent.label))
                    }
                }
            }
        } else if !d.mdnsEnabled {
            out.append(.discoveryOff)
        }
        if d.mdnsEnabled, let error = d.mdnsError { out.append(.mdnsFailed(error: error)) }

        for p in d.peers where !p.online {
            let reachableNet = p.lastKnown.isEmpty || p.lastKnown.contains { a in
                d.interfaces.contains { Addresses.sameSubnet(a, $0.addr, $0.netmask) }
            }
            if !reachableNet && !d.interfaces.isEmpty {
                out.append(.otherNetwork(peerLabel: p.label))
                continue
            }
            if let path = p.lastFailurePath, let failure = p.lastFailure {
                out.append(.peerNotReached(peerLabel: p.label, path: path, failure: failure))
            }
        }
        if out.isEmpty { out.append(.allFine) }
        return out
    }

    /// The core reports a failed join as `addr: error` text.
    static func splitJoinFailure(_ text: String) -> DiagIssue {
        if let range = text.range(of: ": "), range.lowerBound > text.startIndex {
            return .multicastJoinFailed(
                addr: String(text[..<range.lowerBound]),
                error: String(text[range.upperBound...])
            )
        }
        return .multicastJoinFailed(addr: text, error: "")
    }
}

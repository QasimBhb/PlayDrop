import Foundation
import PlayDropCore
import PlayDropShared

// Maps the core's `Diagnostics` record to `PlayDropShared.DiagSnapshot`, the Foundation-only
// mirror that `DiagnosticsSummary` and `LocalNetworkInference` work on (so both are tested
// on Linux), and gives the discovery paths their display names. A dial failure's path is
// passed as a stable token (`mdns`, `beacon`, `lastKnown`); `pathName` turns it into words.

/// See the file comment.
enum DiagnosticsMapping {
    /// The shared mirror of `d`.
    static func snapshot(_ d: Diagnostics) -> DiagSnapshot {
        DiagSnapshot(
            bindOk: d.bindOk,
            port: d.port,
            interfaces: d.interfaces.map { DiagInterface(addr: $0.addr, netmask: $0.netmask) },
            multicastEnabled: d.multicast.enabled,
            multicastJoinFailed: d.multicast.joinFailed,
            beaconRecently: d.multicast.beaconRecently,
            mdnsEnabled: d.mdns.enabled,
            mdnsRegistered: d.mdns.registered,
            mdnsError: d.mdns.error,
            peers: d.peers.map { p in
                DiagPeer(
                    label: p.label,
                    online: p.online,
                    lastKnown: p.lastKnown,
                    beaconSeen: p.beaconSeen,
                    mdnsSeen: p.mdnsSeen,
                    lastFailurePath: p.lastResult.map { token(for: $0.path) },
                    lastFailure: p.lastResult?.failure
                )
            }
        )
    }

    /// The stable token for a discovery path.
    static func token(for path: DiscoveryPath) -> String {
        switch path {
        case .mdns: return "mdns"
        case .beacon: return "beacon"
        case .lastKnown: return "lastKnown"
        }
    }

    /// Display name of a path token; unknown tokens are shown as they are.
    static func pathName(_ token: String) -> String {
        switch token {
        case "mdns": return L10n.Diag.pathMdns
        case "beacon": return L10n.Diag.pathBeacon
        case "lastKnown": return L10n.Diag.pathLastKnown
        default: return token
        }
    }

    /// Display name of a path.
    static func pathName(_ path: DiscoveryPath) -> String { pathName(token(for: path)) }

    /// The words for one finding. Port of Android `issueText`.
    static func text(_ issue: DiagIssue) -> String {
        switch issue {
        case .notListening(let port): return L10n.Diag.issueNotListening(port)
        case .noInterface: return L10n.Diag.issueNoInterface
        case .multicastJoinFailed(let addr, let error): return L10n.Diag.issueMulticastJoin(addr, error)
        case .multicastBlocked(let iface, let peer): return L10n.Diag.issueMulticastBlocked(iface, peer)
        case .discoveryOff: return L10n.Diag.issueDiscoveryOff
        case .mdnsFailed(let error): return L10n.Diag.issueMdns(error)
        case .otherNetwork(let peer): return L10n.Diag.issueOtherNetwork(peer)
        case .peerNotReached(let peer, let path, let failure):
            return L10n.Diag.issuePeer(peer, pathName(path), failure)
        case .allFine: return L10n.Diag.allFine
        }
    }
}

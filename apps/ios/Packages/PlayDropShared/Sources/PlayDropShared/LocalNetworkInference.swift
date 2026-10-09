// iOS has no API that reports the Local Network permission, and without it every LAN socket
// fails quietly (multicast joins and dials return "not permitted" or "no route to host").
// This file infers the permission from what the core saw, so the app can explain the
// problem and link to Settings instead of looking silently broken (anti-pattern 18).
import Foundation

/// The app's best guess at the Local Network permission.
public enum LocalNetworkAccess: Equatable, Sendable {
    /// Nothing seen yet either way.
    case unknown
    /// A peer was reached on the LAN, so the permission is on.
    case granted
    /// Failures look like the permission is off; `reason` says which signal (no secrets,
    /// only the core's error text).
    case likelyDenied(reason: String)
}

/// Infers `LocalNetworkAccess` from a diagnostics snapshot.
public enum LocalNetworkInference {
    /// Lower-cased error fragments that a denied Local Network permission produces on iOS:
    /// the errno names and their `strerror` texts (EPERM is os error 1, EHOSTUNREACH is
    /// os error 65 on Darwin).
    static let deniedMarkers = [
        "not permitted",
        "no route to host",
        "eperm",
        "ehostunreach",
        "os error 65",
    ]

    /// `.granted` when a peer was online during this run (or is online now); otherwise
    /// `.likelyDenied` when a multicast join, a peer's last dial or mDNS failed with a
    /// permission-shaped error, or mDNS is on but failed to register; otherwise `.unknown`.
    /// With no interface at all (Wi-Fi off) the answer is `.unknown`: nothing can be
    /// learned about the permission then.
    public static func infer(from d: DiagSnapshot, peerOnlineThisRun: Bool) -> LocalNetworkAccess {
        if peerOnlineThisRun || d.peers.contains(where: \.online) { return .granted }
        if d.interfaces.isEmpty { return .unknown }

        for failure in d.multicastJoinFailed where looksDenied(failure) {
            return .likelyDenied(reason: "multicast join failed: \(failure)")
        }
        for peer in d.peers {
            if let failure = peer.lastFailure, looksDenied(failure) {
                return .likelyDenied(reason: "dial failed: \(failure)")
            }
        }
        if d.mdnsEnabled {
            if let error = d.mdnsError {
                return .likelyDenied(reason: "mDNS failed: \(error)")
            }
            if !d.mdnsRegistered {
                return .likelyDenied(reason: "mDNS service not registered")
            }
        }
        return .unknown
    }

    /// True when `text` contains one of `deniedMarkers`, ignoring case.
    static func looksDenied(_ text: String) -> Bool {
        let lower = text.lowercased()
        return deniedMarkers.contains { lower.contains($0) }
    }
}

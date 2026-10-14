// Decisions behind the "Can't find your computer?" help (guest Wi-Fi path): which findings
// point to it, and which network interfaces can carry a PlayDrop peer. Pure Foundation so
// both run in the Linux tests; the screens word the result.

/// When to offer the connection help.
public enum ConnectivityHelp {
    /// True when `issues` say the two devices probably cannot find or reach each other on
    /// this network (multicast filtered or refused, discovery failing, a peer on another
    /// network, a peer that was not reached). False for a local fault the help cannot fix
    /// (listener down, Wi-Fi off) and for a healthy report.
    public static func isSuggested(for issues: [DiagIssue]) -> Bool {
        issues.contains { issue in
            switch issue {
            case .multicastJoinFailed, .multicastBlocked, .discoveryOff, .mdnsFailed,
                 .otherNetwork, .peerNotReached:
                return true
            case .notListening, .noInterface, .allFine:
                return false
            }
        }
    }

    /// True when this build can use multicast discovery. `profile` is the provisioning
    /// profile embedded in the app: a development build signed with a free Apple team carries
    /// no `com.apple.developer.networking.multicast` entitlement, so discovery cannot work
    /// and pairing by address or QR code is the normal path. A build with no embedded profile
    /// (App Store, TestFlight) gets the entitlement from the store, so it counts as capable.
    public static func multicastAvailable(profile: EmbeddedProvisioningProfile?) -> Bool {
        guard let profile else { return true }
        return profile.hasMulticastEntitlement
    }
}

/// Which network interfaces can carry a PlayDrop peer, by BSD name.
public enum InterfaceKinds {
    /// Name prefixes that never reach a LAN peer: cellular (`pdp_ip`), VPN and system tunnels
    /// (`utun`, `ipsec`), Apple Wireless Direct Link and its companion (`awdl`, `llw`).
    public static let skippedPrefixes = ["utun", "pdp_ip", "ipsec", "llw", "awdl"]

    /// True when the interface called `name` may reach a PlayDrop peer. Wi-Fi and Ethernet
    /// (`en*`) and Personal Hotspot (`bridge*`, which also carries the USB tether) stay in.
    public static func carriesLanPeers(_ name: String) -> Bool {
        !skippedPrefixes.contains { name.hasPrefix($0) }
    }
}

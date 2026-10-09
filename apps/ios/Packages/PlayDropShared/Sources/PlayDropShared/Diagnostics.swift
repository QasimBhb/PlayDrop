// A plain mirror of the core's `Diagnostics` record (from `PlayDropHost.diagnose()`), with
// only the fields the summary and the Local Network inference read. The app converts the
// UniFFI record into this one, so the decision logic lives here, in Foundation-only code
// that is unit-tested on Linux.

/// One IPv4 interface the core listens on.
public struct DiagInterface: Equatable, Sendable {
    /// Interface address, dotted IPv4.
    public let addr: String
    /// Netmask, dotted IPv4.
    public let netmask: String

    /// Creates an interface record.
    public init(addr: String, netmask: String) {
        self.addr = addr
        self.netmask = netmask
    }
}

/// What the core knows about reaching one paired peer.
public struct DiagPeer: Equatable, Sendable {
    /// Display label (shown with the Short ID elsewhere, SR-8).
    public let label: String
    /// Connected right now.
    public let online: Bool
    /// Last addresses the peer was reached at, as `ip:port`.
    public let lastKnown: [String]
    /// A discovery beacon from this peer was heard.
    public let beaconSeen: Bool
    /// The peer was seen over mDNS.
    public let mdnsSeen: Bool
    /// Discovery path of the most recent failed dial (`mdns`, `beacon`, `lastKnown`), if any.
    public let lastFailurePath: String?
    /// Reason of the most recent failed dial, if any.
    public let lastFailure: String?

    /// Creates a peer record.
    public init(
        label: String,
        online: Bool,
        lastKnown: [String],
        beaconSeen: Bool,
        mdnsSeen: Bool,
        lastFailurePath: String?,
        lastFailure: String?
    ) {
        self.label = label
        self.online = online
        self.lastKnown = lastKnown
        self.beaconSeen = beaconSeen
        self.mdnsSeen = mdnsSeen
        self.lastFailurePath = lastFailurePath
        self.lastFailure = lastFailure
    }
}

/// The diagnostics snapshot the summary is built from.
public struct DiagSnapshot: Equatable, Sendable {
    /// The listener is bound.
    public let bindOk: Bool
    /// The port it listens (or tried to listen) on.
    public let port: UInt16
    /// Usable IPv4 interfaces.
    public let interfaces: [DiagInterface]
    /// Beacon discovery is on.
    public let multicastEnabled: Bool
    /// Failed multicast joins, each as `addr: error` text.
    public let multicastJoinFailed: [String]
    /// A beacon was heard recently.
    public let beaconRecently: Bool
    /// mDNS discovery is on.
    public let mdnsEnabled: Bool
    /// The mDNS service is registered.
    public let mdnsRegistered: Bool
    /// The last mDNS error, if any.
    public let mdnsError: String?
    /// Every paired peer.
    public let peers: [DiagPeer]

    /// Creates a snapshot.
    public init(
        bindOk: Bool,
        port: UInt16,
        interfaces: [DiagInterface],
        multicastEnabled: Bool,
        multicastJoinFailed: [String],
        beaconRecently: Bool,
        mdnsEnabled: Bool,
        mdnsRegistered: Bool,
        mdnsError: String?,
        peers: [DiagPeer]
    ) {
        self.bindOk = bindOk
        self.port = port
        self.interfaces = interfaces
        self.multicastEnabled = multicastEnabled
        self.multicastJoinFailed = multicastJoinFailed
        self.beaconRecently = beaconRecently
        self.mdnsEnabled = mdnsEnabled
        self.mdnsRegistered = mdnsRegistered
        self.mdnsError = mdnsError
        self.peers = peers
    }
}

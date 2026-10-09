// The name to show for a peer when no user-facing label is known yet (for example in an
// event that arrives before the peer list is loaded). Mirrors the Android app's `labelOf`
// fallback: the Short ID, which the user compared at pairing, else the start of the hex
// Device ID. Never a name the peer chose for itself (SR-8).

/// Fallback labels for peers.
public enum PeerLabel {
    /// Characters of the Device ID used when no Short ID is known.
    public static let deviceIdPrefixLength = 8

    /// `shortId` when it is non-empty, else the first 8 characters of `deviceId`.
    public static func fallback(deviceId: String, shortId: String?) -> String {
        if let shortId, !shortId.isEmpty { return shortId }
        return String(deviceId.prefix(deviceIdPrefixLength))
    }
}

// Which feature a refusal from a paired device is about. A computer that has not granted
// this iPhone a feature answers our frame with an `error` (protocol.md 5.3, 5.14), which the
// core reports as a `peerRefused` event naming the frame type it answered and the error
// code. The app turns that into words for the screen that sent the frame (remote input,
// "Now playing"), so a refused command never fails silently (anti-pattern 18).
//
// `RefusalThrottle` keeps a burst of refused taps (every media button answers on its own)
// down to one message per device and feature within a few seconds.

/// A feature the other device can refuse because it has not granted it to this iPhone.
public enum RefusedFeature: String, Equatable, Hashable, Sendable {
    /// Remote input (`input` frames): the computer has remote input off for this phone.
    case input
    /// Media control (`media` command frames): the computer has Media off for this phone.
    case media
}

/// Maps a `peerRefused` event to the feature it refuses.
public enum PeerRefusals {
    /// The error code a device sends when the frame needs a grant it has not given.
    public static let forbiddenCode = "forbidden"

    /// The refused feature for a `peerRefused` event with `frameType` (nil when the core no
    /// longer knows which frame was answered) and `code`; nil for any other refusal, which
    /// the caller handles as a plain error.
    public static func feature(frameType: String?, code: String) -> RefusedFeature? {
        guard code == forbiddenCode, let frameType else { return nil }
        switch frameType {
        case "input": return .input
        case "media": return .media
        default: return nil
        }
    }
}

/// Lets one refusal message through per device and feature within `interval` seconds.
public struct RefusalThrottle: Sendable {
    /// Minimum time between two messages for the same device and feature.
    public let interval: Double
    private var lastShown: [Key: Double] = [:]

    private struct Key: Hashable, Sendable {
        let peer: String
        let feature: RefusedFeature
    }

    /// A throttle with `interval` seconds between messages (5 s by default).
    public init(interval: Double = 5) {
        self.interval = interval
    }

    /// True when a refusal of `feature` by `peer` at `now` (seconds, any monotonic origin)
    /// should be shown, and records it; false while the previous one is still recent.
    public mutating func shouldShow(peer: String, feature: RefusedFeature, now: Double) -> Bool {
        let key = Key(peer: peer, feature: feature)
        if let last = lastShown[key], now - last < interval, now >= last {
            return false
        }
        lastShown[key] = now
        return true
    }
}

// The settings contract between the app and the share extension. Both processes read the
// App Group's UserDefaults suite (`UserDefaults(suiteName: PlayDropIdentifiers.appGroup)`),
// the app being the only writer. Keeping the key strings, the mirrored peer entry and its
// JSON codec here gives both targets one copy, so they cannot drift apart: a renamed key or
// a changed field would otherwise make the extension silently read nothing.
import Foundation

/// UserDefaults keys of the shared settings suite. Changing a value loses that setting on
/// existing installs, so treat them as part of the on-disk format.
public enum SharedSettingsKeys {
    /// This device's display name (String; absent means the system name).
    public static let deviceName = "deviceName"
    /// Listening port (Integer; absent means `PlayDropIdentifiers.defaultPort`).
    public static let port = "port"
    /// Hex Device ID the share flow sends to without asking (String; absent when unset,
    /// never stored as an empty string).
    public static let defaultPeer = "defaultPeer"
    /// True once onboarding finished (Bool).
    public static let onboardingDone = "onboardingDone"
    /// Put incoming clipboard content on the pasteboard while in the foreground (Bool).
    public static let applyIncomingClipboard = "applyIncomingClipboard"
    /// Pull peer outboxes when the app opens (Bool).
    public static let pullOutboxOnOpen = "pullOutboxOnOpen"
    /// Core logging at debug level (Bool).
    public static let logLevelDebug = "logLevelDebug"
    /// JSON array of `SharedPeerEntry`, written by the app's peer directory mirror so the
    /// extension and the Shortcut device picker can name devices while the core is down.
    public static let peerDirectory = "peerDirectory"

    /// Every key, for tests that check they are distinct.
    public static let all: [String] = [
        deviceName, port, defaultPeer, onboardingDone, applyIncomingClipboard,
        pullOutboxOnOpen, logLevelDebug, peerDirectory,
    ]

    /// The stored form of a default-peer value: trimmed, and nil when nothing is left. Both
    /// the app's writer and every reader go through this, so an empty string is never
    /// written and a stray one is never read as a device.
    public static func normalizedPeer(_ value: String?) -> String? {
        guard let trimmed = value?.trimmingCharacters(in: .whitespacesAndNewlines), !trimmed.isEmpty else {
            return nil
        }
        return trimmed
    }
}

/// One paired peer as the app mirrors it into the shared suite. Display only (SR-8): the
/// label is never trusted for anything but showing, and is always shown with the Short ID.
public struct SharedPeerEntry: Codable, Equatable, Hashable, Sendable {
    /// Hex Device ID.
    public let deviceId: String
    /// Local label of the peer.
    public let label: String
    /// Short ID of the peer.
    public let shortId: String

    /// Creates an entry.
    public init(deviceId: String, label: String, shortId: String) {
        self.deviceId = deviceId
        self.label = label
        self.shortId = shortId
    }

    /// "Label (SHORT-ID)", or just the Short ID when the label is empty or is the Short ID.
    public var displayLabel: String {
        let trimmed = label.trimmingCharacters(in: .whitespacesAndNewlines)
        if trimmed.isEmpty || trimmed == shortId { return shortId }
        return "\(trimmed) (\(shortId))"
    }
}

/// Encodes and decodes the `peerDirectory` value.
public enum PeerDirectoryCodec {
    /// The JSON the app stores for `entries`. Keys are sorted so an unchanged list encodes
    /// to the same bytes.
    public static func encode(_ entries: [SharedPeerEntry]) throws -> Data {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys]
        return try encoder.encode(entries)
    }

    /// The entries stored in `data`; empty when there is nothing stored or it does not
    /// decode (an older or damaged value must never break the share sheet).
    public static func decode(_ data: Data?) -> [SharedPeerEntry] {
        guard let data else { return [] }
        return (try? JSONDecoder().decode([SharedPeerEntry].self, from: data)) ?? []
    }
}

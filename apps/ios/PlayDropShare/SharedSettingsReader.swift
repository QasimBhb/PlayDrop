import Foundation
import PlayDropShared

// Read-only view of the settings the app keeps in the App Group's UserDefaults suite. The
// share extension cannot import app target files, so it reads the keys, the peer entry type
// and its codec from PlayDropShared (`SharedSettings.swift`), the same copy the app writes
// with. It never writes: the app (AppSettings, and PeerDirectoryMirror for the peer
// directory) is the only writer, so the two processes cannot fight over a value.

/// Reads the shared settings. Every getter tolerates a missing suite (an App Group that is
/// not provisioned) by returning nil, so the extension degrades to "Queued" instead of failing.
struct SharedSettingsReader {
    private let defaults: UserDefaults?

    /// Opens the App Group suite.
    init() {
        defaults = UserDefaults(suiteName: PlayDropIdentifiers.appGroup)
    }

    /// The hex Device ID the user chose as the default target, if any.
    var defaultPeer: String? {
        SharedSettingsKeys.normalizedPeer(defaults?.string(forKey: SharedSettingsKeys.defaultPeer))
    }

    /// The paired peers as last mirrored by the app; empty when the app never ran.
    var peerDirectory: [SharedPeerEntry] {
        PeerDirectoryCodec.decode(defaults?.data(forKey: SharedSettingsKeys.peerDirectory))
    }

    /// The display label of `peer` from the mirrored directory, if it is listed.
    func displayLabel(of peer: String) -> String? {
        peerDirectory.first { $0.deviceId == peer }?.displayLabel
    }
}

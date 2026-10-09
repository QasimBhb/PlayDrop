import Foundation
import Observation
import PlayDropCore
import PlayDropShared

// Mirrors the paired-peer list into the App Group settings suite so that code running
// without the core can still name devices: the share extension shows "Queued for <device>",
// and the Shortcut device picker lists devices while the app is in the background. Names are
// display only and always carry the Short ID (SR-8). The core's peer store stays the source
// of truth; this copy is rewritten whenever `CoreHost.peers` changes while the core runs.
// The entry type, its JSON codec and the key live in PlayDropShared (`SharedSettings.swift`)
// so the extension decodes exactly what is written here.

/// Writes and reads the mirrored peer list; see the file comment.
@MainActor
final class PeerDirectoryMirror {
    /// The process-wide mirror; `AppServices.start()` starts it, readers (the Shortcut
    /// device picker) use `read()`.
    static let shared = PeerDirectoryMirror()

    /// Key in the App Group suite.
    static let key = SharedSettingsKeys.peerDirectory

    private let defaults = UserDefaults(suiteName: PlayDropIdentifiers.appGroup)
    private var observing = false
    private var lastWritten: [SharedPeerEntry]?

    private init() {}

    /// Starts following `CoreHost.shared.peers`. Idempotent.
    func startObserving() {
        guard !observing else { return }
        observing = true
        observePeers()
    }

    /// The mirrored peers, as last written (empty when the app never ran the core).
    func read() -> [SharedPeerEntry] {
        PeerDirectoryCodec.decode(defaults?.data(forKey: Self.key))
    }

    /// Writes the current peer list when the core is running. While it is down the list is
    /// unknown, so the last mirror is kept.
    func writeIfRunning() {
        let host = CoreHost.shared
        guard host.running else { return }
        let peers = host.peers.map { SharedPeerEntry(deviceId: $0.deviceId, label: $0.label, shortId: $0.shortId) }
        guard peers != lastWritten else { return }
        guard let data = try? PeerDirectoryCodec.encode(peers) else { return }
        defaults?.set(data, forKey: Self.key)
        lastWritten = peers
    }

    private func observePeers() {
        withObservationTracking {
            _ = CoreHost.shared.peers
            _ = CoreHost.shared.running
        } onChange: { [weak self] in
            // onChange fires before the new value is stored; hop to the main actor so the
            // write reads the updated list, then re-arm the observation.
            Task { @MainActor [weak self] in
                self?.writeIfRunning()
                self?.observePeers()
            }
        }
        writeIfRunning()
    }
}

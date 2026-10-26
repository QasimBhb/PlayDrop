// Builders for the core records the tests need. The generated PlayDropCore structs have
// long memberwise initialisers; these keep each test to the fields it is about.
import Foundation
import PlayDropCore
import PlayDropShared
@testable import PlayDrop

enum Fixtures {
    /// Every grant off except files and clipboard out.
    static let grants = Grants(files: true, clipboardIn: false, clipboardOut: true, input: false, media: false, find: false)

    /// A paired peer.
    static func peer(
        _ id: String, label: String = "", shortId: String? = nil, online: Bool = false,
        lastSeen: UInt64? = 100, removedByPeer: Bool = false, capabilities: [String] = []
    ) -> PeerSummary {
        PeerSummary(
            deviceId: id, shortId: shortId ?? "S-\(id)", label: label, online: online, grants: grants,
            lastKnown: [], pairedAt: 0, lastSeen: lastSeen, removedByPeer: removedByPeer,
            capabilities: capabilities
        )
    }

    /// A transfer snapshot.
    static func transfer(
        _ id: String, peer: String = "p", direction: TransferDirection = .receive,
        state: TransferStatus = .active, bytes: UInt64 = 0, total: UInt64 = 100,
        bytesPerSec: UInt64 = 0, startedAt: UInt64 = 0, note: String? = nil,
        delivered: [DeliveredFile] = [], resumable: Bool = false
    ) -> TransferSnapshot {
        TransferSnapshot(
            id: id, peer: peer, shortId: "S-\(peer)", direction: direction, state: state,
            bytes: bytes, total: total, bytesPerSec: bytesPerSec, filesDone: 0, startedAt: startedAt,
            fileCount: 1, note: note, delivered: delivered, resumable: resumable
        )
    }

    /// A committed outgoing batch with one item.
    static func batch(_ id: String, preferredPeer: String? = nil) -> OutgoingBatch {
        OutgoingBatch(
            id: id, createdAtMs: 0,
            items: [OutgoingItem(fileName: "a.txt", displayName: "a.txt", size: 1, mimeType: "text/plain", modifiedMs: 0)],
            note: nil, preferredPeer: preferredPeer
        )
    }

    /// A clipboard push result.
    static func push(
        sentTo: [String] = [], echo: Bool = false, offline: UInt32 = 0, noGrant: UInt32 = 0
    ) -> ClipboardPush {
        ClipboardPush(sentTo: sentTo, skippedEcho: echo, skippedOffline: offline, skippedNoGrant: noGrant, skippedUnsupported: 0)
    }

    /// A media player state.
    static func media(
        _ player: String, name: String? = nil, playing: Bool = false, can: [MediaAction] = [],
        durationMs: UInt64? = nil, positionMs: UInt64? = nil, positionAtMs: UInt64? = nil
    ) -> MediaState {
        MediaState(
            player: player, name: name, playing: playing, title: nil, artist: nil, album: nil,
            durationMs: durationMs, positionMs: positionMs, positionAtMs: positionAtMs, volume: nil, can: can
        )
    }

    /// A fresh, empty UserDefaults suite; remove it with `removePersistentDomain(forName:)`.
    static func defaults() -> (UserDefaults, String) {
        let name = "org.playdrop.tests.\(UUID().uuidString)"
        return (UserDefaults(suiteName: name)!, name)
    }

    /// A new empty directory under the temporary directory.
    static func tempDir() throws -> URL {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent("pd-tests-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        return dir
    }
}

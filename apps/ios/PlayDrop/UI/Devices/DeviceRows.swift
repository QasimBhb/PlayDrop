import Foundation
import PlayDropCore

// Turns the core's peers plus the last battery and presence reports into what a device row
// shows, and decides where a one-tap send goes. Plain logic with no SwiftUI so it can be
// unit-tested. Port of Android `ui/devices/DeviceRows.kt`.

/// A paired device as a row shows it.
struct DeviceRow: Identifiable, Equatable, Sendable {
    /// Hex Device ID.
    var id: String { deviceId }
    /// Hex Device ID.
    let deviceId: String
    /// Local label.
    let label: String
    /// Short ID, always shown next to the label (SR-8).
    let shortId: String
    /// A session is up.
    let online: Bool
    /// Online but reported `asleep`.
    let asleep: Bool
    /// Last battery percent the device reported, if any.
    let batteryLevel: Int?
    /// Charging at the last report.
    let charging: Bool
    /// Last frame, Unix seconds; shown while offline.
    let lastSeen: UInt64?
    /// The one-tap send target.
    let isDefault: Bool
    /// The other side unpaired us; pairing again is needed.
    let removedByPeer: Bool
}

/// Derives device rows; see the file comment.
enum DeviceRows {
    /// Rows sorted default first, then online ones, then by label.
    static func derive(peers: [PeerSummary], status: [String: PeerStatus], defaultPeer: String?) -> [DeviceRow] {
        peers.map { p in
            let s = status[p.deviceId]
            return DeviceRow(
                deviceId: p.deviceId,
                label: p.label,
                shortId: p.shortId,
                online: p.online,
                asleep: p.online && s?.presence == "asleep",
                batteryLevel: s?.batteryLevel.map(Int.init),
                charging: s?.charging ?? false,
                lastSeen: p.lastSeen,
                isDefault: p.deviceId == defaultPeer,
                removedByPeer: p.removedByPeer
            )
        }
        .sorted { a, b in
            if a.isDefault != b.isDefault { return a.isDefault }
            if a.online != b.online { return a.online }
            return a.label.localizedCaseInsensitiveCompare(b.label) == .orderedAscending
        }
    }

    /// Where a one-tap send goes: the default device if it is still paired, otherwise the
    /// only paired device, otherwise nil (the screen asks).
    static func sendTarget(peers: [PeerSummary], defaultPeer: String?) -> String? {
        if let d = defaultPeer, peers.contains(where: { $0.deviceId == d }) { return d }
        return peers.count == 1 ? peers[0].deviceId : nil
    }

    /// The status line: removed, asleep, online, or "last seen 5 min ago".
    static func statusText(_ row: DeviceRow, now: UInt64) -> String {
        if row.removedByPeer { return L10n.Devices.removedShort }
        if row.asleep { return L10n.Common.asleep }
        if row.online { return L10n.Common.online }
        return L10n.Devices.lastSeen(UIFormat.ago(row.lastSeen, now: now))
    }
}

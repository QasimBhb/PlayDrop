import AppIntents
import Foundation
import PlayDropCore

// A paired device as Shortcuts sees it, so "Send Clipboard" can target one device. The list
// comes from the running core (`CoreHost.shared.peers`); when the core is down (the Shortcuts
// editor queries the app in the background) it falls back to the peer list the app mirrors
// into the App Group. Names are display only and always shown with the Short ID (SR-8).

/// One paired device for Shortcuts.
struct DeviceEntity: AppEntity {
    /// Shown as the parameter's type name.
    static let typeDisplayRepresentation: TypeDisplayRepresentation = "Device"
    /// Resolves and suggests devices.
    static let defaultQuery = DeviceQuery()

    /// Hex Device ID.
    let id: String
    /// Local label of the device.
    let label: String
    /// Short ID of the device.
    let shortId: String

    /// Label as the title, Short ID as the subtitle.
    var displayRepresentation: DisplayRepresentation {
        DisplayRepresentation(title: "\(label)", subtitle: "\(shortId)")
    }
}

/// Lists paired devices for the Shortcuts parameter picker.
struct DeviceQuery: EntityQuery {
    /// The devices among `identifiers` that are still paired.
    func entities(for identifiers: [DeviceEntity.ID]) async throws -> [DeviceEntity] {
        let wanted = Set(identifiers)
        return await Self.allDevices().filter { wanted.contains($0.id) }
    }

    /// Every paired device.
    func suggestedEntities() async throws -> [DeviceEntity] {
        await Self.allDevices()
    }

    /// Paired devices from the running core, else from the mirrored list.
    @MainActor
    private static func allDevices() -> [DeviceEntity] {
        let host = CoreHost.shared
        if host.running {
            return host.peers.map { DeviceEntity(id: $0.deviceId, label: $0.label, shortId: $0.shortId) }
        }
        return PeerDirectoryMirror.shared.read().map {
            DeviceEntity(id: $0.deviceId, label: $0.label, shortId: $0.shortId)
        }
    }
}

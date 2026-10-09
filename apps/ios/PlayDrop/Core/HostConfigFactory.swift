import Foundation
import PlayDropCore
import PlayDropShared

// Builds the `HostConfig` the iOS app starts the core with, and the two directories it
// names:
// - data dir `<Application Support>/playdrop/core`: identity, pins and transfer state.
//   Created with mode 0700 (the core refuses loose permissions, SR-28) and excluded from
//   backups: its secrets are sealed with a this-device-only Keychain key, so a restored copy
//   on another phone could never be opened and would only make start fail.
// - receive dir `<Documents>/PlayDrop`: finished files land here and show in the Files app
//   under "On My iPhone > PlayDrop" (`UIFileSharingEnabled` in Info.plist). The core stages
//   and finalises in place (`ReceiveTarget.directory`).
// Everything else follows the platform table in docs/core-api.md: phone, mobile keep-alive,
// any address, discovery on, `getifaddrs` interfaces, Keychain secret store.

/// What the config needs from the main actor (AppSettings and UIDevice), captured first so
/// the file work can run off the main actor.
struct HostConfigInputs: Sendable {
    /// Name shown to peers.
    let deviceName: String
    /// Listening port.
    let port: UInt16
    /// Device type announced to peers.
    let deviceType: DeviceType
}

/// Builds `HostConfig`; see the file comment.
enum HostConfigFactory {
    /// The core's data directory, created if needed.
    static func dataDirectory() throws -> URL {
        let support = try FileManager.default.url(
            for: .applicationSupportDirectory, in: .userDomainMask, appropriateFor: nil, create: true
        )
        let dir = support.appending(path: "playdrop", directoryHint: .isDirectory)
            .appending(path: "core", directoryHint: .isDirectory)
        try ensureDirectory(dir, posixPermissions: 0o700)
        var values = URLResourceValues()
        values.isExcludedFromBackup = true
        var mutable = dir
        try mutable.setResourceValues(values)
        return dir
    }

    /// The folder under Documents where received files land, created if needed.
    static func receiveDirectory() throws -> URL {
        let documents = try FileManager.default.url(
            for: .documentDirectory, in: .userDomainMask, appropriateFor: nil, create: true
        )
        let dir = documents.appending(path: PlayDropIdentifiers.receiveFolderName, directoryHint: .isDirectory)
        try ensureDirectory(dir, posixPermissions: nil)
        return dir
    }

    /// The full config. Blocking file work; call it off the main actor.
    static func make(_ inputs: HostConfigInputs) throws -> HostConfig {
        let data = try dataDirectory()
        let receive = try receiveDirectory()
        return HostConfig(
            dataDir: data.path(percentEncoded: false),
            receive: .directory(path: receive.path(percentEncoded: false)),
            deviceName: inputs.deviceName,
            deviceType: inputs.deviceType,
            keepAlive: .mobile,
            port: inputs.port,
            bindIp: nil,
            discovery: true,
            interfaces: SystemInterfaces(),
            secretStore: KeychainSecretStore()
        )
    }

    /// Creates `dir` with its parents and, when given, forces `posixPermissions` on it.
    private static func ensureDirectory(_ dir: URL, posixPermissions: Int?) throws {
        let fm = FileManager.default
        let path = dir.path(percentEncoded: false)
        if !fm.fileExists(atPath: path) {
            try fm.createDirectory(at: dir, withIntermediateDirectories: true)
        }
        if let posixPermissions {
            try fm.setAttributes([.posixPermissions: NSNumber(value: posixPermissions)], ofItemAtPath: path)
        }
    }
}

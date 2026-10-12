// Fixed identifiers shared by the app, the share extension, the Shortcut action and the
// build configuration. Change them here and in `apps/ios/project.yml` together; nothing
// else may spell them out.
import Foundation

/// Identifiers that the app, the extension and the tooling must agree on.
public enum PlayDropIdentifiers {
    /// Bundle identifier of the app.
    public static let appBundleId = "org.playdrop.ios"
    /// Bundle identifier of the share extension.
    public static let shareExtensionBundleId = "org.playdrop.ios.share"
    /// App Group that carries the share extension's outgoing queue and the shared settings,
    /// as registered for this build: the first group in the embedded provisioning profile
    /// (xtool prefixes it, see `EmbeddedProvisioningProfile`), else `defaultAppGroup`.
    /// Resolved once per process; the app and the extension each read their own profile and
    /// land on the same group because both were provisioned from the same bundle identifier.
    public static let appGroup: String =
        EmbeddedProvisioningProfile.load(from: .main)?.appGroups.first ?? defaultAppGroup
    /// App Group as declared in the entitlements files and `project.yml`; what Xcode-signed
    /// and App Store builds use.
    public static let defaultAppGroup = "group.org.playdrop.ios"
    /// Keychain access group for the secret-store key (prefixed with the team id by Xcode).
    public static let keychainAccessGroup = "org.playdrop.ios"
    /// URL scheme. Opening `playdrop://` only brings the app forward and drains the outgoing
    /// queue; every parameter is ignored (threat model SR-30).
    public static let urlScheme = "playdrop"
    /// Bonjour service type the core advertises and browses (protocol.md 1 and 8.1); it must
    /// be listed under `NSBonjourServices` in the app's Info.plist.
    public static let bonjourServiceType = "_playdrop._tcp"
    /// Default TCP and UDP port (protocol.md 1).
    public static let defaultPort: UInt16 = 42187
    /// Folder under Documents where received files land; shown in the Files app.
    public static let receiveFolderName = "PlayDrop"
    /// Folder inside the App Group container where the share extension queues items.
    public static let outgoingQueueFolderName = "outgoing"
}

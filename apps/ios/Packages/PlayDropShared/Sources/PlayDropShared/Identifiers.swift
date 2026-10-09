// Fixed identifiers shared by the app, the share extension, the Shortcut action and the
// build configuration. Change them here and in `apps/ios/project.yml` together; nothing
// else may spell them out.

/// Identifiers that the app, the extension and the tooling must agree on.
public enum PlayDropIdentifiers {
    /// Bundle identifier of the app.
    public static let appBundleId = "org.playdrop.ios"
    /// Bundle identifier of the share extension.
    public static let shareExtensionBundleId = "org.playdrop.ios.share"
    /// App Group that carries the share extension's outgoing queue and the shared settings.
    public static let appGroup = "group.org.playdrop.ios"
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

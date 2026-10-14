// The entitlements a development build was actually signed with, read from the provisioning
// profile that Xcode and xtool embed in the bundle. The App Group identifier can differ from
// the one in `project.yml`: xtool registers every identifier under its own prefix
// (`group.XTL-<id>.org.playdrop.ios`), and a free Apple team gets no App Group at all. The app
// and the extension therefore ask their own profile which group they really have
// (`PlayDropIdentifiers.appGroup`) instead of trusting the constant. App Store builds carry
// no embedded profile, and for them the constant is right.
import Foundation

/// The parts of an `embedded.mobileprovision` file that the app and the extension need.
public struct EmbeddedProvisioningProfile: Sendable, Equatable {
    /// File name of the profile inside an app or extension bundle.
    public static let fileName = "embedded.mobileprovision"
    /// Entitlement keys read from the profile.
    static let appGroupsKey = "com.apple.security.application-groups"
    static let applicationIdentifierKey = "application-identifier"
    static let multicastKey = "com.apple.developer.networking.multicast"

    /// `com.apple.security.application-groups`, in the profile's order; empty when the build
    /// has no App Group.
    public let appGroups: [String]
    /// `application-identifier` (`<team id>.<bundle id>`), nil when the profile lacks it.
    public let applicationIdentifier: String?
    /// `com.apple.developer.networking.multicast` is true: the build may send and join
    /// multicast, which discovery needs. False on a free-team build.
    public let hasMulticastEntitlement: Bool

    public init(appGroups: [String], applicationIdentifier: String?, hasMulticastEntitlement: Bool = false) {
        self.appGroups = appGroups
        self.applicationIdentifier = applicationIdentifier
        self.hasMulticastEntitlement = hasMulticastEntitlement
    }

    /// The profile embedded in `bundle`; nil when there is none (App Store and TestFlight
    /// builds, Linux tests) or it cannot be parsed.
    public static func load(from bundle: Bundle) -> EmbeddedProvisioningProfile? {
        guard let url = bundle.url(forResource: "embedded", withExtension: "mobileprovision"),
              let data = try? Data(contentsOf: url)
        else { return nil }
        return EmbeddedProvisioningProfile(data: data)
    }

    /// Parses the raw file. A profile is a CMS (PKCS#7) signature wrapping an XML property
    /// list; the signature is not checked here (iOS checked it at install time), the plist is
    /// cut out by its `<?xml` and `</plist>` markers and decoded. Returns nil when no plist is
    /// found or it is not a dictionary with an `Entitlements` dictionary.
    public init?(data: Data) {
        guard let start = data.range(of: Data("<?xml".utf8)),
              let end = data.range(of: Data("</plist>".utf8), in: start.lowerBound..<data.endIndex)
        else { return nil }
        let plist = data[start.lowerBound..<end.upperBound]
        guard let root = try? PropertyListSerialization.propertyList(from: plist, format: nil),
              let dict = root as? [String: Any],
              let entitlements = dict["Entitlements"] as? [String: Any]
        else { return nil }
        appGroups = entitlements[Self.appGroupsKey] as? [String] ?? []
        applicationIdentifier = entitlements[Self.applicationIdentifierKey] as? String
        hasMulticastEntitlement = entitlements[Self.multicastKey] as? Bool ?? false
    }
}

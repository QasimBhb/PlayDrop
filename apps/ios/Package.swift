// swift-tools-version: 6.0
// The PlayDrop iOS app as a Swift package, for building without Xcode: xtool
// (https://xtool.sh) turns the two library products below into PlayDrop.app and its share
// extension, signs them and installs them on an iPhone from Linux (`scripts/xtool-dev.sh`,
// README "Building on Linux with xtool"). Xcode keeps using `project.yml`; both read the
// same source folders, so a file added to PlayDrop/ or PlayDropShare/ is in both builds.
//
// What differs from the Xcode build: no asset catalog (the icon comes from xtool.yml),
// no string catalog (every string is its English source text), no AppIntents metadata
// extraction (the Shortcut is not listed in the Shortcuts app) and no unit tests.
import PackageDescription

let package = Package(
    name: "PlayDrop",
    platforms: [.iOS(.v17)],
    products: [
        // xtool: the application.
        .library(name: "PlayDrop", targets: ["PlayDrop"]),
        // xtool: the share extension (xtool.yml, `extensions`).
        .library(name: "PlayDropShare", targets: ["PlayDropShare"]),
    ],
    dependencies: [
        .package(path: "Packages/PlayDropCore"),
        .package(path: "Packages/PlayDropShared"),
    ],
    targets: [
        .target(
            name: "PlayDrop",
            dependencies: [
                .product(name: "PlayDropCore", package: "PlayDropCore"),
                .product(name: "PlayDropShared", package: "PlayDropShared"),
            ],
            path: "PlayDrop",
            // Info.plist, entitlements, the catalogs and the privacy manifest are not
            // sources; xtool.yml names the ones it needs.
            exclude: ["Resources"],
            swiftSettings: swiftSettings,
            linkerSettings: [
                // Same explicit link as project.yml, so the Shortcut's types resolve.
                .linkedFramework("AppIntents"),
            ]
        ),
        .target(
            name: "PlayDropShare",
            dependencies: [
                .product(name: "PlayDropShared", package: "PlayDropShared"),
            ],
            path: "PlayDropShare",
            exclude: ["Info.plist", "PlayDropShare.entitlements", "PrivacyInfo.xcprivacy"],
            swiftSettings: swiftSettings,
            linkerSettings: [
                // xtool links the extension as a static archive behind a stub whose entry
                // point is Foundation's _NSExtensionMain, so nothing references this code
                // and the linker drops all of it (an empty 48 KiB binary that cannot even
                // be signed). -ObjC keeps every member that defines an Objective-C class,
                // which is what the principal class lookup needs. Xcode compiles the
                // extension sources straight into the appex and never sees this flag.
                .unsafeFlags(["-Xlinker", "-ObjC"]),
            ]
        ),
    ]
)

/// Mirrors project.yml: Swift 5 language mode with the Swift 6 concurrency checks on as
/// warnings (SWIFT_VERSION 5.0, SWIFT_STRICT_CONCURRENCY complete).
var swiftSettings: [SwiftSetting] {
    [
        .swiftLanguageMode(.v5),
        .enableExperimentalFeature("StrictConcurrency"),
    ]
}

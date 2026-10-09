// swift-tools-version: 6.0
// PlayDropShared: logic shared by the app and the share extension that needs only
// Foundation, so it builds and tests on Linux too (`swift test` in this directory).
// No UIKit, SwiftUI, Security or PlayDropCore imports in this package.
import PackageDescription

let package = Package(
    name: "PlayDropShared",
    platforms: [.iOS(.v17), .macOS(.v14)],
    products: [
        .library(name: "PlayDropShared", targets: ["PlayDropShared"]),
    ],
    targets: [
        .target(
            name: "PlayDropShared",
            swiftSettings: [.swiftLanguageMode(.v6)]
        ),
        .testTarget(
            name: "PlayDropSharedTests",
            dependencies: ["PlayDropShared"],
            swiftSettings: [.swiftLanguageMode(.v6)]
        ),
    ]
)

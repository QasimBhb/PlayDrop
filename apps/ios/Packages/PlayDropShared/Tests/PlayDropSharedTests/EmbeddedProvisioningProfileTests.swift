// The provisioning profile reader: the plist is cut out of the CMS envelope and the App Group
// and application identifier come from its Entitlements dictionary.
import XCTest
@testable import PlayDropShared

final class EmbeddedProvisioningProfileTests: XCTestCase {
    /// A profile-shaped blob: binary junk around an XML plist, as in a real file.
    private func profile(entitlements: String) -> Data {
        var data = Data([0x30, 0x82, 0x1A, 0x2B, 0x06, 0x09, 0x00, 0xFF])
        data.append(Data("""
        <?xml version="1.0" encoding="UTF-8"?>
        <!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
        <plist version="1.0">
        <dict>
            <key>AppIDName</key>
            <string>XTool org playdrop ios</string>
            <key>Entitlements</key>
            <dict>
        \(entitlements)
            </dict>
            <key>TeamIdentifier</key>
            <array><string>WZB26M56NX</string></array>
        </dict>
        </plist>
        """.utf8))
        data.append(Data([0x00, 0x01, 0x3C, 0x2F, 0x70, 0x6C, 0x69, 0x73]))
        return data
    }

    func testReadsAppGroupsAndApplicationIdentifier() throws {
        let data = profile(entitlements: """
                <key>application-identifier</key>
                <string>WZB26M56NX.XTL-ABCD1234.org.playdrop.ios</string>
                <key>com.apple.security.application-groups</key>
                <array>
                    <string>group.XTL-ABCD1234.org.playdrop.ios</string>
                    <string>group.XTL-ABCD1234.other</string>
                </array>
                <key>get-task-allow</key>
                <true/>
        """)
        let p = try XCTUnwrap(EmbeddedProvisioningProfile(data: data))
        XCTAssertEqual(p.appGroups, ["group.XTL-ABCD1234.org.playdrop.ios", "group.XTL-ABCD1234.other"])
        XCTAssertEqual(p.applicationIdentifier, "WZB26M56NX.XTL-ABCD1234.org.playdrop.ios")
    }

    func testProfileWithoutAppGroupGivesEmptyList() throws {
        let data = profile(entitlements: """
                <key>application-identifier</key>
                <string>WZB26M56NX.XTL-ABCD1234.org.playdrop.ios</string>
        """)
        let p = try XCTUnwrap(EmbeddedProvisioningProfile(data: data))
        XCTAssertEqual(p.appGroups, [])
    }

    func testGarbageAndMissingEntitlementsAreNil() {
        XCTAssertNil(EmbeddedProvisioningProfile(data: Data()))
        XCTAssertNil(EmbeddedProvisioningProfile(data: Data("no plist here".utf8)))
        let noEntitlements = Data("""
        <?xml version="1.0" encoding="UTF-8"?>
        <plist version="1.0"><dict><key>Name</key><string>x</string></dict></plist>
        """.utf8)
        XCTAssertNil(EmbeddedProvisioningProfile(data: noEntitlements))
        let notADict = Data("""
        <?xml version="1.0" encoding="UTF-8"?>
        <plist version="1.0"><array><string>x</string></array></plist>
        """.utf8)
        XCTAssertNil(EmbeddedProvisioningProfile(data: notADict))
    }

    func testIdentifiersFallBackToTheDeclaredGroupWithoutAProfile() {
        // Test bundles carry no embedded.mobileprovision, so the constant must win.
        XCTAssertEqual(PlayDropIdentifiers.appGroup, PlayDropIdentifiers.defaultAppGroup)
        XCTAssertEqual(PlayDropIdentifiers.defaultAppGroup, "group.org.playdrop.ios")
    }
}

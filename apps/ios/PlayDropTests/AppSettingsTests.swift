// AppSettings over a private UserDefaults suite: defaults, the shared key strings the share
// extension reads, and that an unset default device removes the key instead of storing "".
import PlayDropShared
import XCTest
@testable import PlayDrop

final class AppSettingsTests: XCTestCase {
    /// Runs `body` with settings over a fresh suite, then deletes the suite.
    @MainActor
    private func withSettings(_ body: (AppSettings, UserDefaults) throws -> Void) rethrows {
        let (defaults, name) = Fixtures.defaults()
        defer { defaults.removePersistentDomain(forName: name) }
        try body(AppSettings(defaults: defaults), defaults)
    }

    @MainActor
    func testDefaults() {
        withSettings { s, _ in
            XCTAssertEqual(s.port, PlayDropIdentifiers.defaultPort)
            XCTAssertNil(s.defaultPeer)
            XCTAssertFalse(s.onboardingDone)
            XCTAssertTrue(s.applyIncomingClipboard)
            XCTAssertTrue(s.pullOutboxOnOpen)
            XCTAssertFalse(s.logLevelDebug)
            XCTAssertFalse(s.deviceName.isEmpty)
        }
    }

    @MainActor
    func testWritesTheSharedKeys() {
        withSettings { s, defaults in
            s.defaultPeer = "ab12"
            s.onboardingDone = true
            s.port = 42188
            s.deviceName = "Kitchen iPhone"
            XCTAssertEqual(defaults.string(forKey: SharedSettingsKeys.defaultPeer), "ab12")
            XCTAssertTrue(defaults.bool(forKey: SharedSettingsKeys.onboardingDone))
            XCTAssertEqual(defaults.integer(forKey: SharedSettingsKeys.port), 42188)
            XCTAssertEqual(defaults.string(forKey: SharedSettingsKeys.deviceName), "Kitchen iPhone")
        }
    }

    @MainActor
    func testTrackpadSensitivityDefaultsAndClamps() {
        withSettings { s, defaults in
            XCTAssertEqual(s.trackpadSensitivity, TrackpadGestures.defaultSensitivity)
            s.trackpadSensitivity = 2.5
            XCTAssertEqual(defaults.double(forKey: AppSettings.trackpadSensitivityKey), 2.5)
            s.trackpadSensitivity = 99
            XCTAssertEqual(s.trackpadSensitivity, TrackpadGestures.sensitivityRange.upperBound)
        }
    }

    @MainActor
    func testUnsetDefaultPeerRemovesTheKey() {
        withSettings { s, defaults in
            s.defaultPeer = "ab12"
            s.defaultPeer = nil
            XCTAssertNil(defaults.object(forKey: SharedSettingsKeys.defaultPeer))
            s.defaultPeer = "ab12"
            s.defaultPeer = "   "
            XCTAssertNil(defaults.object(forKey: SharedSettingsKeys.defaultPeer), "blank is unset, never stored")
            defaults.set("", forKey: SharedSettingsKeys.defaultPeer)
            XCTAssertNil(s.defaultPeer, "a stray empty value reads as unset")
        }
    }

    @MainActor
    func testPortRules() {
        withSettings { s, defaults in
            s.port = 80
            XCTAssertEqual(s.port, PlayDropIdentifiers.defaultPort, "privileged ports are refused")
            s.port = PlayDropIdentifiers.defaultPort
            XCTAssertNil(defaults.object(forKey: SharedSettingsKeys.port), "the default is not stored")
        }
    }

    @MainActor
    func testDeviceNameCleaning() {
        XCTAssertEqual(AppSettings.cleanDeviceName("  Desk  "), "Desk")
        XCTAssertEqual(AppSettings.cleanDeviceName("   "), "")
        XCTAssertLessThanOrEqual(AppSettings.cleanDeviceName(String(repeating: "é", count: 60)).utf8.count, AppSettings.maxDeviceNameBytes)
        XCTAssertFalse(AppSettings.cleanDeviceName("a/b").contains("/"))
    }
}

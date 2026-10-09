// Where the drain sends a shared batch: its preferred device while paired, else the
// default device while paired, else nobody (the UI asks).
import PlayDropShared
import XCTest
@testable import PlayDrop

final class OutgoingDrainRulesTests: XCTestCase {
    private func target(_ preferred: String?, paired: Set<String>, defaultPeer: String?) -> String? {
        OutgoingDrain.intendedPeer(for: Fixtures.batch("b", preferredPeer: preferred), paired: paired, defaultPeer: defaultPeer)
    }

    func testPreferredWinsWhilePaired() {
        XCTAssertEqual(target("a", paired: ["a", "d"], defaultPeer: "d"), "a")
    }

    func testFallsBackToDefault() {
        XCTAssertEqual(target("gone", paired: ["d"], defaultPeer: "d"), "d", "the preferred device was unpaired")
        XCTAssertEqual(target(nil, paired: ["d"], defaultPeer: "d"), "d")
    }

    func testNobodyWhenNeitherIsPaired() {
        XCTAssertNil(target("gone", paired: ["x"], defaultPeer: "also-gone"))
        XCTAssertNil(target(nil, paired: ["x"], defaultPeer: nil))
        XCTAssertNil(target(nil, paired: [], defaultPeer: "d"))
    }

    @MainActor
    func testSeededSettingsFeedTheDefault() {
        let (defaults, name) = Fixtures.defaults()
        defer { defaults.removePersistentDomain(forName: name) }
        let settings = AppSettings(defaults: defaults)
        settings.defaultPeer = "d"
        XCTAssertEqual(target(nil, paired: ["d"], defaultPeer: settings.defaultPeer), "d")
        settings.defaultPeer = "  "
        XCTAssertNil(target(nil, paired: ["d"], defaultPeer: settings.defaultPeer))
    }
}

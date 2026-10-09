// Port setting validation, including the KDE Connect and LocalSend ports (CLAUDE.md hard rules).
import XCTest
@testable import PlayDrop

final class PortRulesTests: XCTestCase {
    func testCheck() {
        XCTAssertEqual(PortRules.check("42187"), .ok(42187))
        XCTAssertEqual(PortRules.check(" 42188 "), .ok(42188))
        XCTAssertEqual(PortRules.check("1024"), .ok(1024))
        XCTAssertEqual(PortRules.check("65535"), .ok(65535))
        XCTAssertEqual(PortRules.check("1714"), .reserved)
        XCTAssertEqual(PortRules.check("1764"), .reserved)
        XCTAssertEqual(PortRules.check("53317"), .reserved)
        XCTAssertEqual(PortRules.check("80"), .invalid)
        XCTAssertEqual(PortRules.check("65536"), .invalid)
        XCTAssertEqual(PortRules.check(""), .invalid)
        XCTAssertEqual(PortRules.check("4a"), .invalid)
        XCTAssertEqual(PortRules.check("\u{0664}\u{0662}\u{0661}\u{0668}\u{0667}"), .invalid, "non-ASCII digits")
    }
}

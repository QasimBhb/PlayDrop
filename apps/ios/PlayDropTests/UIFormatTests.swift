// Number and time formatting: binary units through ByteFormat, countdowns, SAS grouping, time left.
import XCTest
@testable import PlayDrop

final class UIFormatTests: XCTestCase {
    func testBinaryUnits() {
        XCTAssertEqual(UIFormat.bytes(1536), "1.5 KiB")
        XCTAssertEqual(UIFormat.bytes(2 * 1024 * 1024 * 1024), "2.0 GiB")
        XCTAssertEqual(UIFormat.rate(30 * 1024 * 1024), "30.0 MiB/s")
    }

    func testFractionAndPercent() {
        XCTAssertEqual(UIFormat.fraction(5, of: 0), 0)
        XCTAssertEqual(UIFormat.fraction(150, of: 100), 1)
        XCTAssertEqual(UIFormat.fraction(25, of: 100), 0.25, accuracy: 0.0001)
        XCTAssertEqual(UIFormat.percent(1, of: 4), 25)
    }

    func testEta() {
        XCTAssertNil(UIFormat.etaSeconds(remaining: 10, bytesPerSec: 0))
        XCTAssertEqual(UIFormat.etaSeconds(remaining: 10, bytesPerSec: 3), 4, "rounds up")
        XCTAssertEqual(UIFormat.etaSeconds(remaining: 9, bytesPerSec: 3), 3)
        XCTAssertEqual(UIFormat.etaSeconds(remaining: 0, bytesPerSec: 3), 0)
    }

    func testCountdownAndSas() {
        XCTAssertEqual(UIFormat.countdown(95), "1:35")
        XCTAssertEqual(UIFormat.countdown(-4), "0:00")
        XCTAssertEqual(UIFormat.sas("123456"), "123 456")
        XCTAssertEqual(UIFormat.sas("12345"), "12345", "only six digits are grouped")
    }

    func testAgo() {
        XCTAssertEqual(UIFormat.ago(nil, now: 5), "never")
        XCTAssertEqual(UIFormat.ago(100, now: 120), "just now")
        XCTAssertEqual(UIFormat.ago(0, now: 7200), "2 hours ago")
        XCTAssertEqual(UIFormat.ago(0, now: 86_400), "1 day ago")
        XCTAssertEqual(UIFormat.ago(500, now: 100), "just now", "a clock that went backwards")
    }

    func testNowSecs() {
        XCTAssertEqual(UIFormat.nowSecs(Date(timeIntervalSince1970: 1_791_000_000.9)), 1_791_000_000)
    }
}

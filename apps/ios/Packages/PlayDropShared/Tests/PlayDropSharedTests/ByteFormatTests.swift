// Number and time formatting matches the Android app and the desktop window.
import XCTest
@testable import PlayDropShared

final class ByteFormatTests: XCTestCase {
    func testBytesUseBinaryUnitsWithOneDecimal() {
        XCTAssertEqual(ByteFormat.bytes(0), "0 B")
        XCTAssertEqual(ByteFormat.bytes(512), "512 B")
        XCTAssertEqual(ByteFormat.bytes(1023), "1023 B")
        XCTAssertEqual(ByteFormat.bytes(1024), "1.0 KiB")
        XCTAssertEqual(ByteFormat.bytes(1536), "1.5 KiB")
        XCTAssertEqual(ByteFormat.bytes(12 * 1024 * 1024), "12.0 MiB")
        XCTAssertEqual(ByteFormat.bytes(2 * 1024 * 1024 * 1024), "2.0 GiB")
        XCTAssertEqual(ByteFormat.bytes(1_288_490_189), "1.2 GiB")
        XCTAssertEqual(ByteFormat.bytes(UInt64.max), "16.0 EiB")
    }

    func testRate() {
        XCTAssertEqual(ByteFormat.rate(30 * 1024 * 1024), "30.0 MiB/s")
        XCTAssertEqual(ByteFormat.rate(0), "0 B/s")
    }

    func testDurations() {
        XCTAssertEqual(ByteFormat.duration(ms: 0), "<1 s")
        XCTAssertEqual(ByteFormat.duration(ms: 999), "<1 s")
        XCTAssertEqual(ByteFormat.duration(ms: 13_400), "13 s")
        XCTAssertEqual(ByteFormat.duration(ms: 45_000), "45 s")
        XCTAssertEqual(ByteFormat.duration(ms: 120_000), "2 min")
        XCTAssertEqual(ByteFormat.duration(ms: 124_000), "2 min 4 s")
        XCTAssertEqual(ByteFormat.duration(ms: 3_780_000), "1 h 3 min")
    }

    func testPercentIsClamped() {
        XCTAssertEqual(ByteFormat.percent(10, of: 0), 0)
        XCTAssertEqual(ByteFormat.percent(0, of: 100), 0)
        XCTAssertEqual(ByteFormat.percent(50, of: 100), 50)
        XCTAssertEqual(ByteFormat.percent(999, of: 1000), 99)
        XCTAssertEqual(ByteFormat.percent(150, of: 100), 100)
        XCTAssertEqual(ByteFormat.percent(UInt64.max / 2, of: UInt64.max), 50)
    }
}

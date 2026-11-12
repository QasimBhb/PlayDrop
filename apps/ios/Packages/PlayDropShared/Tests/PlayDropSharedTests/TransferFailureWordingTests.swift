// Failed transfers never show an HTTP status or a wire code.
import XCTest
@testable import PlayDropShared

final class TransferFailureWordingTests: XCTestCase {
    func testAKnownCodeWinsOverTheText() {
        XCTAssertEqual(
            TransferFailureWording.resolve(code: .noSpace, text: "507 Insufficient Storage no_space"),
            .known(.noSpace)
        )
        XCTAssertEqual(TransferFailureWording.resolve(code: .timedOut, text: nil), .known(.timedOut))
    }

    func testWireTextIsClassifiedWhenThereIsNoCode() {
        let cases: [(String, TransferFailureCode)] = [
            ("507 Insufficient Storage no_space", .noSpace),
            ("no_space", .noSpace),
            ("413 too_large", .tooLarge),
            ("HTTP 429 Too Many Requests", .rateLimited),
            ("403 declined", .declined),
        ]
        for (text, code) in cases {
            XCTAssertEqual(TransferFailureWording.resolve(code: nil, text: text), .known(code), text)
            XCTAssertEqual(TransferFailureWording.resolve(code: .other, text: text), .known(code), text)
        }
    }

    func testUnknownWireWordsBecomeTheGenericReason() {
        XCTAssertEqual(TransferFailureWording.resolve(code: nil, text: "500 internal_error"), .known(.other))
        XCTAssertEqual(TransferFailureWording.resolve(code: nil, text: "bad_manifest"), .known(.other))
        XCTAssertEqual(TransferFailureWording.resolve(code: nil, text: "status 502"), .known(.other))
    }

    func testPlainTextPassesThroughTrimmed() {
        XCTAssertEqual(
            TransferFailureWording.resolve(code: nil, text: "  failed integrity check from Laptop \n"),
            .plain("failed integrity check from Laptop")
        )
        XCTAssertEqual(
            TransferFailureWording.resolve(code: .other, text: "The disk was removed"),
            .plain("The disk was removed")
        )
    }

    func testNumbersThatAreNotStatusesAreKept() {
        XCTAssertEqual(
            TransferFailureWording.resolve(code: nil, text: "sent 1500 of 4096 bytes"),
            .plain("sent 1500 of 4096 bytes")
        )
        XCTAssertEqual(
            TransferFailureWording.resolve(code: nil, text: "file 12345 skipped"), .plain("file 12345 skipped")
        )
    }

    func testNothingToSayWithoutCodeOrText() {
        XCTAssertNil(TransferFailureWording.resolve(code: nil, text: nil))
        XCTAssertNil(TransferFailureWording.resolve(code: nil, text: "  "))
        XCTAssertEqual(TransferFailureWording.resolve(code: .other, text: ""), .known(.other))
    }
}

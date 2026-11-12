// How the remote input screen reacts to each way a session can end.
import XCTest
@testable import PlayDropShared

final class InputEndRulesTests: XCTestCase {
    func testLostConnectionResumesQuietlyEvenWithText() {
        XCTAssertEqual(InputEndRules.handling(code: .connectionLost, reason: "connection ended"), .resumeWhenBack)
    }

    func testEveryOtherCodeClosesWithItsOwnNotice() {
        let expected: [InputEndCode: InputEndNotice] = [
            .grantOff: .grantOff,
            .locked: .locked,
            .notAccepted: .notAccepted,
            .stoppedByUser: .stoppedByUser,
            .idleTimeout: .idleTimeout,
        ]
        for (code, notice) in expected {
            XCTAssertEqual(InputEndRules.handling(code: code, reason: "x"), .close(notice), "\(code)")
        }
    }

    func testEveryCodeIsHandled() {
        for code in InputEndCode.allCases where code != .connectionLost && code != .other {
            guard case .close = InputEndRules.handling(code: code, reason: "") else {
                return XCTFail("\(code) must close")
            }
        }
    }

    func testOtherKeepsTheComputersWordsTrimmedAndCut() {
        XCTAssertEqual(
            InputEndRules.handling(code: .other, reason: "  gone fishing \n"), .close(.other(reason: "gone fishing"))
        )
        XCTAssertEqual(InputEndRules.handling(code: .other, reason: "  \n"), .close(.other(reason: nil)))
        let long = String(repeating: "a", count: 500)
        guard case let .close(.other(reason: cut?)) = InputEndRules.handling(code: .other, reason: long) else {
            return XCTFail("expected an other notice")
        }
        XCTAssertEqual(cut.count, InputEndRules.maxReasonLength)
    }

    func testOnlyGrantOffAndNotAcceptedAreRefusals() {
        XCTAssertTrue(InputEndNotice.grantOff.isRefusal)
        XCTAssertTrue(InputEndNotice.notAccepted.isRefusal)
        for notice: InputEndNotice in [.locked, .stoppedByUser, .idleTimeout, .other(reason: nil)] {
            XCTAssertFalse(notice.isRefusal, "\(notice)")
        }
    }

    func testABareRefusalNeverReplacesAMoreSpecificEnd() {
        XCTAssertEqual(InputEndRules.afterRefusalAnswer(existing: nil), .grantOff)
        XCTAssertEqual(InputEndRules.afterRefusalAnswer(existing: .locked), .locked)
        XCTAssertEqual(InputEndRules.afterRefusalAnswer(existing: .notAccepted), .notAccepted)
    }
}

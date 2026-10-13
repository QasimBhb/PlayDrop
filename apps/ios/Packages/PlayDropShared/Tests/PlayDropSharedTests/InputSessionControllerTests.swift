// The remote input session rules: start once, stop once, stop on offline or background,
// no automatic restart after a refusal.
import XCTest
@testable import PlayDropShared

private struct Refused: Error {}
private struct Unreachable: Error {}

/// Records what the controller sent and fails on demand.
@MainActor
private final class Recorder {
    var sent: [Bool] = []
    var failNext: Error?

    func controller() -> InputSessionController {
        InputSessionController(
            send: { [unowned self] start in
                if let e = self.failNext {
                    self.failNext = nil
                    throw e
                }
                self.sent.append(start)
            },
            isRefusal: { $0 is Refused }
        )
    }
}

@MainActor
final class InputSessionControllerTests: XCTestCase {
    func testStartsOnceAndStopsOnce() async {
        let r = Recorder()
        let c = r.controller()
        let o1 = await c.open()
        XCTAssertEqual(o1, .sent)
        let o2 = await c.open()
        XCTAssertEqual(o2, .skipped)
        XCTAssertTrue(c.isActive)
        let o3 = await c.close()
        XCTAssertEqual(o3, .sent)
        let o4 = await c.close()
        XCTAssertEqual(o4, .skipped)
        XCTAssertEqual(r.sent, [true, false])
        XCTAssertEqual(c.state, .closed)
    }

    func testStopsWhenThePeerGoesOfflineAndResumes() async {
        let r = Recorder()
        let c = r.controller()
        await c.open()
        let o1 = await c.suspend()
        XCTAssertEqual(o1, .sent)
        let o2 = await c.suspend()
        XCTAssertEqual(o2, .skipped, "a second offline sends nothing")
        XCTAssertFalse(c.isActive)
        let o3 = await c.resume()
        XCTAssertEqual(o3, .sent)
        let o4 = await c.close()
        XCTAssertEqual(o4, .sent)
        XCTAssertEqual(r.sent, [true, false, true, false])
    }

    func testCloseAfterSuspendSendsNoSecondStop() async {
        let r = Recorder()
        let c = r.controller()
        await c.open()
        await c.suspend()
        let o1 = await c.close()
        XCTAssertEqual(o1, .skipped)
        XCTAssertEqual(r.sent, [true, false])
        let o2 = await c.resume()
        XCTAssertEqual(o2, .skipped, "a closed screen never restarts")
    }

    func testNoRestartAfterARefusal() async {
        let r = Recorder()
        let c = r.controller()
        r.failNext = Refused()
        let o1 = await c.open()
        XCTAssertEqual(o1, .refused)
        XCTAssertEqual(c.state, .refused)
        let o2 = await c.resume()
        XCTAssertEqual(o2, .skipped)
        let o3 = await c.open()
        XCTAssertEqual(o3, .skipped)
        let o4 = await c.close()
        XCTAssertEqual(o4, .skipped, "nothing to stop after a refusal")
        XCTAssertEqual(r.sent, [])
    }

    func testRefusalEventEndsTheSessionWithoutSending() async {
        let r = Recorder()
        let c = r.controller()
        await c.open()
        c.refused()
        XCTAssertFalse(c.isActive)
        await c.suspend()
        await c.resume()
        await c.close()
        XCTAssertEqual(r.sent, [true])
    }

    func testEndedByTheDesktopNeverRestarts() async {
        let r = Recorder()
        let c = r.controller()
        await c.open()
        c.endedByPeer()
        XCTAssertEqual(c.state, .endedByPeer)
        XCTAssertFalse(c.isActive)
        let o1 = await c.suspend()
        XCTAssertEqual(o1, .skipped)
        let o2 = await c.resume()
        XCTAssertEqual(o2, .skipped, "no automatic restart")
        let o3 = await c.open()
        XCTAssertEqual(o3, .skipped)
        let o4 = await c.close()
        XCTAssertEqual(o4, .skipped, "the desktop already closed it")
        XCTAssertEqual(r.sent, [true])
    }

    func testOtherFailureCanResume() async {
        let r = Recorder()
        let c = r.controller()
        r.failNext = Unreachable()
        let failed = await c.open()
        guard case .failed = failed else { return XCTFail("expected a failure") }
        XCTAssertEqual(c.state, .suspended)
        let o1 = await c.resume()
        XCTAssertEqual(o1, .sent)
        XCTAssertEqual(r.sent, [true])
    }

    func testBackgroundBeforeStartStartsOnResume() async {
        let r = Recorder()
        let c = r.controller()
        let o1 = await c.suspend()
        XCTAssertEqual(o1, .skipped)
        let o2 = await c.resume()
        XCTAssertEqual(o2, .sent)
        XCTAssertEqual(r.sent, [true])
    }

    func testCloseWhileStartingStillStops() async {
        let r = Recorder()
        let c = r.controller()
        async let opened = c.open()
        async let closed = c.close()
        _ = await (opened, closed)
        await c.settle()
        XCTAssertEqual(r.sent, [true, false], "the stop never overtakes the start")
        XCTAssertEqual(c.state, .closed)
    }
}

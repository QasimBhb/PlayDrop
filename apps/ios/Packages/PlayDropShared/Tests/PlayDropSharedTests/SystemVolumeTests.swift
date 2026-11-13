// The system volume entry is split off the player list, and the slider throttles while
// dragging, sends the final value on release and waits for the echo.
import PlayDropShared
import XCTest

final class SystemVolumeTests: XCTestCase {
    private struct P: Equatable { let id: String; let v: Int }

    func testReservedId() {
        XCTAssertEqual(SystemVolume.playerId, "@system")
        XCTAssertTrue(SystemVolume.isSystem("@system"))
        XCTAssertFalse(SystemVolume.isSystem("system"))
        XCTAssertFalse(SystemVolume.isSystem("@System"))
        XCTAssertFalse(SystemVolume.isSystem("spotify"))
    }

    func testSplitKeepsPlayerOrderAndDropsEverySystemEntry() {
        let list = [P(id: "a", v: 1), P(id: "@system", v: 40), P(id: "b", v: 2), P(id: "@system", v: 99)]
        let split = SystemVolume.split(list) { $0.id }
        XCTAssertEqual(split.system, P(id: "@system", v: 40), "the first one wins")
        XCTAssertEqual(split.players.map(\.id), ["a", "b"])
        XCTAssertEqual(SystemVolume.realPlayers(list) { $0.id }.map(\.id), ["a", "b"])
    }

    func testSplitWithoutSystemEntry() {
        let split = SystemVolume.split([P(id: "a", v: 1)]) { $0.id }
        XCTAssertNil(split.system)
        XCTAssertEqual(split.players.count, 1)
        let onlySystem = SystemVolume.split([P(id: "@system", v: 5)]) { $0.id }
        XCTAssertNotNil(onlySystem.system)
        XCTAssertTrue(onlySystem.players.isEmpty)
    }

    func testPercentClamps() {
        XCTAssertEqual(SystemVolume.percent(49.6), 50)
        XCTAssertEqual(SystemVolume.percent(-3.0), 0)
        XCTAssertEqual(SystemVolume.percent(140.0), 100)
        XCTAssertEqual(SystemVolume.percent(Double.nan), 0)
        XCTAssertEqual(SystemVolume.percent(nil as Int?), 0)
        XCTAssertEqual(SystemVolume.percent(250 as Int?), 100)
    }

    func testShowsReportedLevelWhenIdle() {
        var m = VolumeSliderModel()
        XCTAssertEqual(m.shown, 0)
        m.report(35)
        XCTAssertEqual(m.shown, 35)
        m.report(nil)
        XCTAssertEqual(m.shown, 0, "not reported reads as 0")
    }

    func testDragThrottlesToFivePerSecond() {
        var m = VolumeSliderModel()
        m.report(20)
        m.begin()
        XCTAssertEqual(m.shown, 20, "the finger starts at the shown level")
        XCTAssertEqual(m.drag(to: 25, now: 10.0), 25, "first move goes out at once")
        XCTAssertNil(m.drag(to: 30, now: 10.05), "within 0.2 s: held back")
        XCTAssertEqual(m.shown, 30, "the slider follows the finger")
        XCTAssertEqual(m.flushDelay(now: 10.05) ?? -1, 0.15, accuracy: 1e-9)
        XCTAssertNil(m.drag(to: 33, now: 10.1))
        XCTAssertNil(m.flush(now: 10.15), "still too soon")
        XCTAssertEqual(m.flush(now: 10.2), 33, "the latest held value, once")
        XCTAssertNil(m.flush(now: 10.5))
        XCTAssertNil(m.flushDelay(now: 10.5))
        XCTAssertEqual(m.drag(to: 40, now: 10.45), 40)

        // Count over one second of a fast drag.
        var sent = 0
        var t = 20.0
        var v = 0.0
        while t < 21.0 {
            if m.drag(to: v, now: t) != nil { sent += 1 }
            t += 0.016
            v += 1
        }
        XCTAssertLessThanOrEqual(sent, 5)
        XCTAssertGreaterThanOrEqual(sent, 4)
    }

    func testSameValueIsNotResent() {
        var m = VolumeSliderModel()
        m.begin()
        XCTAssertEqual(m.drag(to: 50, now: 1), 50)
        XCTAssertNil(m.drag(to: 50.2, now: 2), "rounds to the value already sent")
        XCTAssertNil(m.flushDelay(now: 2))
    }

    func testDragIgnoresIncomingStates() {
        var m = VolumeSliderModel()
        m.report(10)
        m.begin()
        _ = m.drag(to: 60, now: 1)
        m.report(15)
        XCTAssertEqual(m.shown, 60)
        XCTAssertEqual(m.reported, 15)
    }

    func testReleaseAlwaysSendsFinalValueAndHoldsUntilEcho() {
        var m = VolumeSliderModel()
        m.report(10)
        m.begin()
        XCTAssertEqual(m.drag(to: 50, now: 1.0), 50)
        XCTAssertNil(m.drag(to: 70, now: 1.05))
        XCTAssertEqual(m.release(now: 1.06), 70, "sent on release despite the throttle")
        XCTAssertFalse(m.dragging)
        XCTAssertNil(m.flushDelay(now: 1.06), "nothing left to flush")
        XCTAssertTrue(m.awaitingEcho)
        m.report(50)
        XCTAssertEqual(m.shown, 70, "an older echo does not pull it back")
        m.report(70)
        XCTAssertEqual(m.shown, 70)
        XCTAssertFalse(m.awaitingEcho, "the echo ends the wait")
        m.report(64)
        XCTAssertEqual(m.shown, 64, "follows the computer again")
    }

    func testReleaseSendsEvenWhenValueWasSent() {
        var m = VolumeSliderModel()
        m.begin()
        XCTAssertEqual(m.drag(to: 40, now: 1), 40)
        XCTAssertEqual(m.release(now: 3), 40)
    }

    func testHoldExpiresWithoutEcho() {
        var m = VolumeSliderModel(minInterval: 0.2, echoWait: 1.5)
        m.report(10)
        m.begin()
        _ = m.drag(to: 80, now: 5)
        _ = m.release(now: 5)
        m.expire(now: 6)
        XCTAssertEqual(m.shown, 80, "still waiting")
        m.expire(now: 6.6)
        XCTAssertEqual(m.shown, 10, "no echo: back to the reported level")
    }

    func testReleaseAtReportedValueDoesNotWait() {
        var m = VolumeSliderModel()
        m.report(30)
        m.begin()
        XCTAssertEqual(m.release(now: 1), 30)
        XCTAssertFalse(m.awaitingEcho)
    }

    func testDiscreteSetSendsAndHolds() {
        var m = VolumeSliderModel()
        m.report(30)
        XCTAssertEqual(m.set(to: 40, now: 1), 40)
        XCTAssertFalse(m.dragging)
        XCTAssertEqual(m.shown, 40)
        m.report(40)
        XCTAssertFalse(m.awaitingEcho)
    }

    func testNewDragDropsTheHold() {
        var m = VolumeSliderModel()
        m.report(10)
        m.begin()
        _ = m.drag(to: 90, now: 1)
        _ = m.release(now: 1)
        m.begin()
        XCTAssertFalse(m.awaitingEcho)
        XCTAssertEqual(m.shown, 90, "the finger starts where the slider was")
    }
}

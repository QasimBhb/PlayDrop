// Trackpad gestures: scaled deltas, the tap versus drag threshold, long-press drag, two-finger
// tap and scroll accumulation.
import XCTest
@testable import PlayDropShared

final class TrackpadGesturesTests: XCTestCase {
    private func p(_ x: Double, _ y: Double) -> TrackpadPoint { TrackpadPoint(x: x, y: y) }

    func testMovesAreScaledAndHeldBackUntilPastTheSlop() {
        var g = TrackpadGestures(sensitivity: 2)
        XCTAssertEqual(g.touches(1, at: p(100, 100), time: 0), [])
        XCTAssertEqual(g.touches(1, at: p(103, 100), time: 0.02), [], "inside the slop")
        XCTAssertEqual(g.touches(1, at: p(110, 100), time: 0.04), [.move(dx: 20, dy: 0)], "the whole distance once past it")
        XCTAssertEqual(g.touches(1, at: p(111, 98), time: 0.06), [.move(dx: 2, dy: -4)], "then every change")
        XCTAssertEqual(g.ended(time: 0.1), [], "a drag is no click")
    }

    func testQuickStillTouchIsALeftClick() {
        var g = TrackpadGestures()
        _ = g.touches(1, at: p(10, 10), time: 0)
        _ = g.touches(1, at: p(12, 11), time: 0.05)
        XCTAssertEqual(g.ended(time: 0.15), [.button(.left, .click)])
    }

    func testSlowStillTouchIsNoClick() {
        var g = TrackpadGestures()
        _ = g.touches(1, at: p(10, 10), time: 0)
        XCTAssertEqual(g.ended(time: 0.4), [])
    }

    func testLongPressDrags() {
        var g = TrackpadGestures(sensitivity: 1)
        _ = g.touches(1, at: p(10, 10), time: 0)
        XCTAssertEqual(g.tick(time: 0.3), [], "not yet")
        XCTAssertEqual(g.tick(time: 0.55), [.button(.left, .down)])
        XCTAssertTrue(g.isDragging)
        XCTAssertEqual(g.tick(time: 0.7), [], "pressed once")
        XCTAssertEqual(g.touches(1, at: p(30, 10), time: 0.8), [.move(dx: 20, dy: 0)])
        XCTAssertEqual(g.ended(time: 1.0), [.button(.left, .up)])
        XCTAssertFalse(g.isDragging)
    }

    func testMovingCancelsTheLongPress() {
        var g = TrackpadGestures()
        _ = g.touches(1, at: p(10, 10), time: 0)
        _ = g.touches(1, at: p(40, 10), time: 0.1)
        XCTAssertEqual(g.tick(time: 0.6), [])
    }

    func testCancelledDragReleasesTheButton() {
        var g = TrackpadGestures()
        _ = g.touches(1, at: p(10, 10), time: 0)
        _ = g.tick(time: 0.6)
        XCTAssertEqual(g.cancelled(), [.button(.left, .up)])
        XCTAssertEqual(g.cancelled(), [])
    }

    func testTwoFingerTapIsARightClick() {
        var g = TrackpadGestures()
        _ = g.touches(1, at: p(10, 10), time: 0)
        _ = g.touches(2, at: p(20, 10), time: 0.02)
        _ = g.touches(1, at: p(10, 10), time: 0.1)
        XCTAssertEqual(g.ended(time: 0.15), [.button(.right, .click)])
    }

    func testTwoFingerScrollAccumulatesWholeLines() {
        var g = TrackpadGestures()
        _ = g.touches(2, at: p(100, 300), time: 0)
        XCTAssertEqual(g.touches(2, at: p(100, 270), time: 0.05), [], "30 points: less than a line")
        XCTAssertEqual(g.touches(2, at: p(100, 250), time: 0.1), [.scroll(dx: 0, dy: 1)], "50 points up: one line down")
        XCTAssertEqual(g.touches(2, at: p(100, 200), time: 0.15), [.scroll(dx: 0, dy: 1)], "the remainder carried over")
        XCTAssertEqual(g.touches(2, at: p(100, 300), time: 0.2), [.scroll(dx: 0, dy: -2)], "back down: scroll up")
        XCTAssertEqual(g.ended(time: 0.25), [], "a scroll is no right click")
    }

    func testSensitivityIsClamped() {
        XCTAssertEqual(TrackpadGestures(sensitivity: 100).sensitivity, TrackpadGestures.sensitivityRange.upperBound)
        XCTAssertEqual(TrackpadGestures(sensitivity: 0).sensitivity, TrackpadGestures.sensitivityRange.lowerBound)
        XCTAssertEqual(TrackpadGestures.clampSensitivity(.nan), TrackpadGestures.defaultSensitivity)
        XCTAssertEqual(TrackpadGestures().sensitivity, 1.5)
    }
}

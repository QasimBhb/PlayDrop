// Media rows: position extrapolation, the "gone" rule and the player list merge.
import XCTest
@testable import PlayDropShared

final class MediaPlaybackTests: XCTestCase {
    func testExtrapolatesWhilePlaying() {
        let pos = MediaPlayback.position(positionMs: 61_200, positionAtMs: 1_000_000, durationMs: 215_000, playing: true, nowMs: 1_005_000)
        XCTAssertEqual(pos, 66_200)
    }

    func testHoldsWhilePaused() {
        let pos = MediaPlayback.position(positionMs: 61_200, positionAtMs: 1_000_000, durationMs: 215_000, playing: false, nowMs: 1_005_000)
        XCTAssertEqual(pos, 61_200)
    }

    func testClampsToTheDurationAndIgnoresAClockBehind() {
        XCTAssertEqual(MediaPlayback.position(positionMs: 210_000, positionAtMs: 0, durationMs: 215_000, playing: true, nowMs: 60_000), 215_000)
        XCTAssertEqual(MediaPlayback.position(positionMs: 5_000, positionAtMs: 10_000, durationMs: nil, playing: true, nowMs: 9_000), 5_000)
        XCTAssertNil(MediaPlayback.position(positionMs: nil, positionAtMs: 0, durationMs: 1, playing: true, nowMs: 5))
        XCTAssertEqual(MediaPlayback.position(positionMs: 1, positionAtMs: nil, durationMs: nil, playing: true, nowMs: 5), 1)
    }

    func testGoneRule() {
        XCTAssertTrue(MediaPlayback.isGone(playing: false, canIsEmpty: true))
        XCTAssertFalse(MediaPlayback.isGone(playing: false, canIsEmpty: false), "paused but controllable")
        XCTAssertFalse(MediaPlayback.isGone(playing: true, canIsEmpty: true), "playing with no controls")
    }

    func testMergeReplacesAppendsAndRemoves() {
        let id: (String) -> String = { String($0.prefix(1)) }
        var list = ["a1", "b1"]
        list = MediaPlayback.merge(list, with: "a2", gone: false, id: id)
        XCTAssertEqual(list, ["a2", "b1"])
        list = MediaPlayback.merge(list, with: "c1", gone: false, id: id)
        XCTAssertEqual(list, ["a2", "b1", "c1"])
        list = MediaPlayback.merge(list, with: "b2", gone: true, id: id)
        XCTAssertEqual(list, ["a2", "c1"])
        XCTAssertEqual(MediaPlayback.merge(list, with: "z1", gone: true, id: id), ["a2", "c1"], "unknown and gone: no row")
    }

    func testFractionSeekAndClock() {
        XCTAssertEqual(MediaPlayback.fraction(positionMs: 50, durationMs: 200), 0.25)
        XCTAssertNil(MediaPlayback.fraction(positionMs: 50, durationMs: 0))
        XCTAssertEqual(MediaPlayback.seekTarget(fraction: 0.5, durationMs: 215_000), 107_500)
        XCTAssertEqual(MediaPlayback.seekTarget(fraction: 2, durationMs: 1000), 1000)
        XCTAssertEqual(MediaPlayback.seekTarget(fraction: -1, durationMs: 1000), 0)
        XCTAssertEqual(MediaPlayback.clock(65_000), "1:05")
        XCTAssertEqual(MediaPlayback.clock(720_000), "12:00")
        XCTAssertEqual(MediaPlayback.clock(3_723_000), "1:02:03")
    }
}

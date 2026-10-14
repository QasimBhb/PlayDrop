// Refusal mapping: only `forbidden` answers to our input and media frames name a feature,
// and the throttle lets one message through per device and feature at a time.
import XCTest
@testable import PlayDropShared

final class PeerRefusalsTests: XCTestCase {
    func testForbiddenMediaAndInputMapToTheirFeature() {
        XCTAssertEqual(PeerRefusals.feature(frameType: "media", code: "forbidden"), .media)
        XCTAssertEqual(PeerRefusals.feature(frameType: "input", code: "forbidden"), .input)
    }

    func testOtherCodesFramesAndUnknownFramesAreNotAFeature() {
        XCTAssertNil(PeerRefusals.feature(frameType: "media", code: "not_paired"))
        XCTAssertNil(PeerRefusals.feature(frameType: "input", code: "bad_request"))
        XCTAssertNil(PeerRefusals.feature(frameType: "clipboard", code: "forbidden"))
        XCTAssertNil(PeerRefusals.feature(frameType: nil, code: "forbidden"))
        XCTAssertNil(PeerRefusals.feature(frameType: "Media", code: "forbidden"), "frame types are lower case on the wire")
    }

    func testThrottleShowsOncePerPeerAndFeatureWithinTheInterval() {
        var t = RefusalThrottle(interval: 5)
        XCTAssertTrue(t.shouldShow(peer: "a", feature: .media, now: 100))
        XCTAssertFalse(t.shouldShow(peer: "a", feature: .media, now: 101))
        XCTAssertFalse(t.shouldShow(peer: "a", feature: .media, now: 104.9))
        XCTAssertTrue(t.shouldShow(peer: "a", feature: .media, now: 105))
        XCTAssertTrue(t.shouldShow(peer: "b", feature: .media, now: 105.5), "another device has its own slot")
        XCTAssertTrue(t.shouldShow(peer: "a", feature: .input, now: 105.5), "another feature has its own slot")
    }

    func testThrottleRecoversFromAClockThatWentBack() {
        var t = RefusalThrottle(interval: 5)
        XCTAssertTrue(t.shouldShow(peer: "a", feature: .media, now: 100))
        XCTAssertTrue(t.shouldShow(peer: "a", feature: .media, now: 50))
        XCTAssertFalse(t.shouldShow(peer: "a", feature: .media, now: 51))
    }
}

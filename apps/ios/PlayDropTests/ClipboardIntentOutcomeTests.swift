// The one sentence the "Send Clipboard" Shortcut shows, ported from Android's ClipboardOutcome.
import XCTest
@testable import PlayDrop

final class ClipboardIntentOutcomeTests: XCTestCase {
    func testOutcomeRules() {
        XCTAssertEqual(ClipboardIntentOutcome.of(Fixtures.push(sentTo: ["a", "b"]), pairedCount: 2), .sent(2))
        XCTAssertEqual(ClipboardIntentOutcome.of(Fixtures.push(echo: true), pairedCount: 2), .alreadySent)
        XCTAssertEqual(ClipboardIntentOutcome.of(Fixtures.push(), pairedCount: 0), .noPairedDevice)
        XCTAssertEqual(ClipboardIntentOutcome.of(Fixtures.push(noGrant: 2), pairedCount: 2), .noGrant)
        XCTAssertEqual(ClipboardIntentOutcome.of(Fixtures.push(offline: 1, noGrant: 1), pairedCount: 2), .noDeviceOnline)
        XCTAssertEqual(ClipboardIntentOutcome.of(Fixtures.push(offline: 2), pairedCount: 2), .noDeviceOnline)
    }

    func testWording() {
        XCTAssertEqual(ClipboardIntentOutcome.sent(1).message, "Sent to 1 device")
        XCTAssertEqual(ClipboardIntentOutcome.sent(3).message, "Sent to 3 devices")
        XCTAssertEqual(ClipboardIntentOutcome.alreadySent.message, "Already sent")
        XCTAssertEqual(ClipboardIntentOutcome.noPairedDevice.message, "No paired devices. Open PlayDrop to add one.")
        XCTAssertEqual(
            ClipboardIntentOutcome.noGrant.message,
            "No device accepts this phone's clipboard. Turn it on in the device settings."
        )
        XCTAssertEqual(ClipboardIntentOutcome.noDeviceOnline.message, "No device online")
    }
}

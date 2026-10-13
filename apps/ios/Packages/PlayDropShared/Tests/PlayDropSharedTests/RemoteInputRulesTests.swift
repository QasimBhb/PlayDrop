// When the remote input link shows, which tabs a peer gets, and the keyboard tab's typing.
import XCTest
@testable import PlayDropShared

final class RemoteInputRulesTests: XCTestCase {
    func testLinkNeedsOnlineAndPointerOrKey() {
        XCTAssertEqual(RemoteInputAvailability.of(online: true, capabilities: ["ping", "input.pointer"]), .available)
        XCTAssertEqual(RemoteInputAvailability.of(online: true, capabilities: ["input.key"]), .available)
        XCTAssertEqual(RemoteInputAvailability.of(online: true, capabilities: ["input.presenter", "ping"]), .unsupported,
                       "presenter alone is not enough for the screen")
        XCTAssertEqual(RemoteInputAvailability.of(online: true, capabilities: []), .unsupported)
        XCTAssertEqual(RemoteInputAvailability.of(online: false, capabilities: ["input.pointer"]), .offline)
    }

    func testTabsFollowCapabilities() {
        XCTAssertEqual(RemoteInputTab.available(for: ["input.presenter", "input.pointer"]), [.trackpad, .presenter])
        XCTAssertEqual(RemoteInputTab.available(for: ["input.key"]), [.keyboard])
        XCTAssertTrue(peerAcceptsMediaControl(online: true, capabilities: ["media.control"]))
        XCTAssertFalse(peerAcceptsMediaControl(online: false, capabilities: ["media.control"]))
        XCTAssertFalse(peerAcceptsMediaControl(online: true, capabilities: ["media.state"]))
    }

    func testKeyNamesAreW3C() {
        XCTAssertEqual(RemoteKey.special, ["Escape", "Tab", "Enter", "Backspace", "ArrowLeft", "ArrowUp", "ArrowDown",
                                           "ArrowRight", "Home", "End", "PageUp", "PageDown", "Delete"])
        XCTAssertEqual(RemoteKey.media, ["AudioVolumeDown", "AudioVolumeMute", "AudioVolumeUp", "MediaPlayPause"])
    }

    func testTypingBecomesTextAndDeletingBecomesBackspace() {
        var k = KeyboardComposer()
        XCTAssertEqual(k.fieldChanged(to: KeyboardComposer.fieldReset), [])
        XCTAssertEqual(k.fieldChanged(to: KeyboardComposer.sentinel + "hi"), [.text("hi")])
        XCTAssertEqual(k.fieldChanged(to: ""), [.key("Backspace", mods: [])])
        XCTAssertEqual(k.fieldChanged(to: "x"), [.key("Backspace", mods: []), .text("x")], "sentinel replaced by a character")
    }

    func testModifiersApplyToTheNextKeyOnly() {
        var k = KeyboardComposer()
        k.toggle(.shift)
        k.toggle(.ctrl)
        XCTAssertEqual(k.latched, [.ctrl, .shift], "chip order, not tap order")
        XCTAssertEqual(k.fieldChanged(to: KeyboardComposer.sentinel + "cv"), [.key("c", mods: [.ctrl, .shift]), .text("v")])
        XCTAssertTrue(k.latched.isEmpty)
        k.toggle(.alt)
        XCTAssertEqual(k.press("Tab"), .key("Tab", mods: [.alt]))
        XCTAssertEqual(k.press("Tab"), .key("Tab", mods: []))
        k.toggle(.meta)
        k.toggle(.meta)
        XCTAssertFalse(k.isLatched(.meta), "a second tap turns the chip off")
        k.toggle(.ctrl)
        XCTAssertEqual(k.fieldChanged(to: ""), [.key("Backspace", mods: [.ctrl])])
    }
}

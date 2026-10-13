// Remote input on the app side: gesture and keyboard outputs become the right core frames,
// refusals are recognised, and media rows offer only what the player allows.
import PlayDropCore
import PlayDropShared
import XCTest
@testable import PlayDrop

final class InputMappingTests: XCTestCase {
    func testTrackpadOutputsMapToFrames() {
        XCTAssertEqual(InputMapping.event(for: .move(dx: 3, dy: -1.5)), .pointerMove(dx: 3, dy: -1.5))
        XCTAssertEqual(InputMapping.event(for: .button(.right, .click)), .pointerButton(button: .right, action: .click))
        XCTAssertEqual(InputMapping.event(for: .button(.left, .down)), .pointerButton(button: .left, action: .down))
        XCTAssertEqual(InputMapping.event(for: .scroll(dx: 0, dy: 2)), .scroll(dx: 0, dy: 2, pixels: false), "lines, not pixels")
    }

    func testKeyboardOutputsMapToFrames() {
        XCTAssertEqual(InputMapping.event(for: .text("hi")), .text(text: "hi"))
        XCTAssertEqual(
            InputMapping.event(for: .key("c", mods: [.ctrl, .shift])),
            .key(key: "c", mods: [.ctrl, .shift], action: .press)
        )
    }

    func testRefusalDetection() {
        XCTAssertTrue(RemoteInputModel.isInputRefusal(frameType: "input", code: "forbidden"))
        XCTAssertFalse(RemoteInputModel.isInputRefusal(frameType: "media", code: "forbidden"))
        XCTAssertFalse(RemoteInputModel.isInputRefusal(frameType: nil, code: "forbidden"))
        XCTAssertFalse(RemoteInputModel.isRefusal(CoreHostError.notRunning))
    }

    func testMediaRowOffersOnlyAllowedControls() {
        let state = Fixtures.media("spotify", playing: true, can: [.pause, .next, .seek], durationMs: 1000)
        let row = MediaRows.row(state, controllable: true)
        XCTAssertEqual(row.name, "spotify", "no display name: the player ID")
        XCTAssertEqual(row.toggleAction, .pause)
        XCTAssertTrue(row.canNext)
        XCTAssertFalse(row.canPrevious)
        XCTAssertTrue(row.canSeek)
        XCTAssertFalse(row.canVolume)

        let locked = MediaRows.row(state, controllable: false)
        XCTAssertNil(locked.toggleAction, "no media.control: no buttons at all")
        XCTAssertFalse(locked.canNext)
        XCTAssertFalse(locked.canSeek)
    }

    func testToggleActionPrefersThePlayersOwnToggle() {
        XCTAssertEqual(MediaRows.toggleAction(playing: true, can: [.playPause, .pause]), .playPause)
        XCTAssertEqual(MediaRows.toggleAction(playing: false, can: [.play, .pause]), .play)
        XCTAssertNil(MediaRows.toggleAction(playing: false, can: [.pause]))
    }
}

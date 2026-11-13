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

    func testMediaRowVolumeOnlyWhenThePlayerAllowsIt() {
        let withVolume = Fixtures.media("vlc", can: [.play, .volume], volume: 40)
        XCTAssertTrue(MediaRows.row(withVolume, controllable: true).canVolume)
        // A browser ignores volume, so the computer leaves it out of `can`.
        let browser = Fixtures.media("firefox", can: [.play], volume: 100)
        XCTAssertFalse(MediaRows.row(browser, controllable: true).canVolume)
        XCTAssertFalse(MediaRows.row(withVolume, controllable: false).canVolume)
    }

    func testSystemVolumeIsNeverAPlayerRow() {
        let list = [Fixtures.media("spotify", playing: true, can: [.pause]), Fixtures.systemVolume(35)]
        let split = SystemVolume.split(list) { $0.player }
        XCTAssertEqual(split.system?.volume, 35)
        XCTAssertEqual(split.players.map(\.player), ["spotify"])
        let alone = SystemVolume.split([Fixtures.systemVolume(0)]) { $0.player }
        XCTAssertNotNil(alone.system, "muted is still reported")
        XCTAssertTrue(alone.players.isEmpty, "no player: no Now playing rows")
        let entry = Fixtures.systemVolume(50)
        XCTAssertFalse(MediaPlayback.isGone(playing: entry.playing, canIsEmpty: entry.can.isEmpty))
    }

    func testToggleActionPrefersThePlayersOwnToggle() {
        XCTAssertEqual(MediaRows.toggleAction(playing: true, can: [.playPause, .pause]), .playPause)
        XCTAssertEqual(MediaRows.toggleAction(playing: false, can: [.play, .pause]), .play)
        XCTAssertNil(MediaRows.toggleAction(playing: false, can: [.pause]))
    }

    func testEveryCoreEndReasonMapsToItsCode() {
        let pairs: [(InputEndReason, InputEndCode)] = [
            (.grantOff, .grantOff), (.locked, .locked), (.notAccepted, .notAccepted),
            (.stoppedByUser, .stoppedByUser), (.idleTimeout, .idleTimeout),
            (.connectionLost, .connectionLost), (.other, .other),
        ]
        for (reason, code) in pairs {
            XCTAssertEqual(InputEndCode(reason), code, "\(reason)")
        }
        XCTAssertEqual(pairs.count, InputEndCode.allCases.count, "every package code has a core reason")
    }

    func testEveryEndNoticeHasASentenceNamingTheComputerWhereItHelps() {
        let notices: [InputEndNotice] = [
            .grantOff, .locked, .notAccepted, .stoppedByUser, .idleTimeout, .other(reason: "gone"), .other(reason: nil),
        ]
        for notice in notices {
            XCTAssertFalse(L10n.Input.endNotice(notice, label: "Desk").isEmpty, "\(notice)")
        }
        XCTAssertTrue(L10n.Input.endNotice(.grantOff, label: "Desk").contains("Desk"))
        XCTAssertTrue(L10n.Input.endNotice(.locked, label: "Desk").contains("Desk is locked"))
        XCTAssertTrue(L10n.Input.endNotice(.other(reason: "gone"), label: "Desk").contains("gone"))
    }
}

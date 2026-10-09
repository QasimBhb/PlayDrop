// Clipboard echo window, pairing URI recognition and peer fallback labels.
import Foundation
import XCTest
@testable import PlayDropShared

final class SmallTypesTests: XCTestCase {
    func testClipboardEchoWindow() {
        let t0 = Date(timeIntervalSince1970: 1_791_000_000)
        var echo = ClipboardEcho()
        XCTAssertEqual(echo.window, 10)
        XCTAssertFalse(echo.isEcho("hello", at: t0), "nothing applied yet")

        echo.remember(appliedText: "hello", at: t0)
        XCTAssertTrue(echo.isEcho("hello", at: t0))
        XCTAssertTrue(echo.isEcho("hello", at: t0.addingTimeInterval(10)))
        XCTAssertFalse(echo.isEcho("hello", at: t0.addingTimeInterval(10.5)), "outside the window")
        XCTAssertFalse(echo.isEcho("hello", at: t0.addingTimeInterval(-1)), "clock went backwards")
        XCTAssertFalse(echo.isEcho("Hello", at: t0), "different text")

        echo.remember(appliedText: "second", at: t0.addingTimeInterval(20))
        XCTAssertFalse(echo.isEcho("hello", at: t0.addingTimeInterval(21)), "only the last clip counts")
        XCTAssertTrue(echo.isEcho("second", at: t0.addingTimeInterval(21)))

        var short = ClipboardEcho(window: 1)
        short.remember(appliedText: "x", at: t0)
        XCTAssertFalse(short.isEcho("x", at: t0.addingTimeInterval(2)))
        XCTAssertEqual(ClipboardEcho.maxBytes, 1_048_576)
    }

    func testPairingUri() {
        XCTAssertTrue(PairingUri.isPairingUri(
            "playdrop://pair?id=3f9a&fp=a81c&addr=192.168.1.20:42187&addr=[fd00::20]:42187&secret=Zq0t&exp=1791460920"))
        XCTAssertTrue(PairingUri.isPairingUri("  PlayDrop://PAIR?id=1 \n"))
        XCTAssertFalse(PairingUri.isPairingUri("playdrop://pair?"))
        XCTAssertFalse(PairingUri.isPairingUri("playdrop://pair"))
        XCTAssertFalse(PairingUri.isPairingUri("playdrop://open?x=1"))
        XCTAssertFalse(PairingUri.isPairingUri("playdrop://pairing?x=1"))
        XCTAssertFalse(PairingUri.isPairingUri("https://example.com/playdrop://pair?id=1"))
        XCTAssertFalse(PairingUri.isPairingUri(""))
    }

    func testPeerLabelFallback() {
        let id = String(repeating: "a1", count: 32)
        XCTAssertEqual(PeerLabel.fallback(deviceId: id, shortId: "K3QD-7F2M-XA9P-41BC"), "K3QD-7F2M-XA9P-41BC")
        XCTAssertEqual(PeerLabel.fallback(deviceId: id, shortId: nil), "a1a1a1a1")
        XCTAssertEqual(PeerLabel.fallback(deviceId: id, shortId: ""), "a1a1a1a1")
        XCTAssertEqual(PeerLabel.fallback(deviceId: "abc", shortId: nil), "abc")
    }

    func testIdentifiersAreStable() {
        XCTAssertEqual(PlayDropIdentifiers.defaultPort, 42187)
        XCTAssertEqual(PlayDropIdentifiers.outgoingQueueFolderName, "outgoing")
        XCTAssertEqual(PairingUri.prefix, "playdrop://pair?")
    }
}

// The settings contract between the app and the share extension: key strings, default-peer
// normalisation and the peer directory codec.
import Foundation
import XCTest
@testable import PlayDropShared

final class SharedSettingsTests: XCTestCase {
    func testKeysAreFixedAndDistinct() {
        XCTAssertEqual(SharedSettingsKeys.all.count, 8)
        XCTAssertEqual(Set(SharedSettingsKeys.all).count, SharedSettingsKeys.all.count)
        // On-disk format: these strings must never change silently.
        XCTAssertEqual(SharedSettingsKeys.defaultPeer, "defaultPeer")
        XCTAssertEqual(SharedSettingsKeys.peerDirectory, "peerDirectory")
        XCTAssertEqual(SharedSettingsKeys.onboardingDone, "onboardingDone")
    }

    func testNormalizedPeer() {
        XCTAssertNil(SharedSettingsKeys.normalizedPeer(nil))
        XCTAssertNil(SharedSettingsKeys.normalizedPeer(""))
        XCTAssertNil(SharedSettingsKeys.normalizedPeer("  \n"))
        XCTAssertEqual(SharedSettingsKeys.normalizedPeer(" ab12 "), "ab12")
    }

    func testDisplayLabel() {
        XCTAssertEqual(SharedPeerEntry(deviceId: "a", label: "Desk", shortId: "K7Q2").displayLabel, "Desk (K7Q2)")
        XCTAssertEqual(SharedPeerEntry(deviceId: "a", label: "  ", shortId: "K7Q2").displayLabel, "K7Q2")
        XCTAssertEqual(SharedPeerEntry(deviceId: "a", label: "K7Q2", shortId: "K7Q2").displayLabel, "K7Q2")
    }

    func testCodecRoundTrip() throws {
        let entries = [
            SharedPeerEntry(deviceId: "aa11", label: "Desk", shortId: "K7Q2"),
            SharedPeerEntry(deviceId: "bb22", label: "Pixel \"7\"", shortId: "M3X9"),
        ]
        let data = try PeerDirectoryCodec.encode(entries)
        XCTAssertEqual(PeerDirectoryCodec.decode(data), entries)
        XCTAssertEqual(try PeerDirectoryCodec.encode(entries), data, "encoding is stable")
        XCTAssertEqual(PeerDirectoryCodec.decode(try PeerDirectoryCodec.encode([])), [])
    }

    func testCodecToleratesMissingOrBadData() {
        XCTAssertEqual(PeerDirectoryCodec.decode(nil), [])
        XCTAssertEqual(PeerDirectoryCodec.decode(Data("not json".utf8)), [])
        XCTAssertEqual(PeerDirectoryCodec.decode(Data(#"[{"deviceId":"a"}]"#.utf8)), [], "missing fields")
    }

    func testCodecReadsTheWireFieldNames() {
        // What an older app build wrote must still decode: the field names are the format.
        let json = #"[{"deviceId":"aa11","label":"Desk","shortId":"K7Q2"}]"#
        XCTAssertEqual(
            PeerDirectoryCodec.decode(Data(json.utf8)),
            [SharedPeerEntry(deviceId: "aa11", label: "Desk", shortId: "K7Q2")]
        )
    }
}

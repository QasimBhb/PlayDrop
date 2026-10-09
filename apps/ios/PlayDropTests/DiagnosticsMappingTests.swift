// The core's Diagnostics record mapped onto PlayDropShared's DiagSnapshot, field for field.
import PlayDropCore
import PlayDropShared
import XCTest
@testable import PlayDrop

final class DiagnosticsMappingTests: XCTestCase {
    func testSnapshotCarriesEveryField() {
        let d = Diagnostics(
            addr: "192.168.1.9:42187", deviceId: "dd", shortId: "SS", port: 42187, bindOk: true,
            interfaces: [Ipv4Interface(index: 3, addr: "192.168.1.9", netmask: "255.255.255.0")],
            multicast: MulticastStatus(enabled: true, joined: ["192.168.1.9"], joinFailed: ["10.0.0.1"], beaconRecently: false, lastBeaconSecsAgo: nil),
            mdns: MdnsStatus(enabled: true, registered: false, error: "denied", peerSeen: false, lastSeenSecsAgo: nil),
            peers: [
                PeerDiagnostic(
                    peer: "p", shortId: "PS", label: "Desk", online: false, lastKnown: ["192.168.1.4:42187"],
                    mdnsSeen: true, beaconSeen: false,
                    lastResult: DialFailure(path: .lastKnown, failure: "No route to host", at: 5)
                ),
                PeerDiagnostic(peer: "q", shortId: "QS", label: "Pix", online: true, lastKnown: [], mdnsSeen: false, beaconSeen: true, lastResult: nil),
            ]
        )
        let s = DiagnosticsMapping.snapshot(d)
        XCTAssertTrue(s.bindOk)
        XCTAssertEqual(s.port, 42187)
        XCTAssertEqual(s.interfaces, [DiagInterface(addr: "192.168.1.9", netmask: "255.255.255.0")])
        XCTAssertTrue(s.multicastEnabled)
        XCTAssertEqual(s.multicastJoinFailed, ["10.0.0.1"])
        XCTAssertFalse(s.beaconRecently)
        XCTAssertTrue(s.mdnsEnabled)
        XCTAssertFalse(s.mdnsRegistered)
        XCTAssertEqual(s.mdnsError, "denied")
        XCTAssertEqual(s.peers.count, 2)
        XCTAssertEqual(s.peers[0].label, "Desk")
        XCTAssertEqual(s.peers[0].lastFailurePath, "lastKnown")
        XCTAssertEqual(s.peers[0].lastFailure, "No route to host")
        XCTAssertTrue(s.peers[0].mdnsSeen)
        XCTAssertNil(s.peers[1].lastFailurePath)
        XCTAssertTrue(s.peers[1].online)
        XCTAssertTrue(s.peers[1].beaconSeen)
    }

    func testPathTokens() {
        XCTAssertEqual(DiagnosticsMapping.token(for: .mdns), "mdns")
        XCTAssertEqual(DiagnosticsMapping.token(for: .beacon), "beacon")
        XCTAssertEqual(DiagnosticsMapping.token(for: .lastKnown), "lastKnown")
        XCTAssertEqual(DiagnosticsMapping.pathName("something new"), "something new")
    }
}

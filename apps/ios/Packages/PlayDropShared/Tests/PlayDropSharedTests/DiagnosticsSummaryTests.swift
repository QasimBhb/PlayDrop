// The summary mirrors the Android LogicTest cases on the same sample snapshot.
import XCTest
@testable import PlayDropShared

final class DiagnosticsSummaryTests: XCTestCase {
    /// The Android `Samples.diagnostics`: an online laptop and an offline desktop whose last
    /// dial was refused.
    static let laptop = DiagPeer(
        label: "Pop laptop", online: true, lastKnown: ["192.168.1.20:42187", "[fe80::1]:42187"],
        beaconSeen: true, mdnsSeen: true, lastFailurePath: nil, lastFailure: nil)
    static let desktop = DiagPeer(
        label: "Office PC", online: false, lastKnown: ["192.168.1.31:42187"],
        beaconSeen: false, mdnsSeen: false, lastFailurePath: "lastKnown", lastFailure: "connection refused")
    static let sample = DiagSnapshot(
        bindOk: true, port: 42187, interfaces: [DiagInterface(addr: "192.168.1.40", netmask: "255.255.255.0")],
        multicastEnabled: true, multicastJoinFailed: [], beaconRecently: true,
        mdnsEnabled: true, mdnsRegistered: true, mdnsError: nil, peers: [laptop, desktop])

    /// `sample` with some fields replaced.
    static func with(
        bindOk: Bool? = nil, interfaces: [DiagInterface]? = nil, multicastEnabled: Bool? = nil,
        joinFailed: [String]? = nil, beaconRecently: Bool? = nil, mdnsEnabled: Bool? = nil,
        mdnsRegistered: Bool? = nil, mdnsError: String?? = nil, peers: [DiagPeer]? = nil
    ) -> DiagSnapshot {
        let s = sample
        return DiagSnapshot(
            bindOk: bindOk ?? s.bindOk, port: s.port, interfaces: interfaces ?? s.interfaces,
            multicastEnabled: multicastEnabled ?? s.multicastEnabled,
            multicastJoinFailed: joinFailed ?? s.multicastJoinFailed,
            beaconRecently: beaconRecently ?? s.beaconRecently, mdnsEnabled: mdnsEnabled ?? s.mdnsEnabled,
            mdnsRegistered: mdnsRegistered ?? s.mdnsRegistered, mdnsError: mdnsError ?? s.mdnsError,
            peers: peers ?? s.peers)
    }

    func testNamesTheFailedPath() {
        XCTAssertEqual(DiagnosticsSummary.of(Self.sample),
                       [.peerNotReached(peerLabel: "Office PC", path: "lastKnown", failure: "connection refused")])
    }

    func testAllFine() {
        XCTAssertEqual(DiagnosticsSummary.of(Self.with(peers: [Self.laptop])), [.allFine])
    }

    func testOrder() {
        let d = Self.with(bindOk: false, interfaces: [], joinFailed: ["192.168.1.40: no route"], mdnsError: "socket closed")
        let issues = DiagnosticsSummary.of(d)
        XCTAssertEqual(issues[0], .notListening(port: 42187))
        XCTAssertEqual(issues[1], .noInterface)
        XCTAssertEqual(issues[2], .multicastJoinFailed(addr: "192.168.1.40", error: "no route"))
        XCTAssertEqual(issues[3], .mdnsFailed(error: "socket closed"))
    }

    func testMulticastBlockedWhenAPeerIsOnlineWithoutBeacon() {
        let silent = DiagPeer(label: "Pop laptop", online: true, lastKnown: ["192.168.1.20:42187"],
                              beaconSeen: false, mdnsSeen: true, lastFailurePath: nil, lastFailure: nil)
        let d = Self.with(beaconRecently: false, peers: [silent])
        XCTAssertEqual(DiagnosticsSummary.of(d), [.multicastBlocked(ifaceAddr: "192.168.1.40", peerLabel: "Pop laptop")])
    }

    func testDiscoveryOff() {
        let d = Self.with(multicastEnabled: false, mdnsEnabled: false, mdnsError: .some("ignored"), peers: [Self.laptop])
        XCTAssertEqual(DiagnosticsSummary.of(d), [.discoveryOff])
    }

    func testOtherNetwork() {
        let away = DiagPeer(label: "Cafe laptop", online: false, lastKnown: ["10.0.0.9:42187"],
                            beaconSeen: false, mdnsSeen: false, lastFailurePath: "lastKnown", lastFailure: "timed out")
        XCTAssertEqual(DiagnosticsSummary.of(Self.with(peers: [away])), [.otherNetwork(peerLabel: "Cafe laptop")])
    }

    func testJoinFailureWithoutSeparator() {
        XCTAssertEqual(DiagnosticsSummary.splitJoinFailure("weird"), .multicastJoinFailed(addr: "weird", error: ""))
        XCTAssertEqual(DiagnosticsSummary.splitJoinFailure(": x"), .multicastJoinFailed(addr: ": x", error: ""))
    }
}

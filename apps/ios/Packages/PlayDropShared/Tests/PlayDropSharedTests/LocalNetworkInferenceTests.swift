// The Local Network permission guess from diagnostics.
import XCTest
@testable import PlayDropShared

final class LocalNetworkInferenceTests: XCTestCase {
    private typealias S = DiagnosticsSummaryTests

    private func offline(_ failure: String?) -> DiagPeer {
        DiagPeer(label: "Office PC", online: false, lastKnown: ["192.168.1.31:42187"], beaconSeen: false,
                 mdnsSeen: false, lastFailurePath: failure == nil ? nil : "lastKnown", lastFailure: failure)
    }

    func testOnlinePeerMeansGranted() {
        XCTAssertEqual(LocalNetworkInference.infer(from: S.sample, peerOnlineThisRun: false), .granted)
        let d = S.with(joinFailed: ["192.168.1.40: Operation not permitted (os error 1)"], peers: [offline("EPERM")])
        XCTAssertEqual(LocalNetworkInference.infer(from: d, peerOnlineThisRun: true), .granted)
    }

    func testPermissionShapedFailuresMeanLikelyDenied() {
        let join = S.with(joinFailed: ["192.168.1.40: Operation not permitted (os error 1)"], peers: [])
        guard case .likelyDenied(let reason) = LocalNetworkInference.infer(from: join, peerOnlineThisRun: false) else {
            return XCTFail("join failure")
        }
        XCTAssertTrue(reason.contains("not permitted"))

        for failure in ["No route to host (os error 65)", "EHOSTUNREACH", "connect: eperm", "os error 65"] {
            let d = S.with(peers: [offline(failure)])
            guard case .likelyDenied = LocalNetworkInference.infer(from: d, peerOnlineThisRun: false) else {
                return XCTFail(failure)
            }
        }
    }

    func testMdnsFailureMeansLikelyDenied() {
        let err = S.with(mdnsError: .some("registration failed"), peers: [])
        XCTAssertEqual(LocalNetworkInference.infer(from: err, peerOnlineThisRun: false),
                       .likelyDenied(reason: "mDNS failed: registration failed"))
        let unregistered = S.with(mdnsRegistered: false, peers: [])
        XCTAssertEqual(LocalNetworkInference.infer(from: unregistered, peerOnlineThisRun: false),
                       .likelyDenied(reason: "mDNS service not registered"))
        let off = S.with(mdnsEnabled: false, mdnsRegistered: false, mdnsError: .some("x"), peers: [])
        XCTAssertEqual(LocalNetworkInference.infer(from: off, peerOnlineThisRun: false), .unknown)
    }

    func testOrdinaryFailuresStayUnknown() {
        XCTAssertEqual(LocalNetworkInference.infer(from: S.with(peers: [offline("connection refused")]),
                                                   peerOnlineThisRun: false), .unknown)
        XCTAssertEqual(LocalNetworkInference.infer(from: S.with(peers: []), peerOnlineThisRun: false), .unknown)
    }

    func testNoInterfaceIsUnknown() {
        let d = S.with(interfaces: [], joinFailed: ["x: not permitted"], mdnsError: .some("x"), peers: [offline("EPERM")])
        XCTAssertEqual(LocalNetworkInference.infer(from: d, peerOnlineThisRun: false), .unknown)
    }
}

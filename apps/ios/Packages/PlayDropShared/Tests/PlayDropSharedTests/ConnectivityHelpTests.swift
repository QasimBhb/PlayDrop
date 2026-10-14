// The help is suggested for findings that point to a blocked or foreign network, not for
// local faults; free-team builds count as having no multicast; hotspot and USB tether
// interfaces are kept while cellular and tunnels are not.
import XCTest
@testable import PlayDropShared

final class ConnectivityHelpTests: XCTestCase {
    func testSuggestedForNetworkFindings() {
        XCTAssertTrue(ConnectivityHelp.isSuggested(for: [.multicastBlocked(ifaceAddr: "10.0.0.2", peerLabel: "Desk")]))
        XCTAssertTrue(ConnectivityHelp.isSuggested(for: [.multicastJoinFailed(addr: "239.255.42.187", error: "denied")]))
        XCTAssertTrue(ConnectivityHelp.isSuggested(for: [.otherNetwork(peerLabel: "Desk")]))
        XCTAssertTrue(ConnectivityHelp.isSuggested(for: [.peerNotReached(peerLabel: "Desk", path: "mdns", failure: "timeout")]))
        XCTAssertTrue(ConnectivityHelp.isSuggested(for: [.discoveryOff]))
        XCTAssertTrue(ConnectivityHelp.isSuggested(for: [.mdnsFailed(error: "x")]))
        XCTAssertTrue(ConnectivityHelp.isSuggested(for: [.noInterface, .otherNetwork(peerLabel: "Desk")]))
    }

    func testNotSuggestedForLocalFaultsOrHealthyReports() {
        XCTAssertFalse(ConnectivityHelp.isSuggested(for: [.allFine]))
        XCTAssertFalse(ConnectivityHelp.isSuggested(for: [.notListening(port: 42187)]))
        XCTAssertFalse(ConnectivityHelp.isSuggested(for: [.noInterface]))
        XCTAssertFalse(ConnectivityHelp.isSuggested(for: []))
    }

    func testMulticastAvailability() {
        XCTAssertTrue(ConnectivityHelp.multicastAvailable(profile: nil), "no profile means a store build")
        let free = EmbeddedProvisioningProfile(appGroups: [], applicationIdentifier: "T.XTL-1.org.playdrop.ios")
        XCTAssertFalse(ConnectivityHelp.multicastAvailable(profile: free))
        let granted = EmbeddedProvisioningProfile(
            appGroups: [], applicationIdentifier: nil, hasMulticastEntitlement: true
        )
        XCTAssertTrue(ConnectivityHelp.multicastAvailable(profile: granted))
    }

    func testHotspotUsbWifiAndEthernetInterfacesAreKept() {
        for name in ["en0", "en1", "en5", "bridge100", "bridge0"] {
            XCTAssertTrue(InterfaceKinds.carriesLanPeers(name), name)
        }
    }

    func testCellularAndTunnelsAreSkipped() {
        for name in ["pdp_ip0", "utun3", "ipsec0", "awdl0", "llw0"] {
            XCTAssertFalse(InterfaceKinds.carriesLanPeers(name), name)
        }
    }
}

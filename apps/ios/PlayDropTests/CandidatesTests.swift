// "Add device" candidates: one row per name and type, best address first.
import PlayDropCore
import XCTest
@testable import PlayDrop

final class CandidatesTests: XCTestCase {
    func testCollapseGroupsAndRanksAddresses() {
        let list = [
            AddDeviceCandidate(name: "Desk", deviceType: "desktop", addr: "[fd00::1]:42187"),
            AddDeviceCandidate(name: "Desk", deviceType: "desktop", addr: "192.168.1.4:42187"),
            AddDeviceCandidate(name: "Desk", deviceType: "desktop", addr: "8.8.8.8:42187"),
            AddDeviceCandidate(name: "Desk", deviceType: "desktop", addr: "192.168.1.4:42187"),
            AddDeviceCandidate(name: "Pix", deviceType: "phone", addr: "10.0.0.2:42187"),
            AddDeviceCandidate(name: "Desk", deviceType: "laptop", addr: "10.0.0.9:42187"),
        ]
        let rows = Candidates.collapse(list)
        XCTAssertEqual(rows.map(\.name), ["Desk", "Pix", "Desk"], "first-seen order, grouped by name and type")
        XCTAssertEqual(rows[0].addrs, ["192.168.1.4:42187", "8.8.8.8:42187", "[fd00::1]:42187"])
        XCTAssertEqual(rows[0].bestAddr, "192.168.1.4:42187")
        XCTAssertNotEqual(rows[0].id, rows[2].id)
    }

    func testRank() {
        XCTAssertEqual(Candidates.rank("10.1.2.3:1"), 0)
        XCTAssertEqual(Candidates.rank("172.20.1.1:1"), 0)
        XCTAssertEqual(Candidates.rank("172.40.1.1:1"), 1)
        XCTAssertEqual(Candidates.rank("192.168.0.1:1"), 0)
        XCTAssertEqual(Candidates.rank("8.8.8.8:1"), 1)
        XCTAssertEqual(Candidates.rank("[fd00::1]:1"), 2)
        XCTAssertEqual(Candidates.rank("not an address"), 2)
    }
}

// Device rows: sort order, asleep and battery mapping, and the one-tap send target.
import PlayDropCore
import XCTest
@testable import PlayDrop

final class DeviceRowsTests: XCTestCase {
    private let peers = [
        Fixtures.peer("a", label: "zed"),
        Fixtures.peer("b", label: "Alpha", online: true),
        Fixtures.peer("c", label: "beta"),
    ]

    func testSortsDefaultThenOnlineThenLabel() {
        let rows = DeviceRows.derive(peers: peers, status: [:], defaultPeer: "c")
        XCTAssertEqual(rows.map(\.deviceId), ["c", "b", "a"])
        XCTAssertTrue(rows[0].isDefault)
        XCTAssertEqual(DeviceRows.derive(peers: peers, status: [:], defaultPeer: nil).map(\.deviceId), ["b", "c", "a"])
    }

    func testStatusMapping() {
        let status = ["b": PeerStatus(batteryLevel: 50, charging: true, presence: "asleep"), "a": PeerStatus(presence: "asleep")]
        let rows = DeviceRows.derive(peers: peers, status: status, defaultPeer: nil)
        let b = rows.first { $0.deviceId == "b" }!
        XCTAssertTrue(b.asleep)
        XCTAssertEqual(b.batteryLevel, 50)
        XCTAssertTrue(b.charging)
        let a = rows.first { $0.deviceId == "a" }!
        XCTAssertFalse(a.asleep, "an offline device is never shown as asleep")
        XCTAssertNil(a.batteryLevel)
    }

    func testSendTarget() {
        XCTAssertEqual(DeviceRows.sendTarget(peers: peers, defaultPeer: "c"), "c")
        XCTAssertNil(DeviceRows.sendTarget(peers: peers, defaultPeer: "gone"), "several devices and no usable default: ask")
        XCTAssertEqual(DeviceRows.sendTarget(peers: [peers[0]], defaultPeer: nil), "a", "the only device")
        XCTAssertNil(DeviceRows.sendTarget(peers: [], defaultPeer: "a"))
    }
}

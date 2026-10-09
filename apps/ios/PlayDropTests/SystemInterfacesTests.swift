// The getifaddrs reader hands the core usable IPv4 interfaces only, never loopback.
import PlayDropCore
import PlayDropShared
import XCTest
@testable import PlayDrop

final class SystemInterfacesTests: XCTestCase {
    func testNoLoopbackAndWellFormed() throws {
        let interfaces = try SystemInterfaces().ipv4Interfaces()
        for iface in interfaces {
            XCTAssertFalse(iface.addr.hasPrefix("127."), "loopback \(iface.addr) must be skipped")
            XCTAssertGreaterThan(iface.index, 0)
            XCTAssertEqual(iface.addr.split(separator: ".").compactMap { UInt8($0) }.count, 4, iface.addr)
            XCTAssertEqual(iface.netmask.split(separator: ".").compactMap { UInt8($0) }.count, 4, iface.netmask)
            XCTAssertTrue(Addresses.sameSubnet(iface.addr, iface.addr, iface.netmask))
        }
    }
}

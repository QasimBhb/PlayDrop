// Typed addresses, IPv6 brackets and subnet math, mirroring the Android AddressesTest.
import XCTest
@testable import PlayDropShared

final class AddressesTests: XCTestCase {
    private func assertParse(_ s: String, _ host: String, _ port: UInt16, file: StaticString = #filePath, line: UInt = #line) {
        guard let r = Addresses.parseHostPort(s) else {
            return XCTFail("\(s) did not parse", file: file, line: line)
        }
        XCTAssertEqual(r.host, host, file: file, line: line)
        XCTAssertEqual(r.port, port, file: file, line: line)
    }

    func testIpv4WithAndWithoutPort() {
        assertParse("1.2.3.4:42187", "1.2.3.4", 42187)
        assertParse(" 192.168.1.20:5000 ", "192.168.1.20", 5000)
        assertParse("192.168.1.20", "192.168.1.20", PlayDropIdentifiers.defaultPort)
    }

    func testIpv6BracketsAndBareLiterals() {
        assertParse("[fd00::1]:42187", "fd00::1", 42187)
        assertParse("[fe80::1]:9", "fe80::1", 9)
        assertParse("[fe80::1]", "fe80::1", PlayDropIdentifiers.defaultPort)
        assertParse("fe80::1", "fe80::1", PlayDropIdentifiers.defaultPort)
        assertParse("::1", "::1", PlayDropIdentifiers.defaultPort)
        assertParse("[::ffff:192.168.1.2]:42187", "::ffff:192.168.1.2", 42187)
        assertParse("2001:db8:0:0:0:0:0:1", "2001:db8:0:0:0:0:0:1", PlayDropIdentifiers.defaultPort)
    }

    func testRejectsNonsense() {
        for bad in ["", "   ", "laptop.local", "laptop", "300.1.1.1", "1.2.3", "1.2.3.4.5", "1.2.3.4:0",
                    "1.2.3.4:70000", "1.2.3.4:", "1.2.3.4:abc", "[::1]x", "[::1]:", "[]", "[1.2.3.4]:5",
                    "1::2::3", "12345::1", "fe80::1%en0", "1:2:3:4:5:6:7", "1:2:3:4:5:6:7:8:9", "g::1",
                    "１.2.3.4", "[fd00::1]:42187:1"] {
            XCTAssertNil(Addresses.parseHostPort(bad), bad)
            XCTAssertFalse(Addresses.isValidHostPort(bad), bad)
        }
    }

    func testNormalized() {
        XCTAssertEqual(Addresses.normalized("192.168.1.20"), "192.168.1.20:42187")
        XCTAssertEqual(Addresses.normalized("fe80::1"), "[fe80::1]:42187")
        XCTAssertEqual(Addresses.normalized("[fe80::1]:9"), "[fe80::1]:9")
        XCTAssertNil(Addresses.normalized("nope"))
    }

    func testHostOf() {
        XCTAssertEqual(Addresses.host(of: "192.168.1.20:42187"), "192.168.1.20")
        XCTAssertEqual(Addresses.host(of: "[fe80::1]:42187"), "fe80::1")
        XCTAssertNil(Addresses.host(of: "garbage"))
    }

    func testSameSubnet() {
        XCTAssertTrue(Addresses.sameSubnet("192.168.1.31:42187", "192.168.1.40", "255.255.255.0"))
        XCTAssertFalse(Addresses.sameSubnet("192.168.2.31:42187", "192.168.1.40", "255.255.255.0"))
        XCTAssertTrue(Addresses.sameSubnet("192.168.2.31:42187", "192.168.1.40", "255.255.252.0"))
        XCTAssertTrue(Addresses.sameSubnet("10.9.8.7:1", "10.0.0.1", "255.0.0.0"))
        XCTAssertTrue(Addresses.sameSubnet("172.16.5.5", "172.16.200.1", "255.255.0.0"))
        XCTAssertFalse(Addresses.sameSubnet("[fe80::1]:42187", "192.168.1.40", "255.255.255.0"))
        XCTAssertFalse(Addresses.sameSubnet("192.168.1.31:42187", "bad", "255.255.255.0"))
        XCTAssertFalse(Addresses.sameSubnet("192.168.1.31:42187", "192.168.1.40", "bad"))
    }

    func testParseIpv4AndPort() {
        XCTAssertEqual(Addresses.parseIpv4("192.168.1.1"), 0xC0A8_0101)
        XCTAssertNil(Addresses.parseIpv4("192.168.01"))
        XCTAssertNil(Addresses.parseIpv4("1.2.3.0004"))
        XCTAssertEqual(Addresses.parsePort("65535"), 65535)
        XCTAssertNil(Addresses.parsePort("65536"))
        XCTAssertNil(Addresses.parsePort("-1"))
    }
}

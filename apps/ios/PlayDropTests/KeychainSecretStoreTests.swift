// The Keychain-backed secret store: round trip, layout, and rejection of tampered,
// truncated or foreign data. Each test uses its own Keychain account and deletes it after.
import PlayDropCore
import Security
import XCTest
@testable import PlayDrop

final class KeychainSecretStoreTests: XCTestCase {
    private let service = "org.playdrop.ios.tests"
    private var account = ""

    override func setUp() {
        super.setUp()
        account = "test-\(UUID().uuidString)"
    }

    override func tearDown() {
        deleteKey(account: account)
        super.tearDown()
    }

    /// Removes the test key item; a missing item is fine.
    private func deleteKey(account: String) {
        let query: [CFString: Any] = [
            kSecClass: kSecClassGenericPassword, kSecAttrService: service, kSecAttrAccount: account,
        ]
        _ = SecItemDelete(query as CFDictionary)
    }

    private func store() -> KeychainSecretStore { KeychainSecretStore(service: service, account: account) }

    func testRoundTripAndLayout() throws {
        let plain = Data("identity key bytes".utf8)
        let sealed = try store().seal(plain: plain)
        XCTAssertEqual(sealed.first, KeychainSecretStore.formatVersion)
        XCTAssertEqual(sealed.count, 1 + KeychainSecretStore.nonceLength + plain.count + KeychainSecretStore.tagLength)
        XCTAssertNotEqual(sealed.dropFirst(1 + KeychainSecretStore.nonceLength).prefix(plain.count), plain[...])
        // A second instance reads the same key from the Keychain.
        XCTAssertEqual(try store().open(sealed: sealed), plain)
        XCTAssertNotEqual(try store().seal(plain: plain), sealed, "a fresh nonce every time")
    }

    func testTamperedDataIsRejected() throws {
        var sealed = try store().seal(plain: Data("secret".utf8))
        sealed[sealed.count - 1] ^= 0x01
        XCTAssertThrowsError(try store().open(sealed: sealed)) { error in
            guard case PlayDropError.Other = error else { return XCTFail("expected Other, got \(error)") }
        }
    }

    func testTruncatedAndUnknownVersionAreRejected() throws {
        XCTAssertThrowsError(try store().open(sealed: Data([1, 2, 3])))
        var sealed = try store().seal(plain: Data("secret".utf8))
        sealed[sealed.startIndex] = 9
        XCTAssertThrowsError(try store().open(sealed: sealed))
    }

    func testAnotherKeyCannotOpen() throws {
        let sealed = try store().seal(plain: Data("secret".utf8))
        let otherAccount = "test-\(UUID().uuidString)"
        defer { deleteKey(account: otherAccount) }
        XCTAssertThrowsError(try KeychainSecretStore(service: service, account: otherAccount).open(sealed: sealed))
    }
}

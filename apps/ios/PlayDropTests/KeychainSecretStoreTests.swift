// The Keychain-backed secret store: round trip, layout, and rejection of tampered,
// truncated or foreign data. Each test uses its own Keychain account and deletes it after.
import PlayDropCore
import Security
import XCTest
@testable import PlayDrop

final class KeychainSecretStoreTests: XCTestCase {
    private let service = "org.playdrop.ios.tests"
    private var account = ""

    override func setUpWithError() throws {
        try super.setUpWithError()
        account = "test-\(UUID().uuidString)"
        try skipWithoutKeychainAccess()
    }

    /// The Keychain refuses every call with `errSecMissingEntitlement` (-34018) when the test
    /// host runs without a keychain entitlement, which is what an unsigned CI build on the
    /// simulator gets. Skip, loudly, instead of failing: the store itself is exercised on a
    /// signed device build. A probe write is made and removed again.
    private func skipWithoutKeychainAccess() throws {
        let probeAccount = "probe-\(UUID().uuidString)"
        let probe: [CFString: Any] = [
            kSecClass: kSecClassGenericPassword, kSecAttrService: service, kSecAttrAccount: probeAccount,
            kSecValueData: Data([1]),
        ]
        let status = SecItemAdd(probe as CFDictionary, nil)
        deleteKey(account: probeAccount)
        if status == errSecMissingEntitlement {
            throw XCTSkip("Keychain unavailable: the test host has no keychain entitlement (OSStatus -34018); run on a signed build")
        }
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

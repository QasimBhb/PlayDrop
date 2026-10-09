import CryptoKit
import Foundation
import PlayDropCore
import PlayDropShared
import Security

// Seals the core's identity key and pin store at rest (SR-28) with AES-256-GCM under a
// 32-byte key kept in the Keychain. The iOS counterpart of Android's KeystoreSecretStore.
//
// - The key is made once with `SymmetricKey(size: .bits256)` and stored as a generic
//   password (service = app bundle id, account "secret-store-key-v1") with
//   `kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly`: readable while the app runs after
//   the first unlock, never synced to iCloud (`kSecAttrSynchronizable` false) and never
//   restored onto another device, so a backup alone cannot open the sealed identity.
// - Sealed layout (the core wraps it in its own `PDSEALED1` header): one version byte (1),
//   then CryptoKit's combined box: 12 byte nonce, ciphertext, 16 byte tag. Same layout as
//   Android's SealFormat.
// - No `kSecAttrAccessGroup` is set: only the app process hosts the core, the share
//   extension never touches the identity, so the item lives in the app's default access
//   group (the first entry of `keychain-access-groups` in the entitlements). Sharing it with
//   the extension would only widen who can read it.
// - The core calls `seal` and `open` on its own background threads; the cached key is
//   guarded by a lock, so the class is safe to share (`@unchecked Sendable`).
// - Errors become `PlayDropError.Other` with a message naming the step and the OSStatus,
//   never key bytes or plaintext.

/// The Keychain-backed `SecretStore` handed to the core in `HostConfig.secretStore`.
final class KeychainSecretStore: SecretStore, @unchecked Sendable {
    /// Current sealed-layout version byte.
    static let formatVersion: UInt8 = 1
    /// AES-GCM nonce length in the combined box.
    static let nonceLength = 12
    /// AES-GCM tag length in the combined box.
    static let tagLength = 16
    /// Keychain account of the key; bump the suffix only with a migration.
    static let keyAccount = "secret-store-key-v1"

    private let service: String
    private let account: String
    private let lock = NSLock()
    private var cachedKey: SymmetricKey?

    /// A store whose key lives under `service` and `account` in the Keychain.
    init(service: String = PlayDropIdentifiers.appBundleId, account: String = KeychainSecretStore.keyAccount) {
        self.service = service
        self.account = account
    }

    /// Encrypts `plain` into `version || nonce || ciphertext || tag`.
    func seal(plain: Data) throws -> Data {
        let key = try loadKey()
        do {
            let box = try AES.GCM.seal(plain, using: key)
            guard let combined = box.combined else {
                throw PlayDropError.Other(detail: Self.failure("sealing produced no combined box"))
            }
            var out = Data(capacity: 1 + combined.count)
            out.append(Self.formatVersion)
            out.append(combined)
            return out
        } catch let e as PlayDropError {
            throw e
        } catch {
            Log.secrets.error("seal failed: \(Log.describe(error), privacy: .public)")
            throw PlayDropError.Other(detail: Self.failure("sealing failed"))
        }
    }

    /// Decrypts bytes produced by `seal`; a wrong key, a bad tag or a truncated input fails.
    func open(sealed: Data) throws -> Data {
        guard sealed.count >= 1 + Self.nonceLength + Self.tagLength else {
            throw PlayDropError.Other(detail: Self.failure("the sealed secret is truncated (\(sealed.count) bytes)"))
        }
        guard sealed[sealed.startIndex] == Self.formatVersion else {
            throw PlayDropError.Other(detail: Self.failure("unknown sealed secret version \(sealed[sealed.startIndex])"))
        }
        let key = try loadKey()
        do {
            let box = try AES.GCM.SealedBox(combined: Data(sealed.dropFirst()))
            return try AES.GCM.open(box, using: key)
        } catch {
            Log.secrets.error("open failed: \(Log.describe(error), privacy: .public)")
            throw PlayDropError.Other(detail: Self.failure("the sealed identity could not be opened with this device's key"))
        }
    }

    // MARK: - Key

    /// The cached key, else the Keychain's, else a new one stored now.
    private func loadKey() throws -> SymmetricKey {
        lock.lock()
        defer { lock.unlock() }
        if let key = cachedKey {
            return key
        }
        let key: SymmetricKey
        if let existing = try readKey() {
            key = existing
        } else {
            key = try createKey()
        }
        cachedKey = key
        return key
    }

    /// The attributes that identify the key item.
    private func baseQuery() -> [CFString: Any] {
        [
            kSecClass: kSecClassGenericPassword,
            kSecAttrService: service,
            kSecAttrAccount: account,
            kSecAttrSynchronizable: false,
        ]
    }

    /// Reads the key; nil when no item exists yet.
    private func readKey() throws -> SymmetricKey? {
        var query = baseQuery()
        query[kSecReturnData] = true
        query[kSecMatchLimit] = kSecMatchLimitOne
        var result: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &result)
        switch status {
        case errSecSuccess:
            guard let data = result as? Data, data.count == 32 else {
                throw PlayDropError.Other(detail: Self.failure("the stored key has the wrong size"))
            }
            return SymmetricKey(data: data)
        case errSecItemNotFound:
            return nil
        default:
            Log.secrets.error("Keychain read failed: OSStatus \(status, privacy: .public)")
            throw PlayDropError.Other(detail: Self.failure("reading the key failed (OSStatus \(status))"))
        }
    }

    /// Makes and stores a new key. A concurrent creator (another process start racing this
    /// one) is resolved by reading back whichever item won.
    private func createKey() throws -> SymmetricKey {
        let key = SymmetricKey(size: .bits256)
        let data = key.withUnsafeBytes { Data($0) }
        var attributes = baseQuery()
        attributes[kSecValueData] = data
        attributes[kSecAttrAccessible] = kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly
        let status = SecItemAdd(attributes as CFDictionary, nil)
        switch status {
        case errSecSuccess:
            Log.secrets.info("created the secret-store key")
            return key
        case errSecDuplicateItem:
            if let existing = try readKey() {
                return existing
            }
            throw PlayDropError.Other(detail: Self.failure("the key vanished while it was being created"))
        default:
            Log.secrets.error("Keychain add failed: OSStatus \(status, privacy: .public)")
            throw PlayDropError.Other(detail: Self.failure("storing the key failed (OSStatus \(status))"))
        }
    }

    /// The user-facing failure sentence around a technical `step`.
    private static func failure(_ step: String) -> String {
        "Secure key storage failed: \(step). If this persists, delete PlayDrop, reinstall it and pair again."
    }
}

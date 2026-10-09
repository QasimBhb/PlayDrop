// Linux type-check stub of CryptoKit: the AES-GCM and SymmetricKey shapes KeychainSecretStore uses.
import Foundation
public struct SymmetricKeySize: Sendable { public static let bits256 = SymmetricKeySize() }
public struct SymmetricKey: Sendable, ContiguousBytes {
    public init(size: SymmetricKeySize) {}
    public init<D: ContiguousBytes>(data: D) {}
    public func withUnsafeBytes<R>(_ body: (UnsafeRawBufferPointer) throws -> R) rethrows -> R { try body(UnsafeRawBufferPointer(start: nil, count: 0)) }
}
public enum AES {
    public enum GCM {
        public struct Nonce: Sendable {}
        public struct SealedBox: Sendable {
            public var combined: Data? { nil }
            public init<C: DataProtocol>(combined: C) throws {}
        }
        public static func seal<Plaintext: DataProtocol>(_ message: Plaintext, using key: SymmetricKey, nonce: Nonce? = nil) throws -> SealedBox { fatalError() }
        public static func open(_ sealedBox: SealedBox, using key: SymmetricKey) throws -> Data { Data() }
    }
}

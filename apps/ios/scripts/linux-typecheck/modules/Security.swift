// Linux type-check stub of Security: the Keychain calls KeychainSecretStore and its tests make.
import Foundation
public typealias OSStatus = Int32
public typealias CFTypeRef = AnyObject
public typealias CFDictionary = NSDictionary
public final class CFString: Hashable, @unchecked Sendable {
    public static func == (a: CFString, b: CFString) -> Bool { a === b }
    public func hash(into h: inout Hasher) { h.combine(ObjectIdentifier(self)) }
}
public let kSecClass = CFString()
public let kSecClassGenericPassword = CFString()
public let kSecAttrService = CFString()
public let kSecAttrAccount = CFString()
public let kSecAttrSynchronizable = CFString()
public let kSecAttrAccessible = CFString()
public let kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly = CFString()
public let kSecReturnData = CFString()
public let kSecMatchLimit = CFString()
public let kSecMatchLimitOne = CFString()
public let kSecValueData = CFString()
public let errSecSuccess: OSStatus = 0
public let errSecItemNotFound: OSStatus = -25300
public let errSecDuplicateItem: OSStatus = -25299
public func SecItemCopyMatching(_ query: CFDictionary, _ result: UnsafeMutablePointer<CFTypeRef?>?) -> OSStatus { 0 }
public func SecItemAdd(_ attributes: CFDictionary, _ result: UnsafeMutablePointer<CFTypeRef?>?) -> OSStatus { 0 }
public func SecItemDelete(_ query: CFDictionary) -> OSStatus { 0 }

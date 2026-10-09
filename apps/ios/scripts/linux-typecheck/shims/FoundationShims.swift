// Linux type-check shim: Apple-only Foundation API missing from swift-corelibs-foundation.
import Foundation
extension FileManager {
    func containerURL(forSecurityApplicationGroupIdentifier groupIdentifier: String) -> URL? { nil }
}

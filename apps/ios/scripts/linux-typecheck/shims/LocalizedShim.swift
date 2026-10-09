// Linux type-check shim: Linux Foundation lacks String(localized:). Mirrors the Apple overload set.
public struct LocalizationValueShim: ExpressibleByStringInterpolation {
    public struct StringInterpolation: StringInterpolationProtocol {
        public init(literalCapacity: Int, interpolationCount: Int) {}
        public mutating func appendLiteral(_ s: String) {}
        public mutating func appendInterpolation(_ s: String) {}
        public mutating func appendInterpolation(_ v: Int) {}
        public mutating func appendInterpolation(_ v: Int32) {}
        public mutating func appendInterpolation(_ v: Int64) {}
        public mutating func appendInterpolation(_ v: UInt) {}
        public mutating func appendInterpolation(_ v: UInt16) {}
        public mutating func appendInterpolation(_ v: UInt32) {}
        public mutating func appendInterpolation(_ v: UInt64) {}
        public mutating func appendInterpolation(_ v: Double) {}
    }
    public init(stringInterpolation: StringInterpolation) {}
    public init(stringLiteral: String) {}
}
extension String { init(localized: LocalizationValueShim) { self = "" } }
import Foundation
extension URL {
    func startAccessingSecurityScopedResource() -> Bool { true }
    func stopAccessingSecurityScopedResource() {}
}
extension String {
    init(localized key: StaticString, defaultValue: LocalizationValueShim, table: String? = nil, comment: StaticString? = nil) { self = "" }
}

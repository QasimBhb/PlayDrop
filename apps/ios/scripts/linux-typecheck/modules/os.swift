// Linux type-check stub of os: Logger with the privacy-annotated interpolations the app logs with.
public struct OSLogPrivacy: Sendable {
    public static let `public` = OSLogPrivacy()
    public static let `private` = OSLogPrivacy()
    public static let auto = OSLogPrivacy()
}
public struct OSLogInterpolation: StringInterpolationProtocol {
    public init(literalCapacity: Int, interpolationCount: Int) {}
    public mutating func appendLiteral(_ literal: String) {}
    public mutating func appendInterpolation(_ argumentString: @autoclosure @escaping () -> String, privacy: OSLogPrivacy = .auto) {}
    public mutating func appendInterpolation(_ number: @autoclosure @escaping () -> Int, privacy: OSLogPrivacy = .auto) {}
    public mutating func appendInterpolation(_ number: @autoclosure @escaping () -> Int32, privacy: OSLogPrivacy = .auto) {}
    public mutating func appendInterpolation(_ number: @autoclosure @escaping () -> UInt64, privacy: OSLogPrivacy = .auto) {}
    public mutating func appendInterpolation(_ boolean: @autoclosure @escaping () -> Bool, privacy: OSLogPrivacy = .auto) {}
}
public struct OSLogMessage: ExpressibleByStringInterpolation {
    public init(stringInterpolation: OSLogInterpolation) {}
    public init(stringLiteral value: String) {}
}
public struct Logger: @unchecked Sendable {
    public init(subsystem: String, category: String) {}
    public func debug(_ message: OSLogMessage) {}
    public func info(_ message: OSLogMessage) {}
    public func notice(_ message: OSLogMessage) {}
    public func error(_ message: OSLogMessage) {}
    public func fault(_ message: OSLogMessage) {}
}

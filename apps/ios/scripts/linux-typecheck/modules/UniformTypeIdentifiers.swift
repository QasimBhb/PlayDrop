// Linux type-check stub of UniformTypeIdentifiers with the real labels.
public struct UTType: Hashable, Sendable {
    public init?(filenameExtension: String) { return nil }
    public init?(filenameExtension: String, conformingTo supertype: UTType) { return nil }
    public init?(mimeType: String) { return nil }
    public init?(_ identifier: String) { return nil }
    public var identifier: String { "" }
    public var preferredMIMEType: String? { nil }
    public var preferredFilenameExtension: String? { nil }
    public func conforms(to type: UTType) -> Bool { false }
    public static let png = UTType(id: 0)
    public static let jpeg = UTType(id: 1)
    public static let image = UTType(id: 2)
    public static let utf8PlainText = UTType(id: 3)
    public static let movie = UTType(id: 4)
    public static let item = UTType(id: 5)
    public static let fileURL = UTType(id: 6)
    public static let plainText = UTType(id: 7)
    public static let url = UTType(id: 8)
    public static let text = UTType(id: 9)
    public static let data = UTType(id: 10)
    public static let heic = UTType(id: 11)
    private init(id: Int) {}
}

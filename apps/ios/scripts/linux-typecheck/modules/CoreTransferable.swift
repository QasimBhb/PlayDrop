// Linux type-check stub of CoreTransferable: Transferable and FileRepresentation with the real labels.
@_exported import Foundation
import UniformTypeIdentifiers
public protocol TransferRepresentation<Item>: Sendable { associatedtype Item: Transferable }
public protocol Transferable {
    associatedtype Representation: TransferRepresentation
    static var transferRepresentation: Representation { get }
}
public struct ReceivedTransferredFile: Sendable {
    public var file: URL { URL(fileURLWithPath: "/") }
    public var isOriginalFile: Bool { true }
}
public struct FileRepresentation<Item: Transferable>: TransferRepresentation {
    public init(importedContentType: UTType, shouldAttemptToOpenInPlace: Bool = false, importing: @escaping @Sendable (ReceivedTransferredFile) async throws -> Item) {}
}

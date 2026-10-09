// Linux type-check shim: NSItemProvider is Apple Foundation; same labels as the iOS 17 SDK.
import Foundation
open class NSItemProvider: @unchecked Sendable {
    public typealias CompletionHandler = ((any NSSecureCoding)?, (any Error)?) -> Void
    open var registeredTypeIdentifiers: [String] { [] }
    open var suggestedName: String?
    open func hasItemConformingToTypeIdentifier(_ typeIdentifier: String) -> Bool { false }
    open func loadFileRepresentation(forTypeIdentifier typeIdentifier: String, completionHandler: @escaping @Sendable (URL?, (any Error)?) -> Void) -> Progress { Progress(totalUnitCount: 0) }
    open func loadItem(forTypeIdentifier typeIdentifier: String, options: [AnyHashable: Any]? = nil, completionHandler: CompletionHandler? = nil) {}
}
open class NSExtensionItem: @unchecked Sendable {
    public init() {}
    open var attachments: [NSItemProvider]?
}

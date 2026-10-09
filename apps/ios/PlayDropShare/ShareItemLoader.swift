import Foundation
import PlayDropShared
import UniformTypeIdentifiers

// Turns one NSItemProvider from the share sheet into one queued item. Files (videos, photos,
// documents) are always streamed to disk through `loadFileRepresentation`, never loaded into
// memory, because the extension runs under a memory ceiling of roughly 120 MB and a 2 GB video
// must pass. Links and text selections are small and come back as values through `loadItem`;
// they are returned to the caller, which de-duplicates them and stores them as `.txt` items.

/// What the loader decided to do with a provider.
enum ShareItemPlan: Equatable {
    /// Stream the representation of this exact type identifier to a file and move it in.
    case file(typeIdentifier: String)
    /// Load a URL value (a web link, or a file URL the host app hands over).
    case url
    /// Load a plain-text value (a selection, or a `.txt` file URL).
    case text
}

/// The outcome for one provider.
enum ShareItemResult: Sendable {
    /// A file was moved or copied into the batch.
    case added(OutgoingItem)
    /// A small text value the caller should add with `BatchSink.addText`.
    case text(String, displayName: String)
    /// The provider failed; the reason is safe to show (it never contains the item's content).
    case failed(String)
    /// The provider offers nothing we can send.
    case unsupported
}

/// Loads share-sheet providers into a `BatchSink`; see the file comment.
@MainActor
enum ShareItemLoader {
    /// Picks how to load `provider`, preferring the original file of a video or photo over
    /// any converted form, then links, then plain text, then any other data.
    static func plan(for provider: NSItemProvider) -> ShareItemPlan? {
        let registered: [(id: String, type: UTType)] = provider.registeredTypeIdentifiers.compactMap { id in
            UTType(id).map { (id: id, type: $0) }
        }
        if let movie = registered.first(where: { $0.type.conforms(to: .movie) }) {
            return .file(typeIdentifier: movie.id)
        }
        if let image = registered.first(where: { $0.type.conforms(to: .image) }) {
            return .file(typeIdentifier: image.id)
        }
        if registered.contains(where: { $0.type.conforms(to: .url) && !$0.type.conforms(to: .fileURL) }) {
            return .url
        }
        if registered.contains(where: { $0.type.conforms(to: .plainText) }) {
            return .text
        }
        // Any other document. URL types conform to public.data too, so skip them here.
        if let data = registered.first(where: { $0.type.conforms(to: .data) && !$0.type.conforms(to: .url) }) {
            return .file(typeIdentifier: data.id)
        }
        if registered.contains(where: { $0.type.conforms(to: .fileURL) }) {
            return .url
        }
        if provider.hasItemConformingToTypeIdentifier(UTType.item.identifier) {
            return .file(typeIdentifier: UTType.item.identifier)
        }
        return nil
    }

    /// Loads `provider` and, for files, moves or copies the bytes into `sink` before the
    /// provider's callback returns.
    static func load(_ provider: NSItemProvider, into sink: BatchSink) async -> ShareItemResult {
        guard let plan = plan(for: provider) else { return .unsupported }
        let suggestedName = provider.suggestedName
        return await withCheckedContinuation { (continuation: CheckedContinuation<ShareItemResult, Never>) in
            switch plan {
            case .file(let typeIdentifier):
                // The callback is marked @Sendable so it never inherits the main actor: the
                // provider calls it on a background queue and the copy must run there.
                _ = provider.loadFileRepresentation(forTypeIdentifier: typeIdentifier) { @Sendable url, error in
                    guard let url else {
                        continuation.resume(returning: .failed(reason(error)))
                        return
                    }
                    continuation.resume(returning: moveIn(
                        url, typeIdentifier: typeIdentifier, suggestedName: suggestedName, sink: sink
                    ))
                }
            case .url:
                let typeIdentifier = provider.hasItemConformingToTypeIdentifier(UTType.url.identifier)
                    ? UTType.url.identifier : UTType.fileURL.identifier
                provider.loadItem(forTypeIdentifier: typeIdentifier, options: nil) { @Sendable item, error in
                    continuation.resume(returning: urlValue(item, error: error, suggestedName: suggestedName, sink: sink))
                }
            case .text:
                provider.loadItem(forTypeIdentifier: UTType.plainText.identifier, options: nil) { @Sendable item, error in
                    continuation.resume(returning: textValue(item, error: error, suggestedName: suggestedName, sink: sink))
                }
            }
        }
    }

    // MARK: - Callback bodies (run on the provider's queue)

    /// Moves the temporary representation file into the batch.
    nonisolated private static func moveIn(
        _ url: URL, typeIdentifier: String, suggestedName: String?, sink: BatchSink
    ) -> ShareItemResult {
        let name = displayName(suggested: suggestedName, file: url)
        do {
            let item = try sink.addFile(
                movingFrom: url, displayName: name, mimeType: mimeType(file: url, typeIdentifier: typeIdentifier)
            )
            return .added(item)
        } catch {
            return .failed(reason(error))
        }
    }

    /// A web link becomes a text item; a file URL is copied (the host app still owns it).
    nonisolated private static func urlValue(
        _ item: (any NSSecureCoding)?, error: (any Error)?, suggestedName: String?, sink: BatchSink
    ) -> ShareItemResult {
        let url: URL?
        if let value = item as? URL {
            url = value
        } else if let data = item as? Data {
            url = URL(dataRepresentation: data, relativeTo: nil)
        } else if let string = item as? String {
            url = URL(string: string)
        } else {
            url = nil
        }
        guard let url else { return .failed(reason(error)) }
        if url.isFileURL {
            return copyIn(url, suggestedName: suggestedName, sink: sink)
        }
        return .text(url.absoluteString, displayName: url.host ?? suggestedName ?? "Link")
    }

    /// A text value becomes a text item; a `.txt` file URL is copied.
    nonisolated private static func textValue(
        _ item: (any NSSecureCoding)?, error: (any Error)?, suggestedName: String?, sink: BatchSink
    ) -> ShareItemResult {
        let name = suggestedName ?? "Text"
        if let string = item as? String {
            return .text(string, displayName: name)
        }
        if let attributed = item as? NSAttributedString {
            return .text(attributed.string, displayName: name)
        }
        if let url = item as? URL, url.isFileURL {
            return copyIn(url, suggestedName: suggestedName, sink: sink)
        }
        if let data = item as? Data, let string = String(data: data, encoding: .utf8) {
            return .text(string, displayName: name)
        }
        return .failed(reason(error))
    }

    /// Copies a file URL handed over by the host app, inside its security scope when it has one.
    nonisolated private static func copyIn(_ url: URL, suggestedName: String?, sink: BatchSink) -> ShareItemResult {
        let scoped = url.startAccessingSecurityScopedResource()
        defer { if scoped { url.stopAccessingSecurityScopedResource() } }
        do {
            let item = try sink.addFile(
                copyingFrom: url,
                displayName: displayName(suggested: suggestedName, file: url),
                mimeType: mimeType(file: url, typeIdentifier: nil)
            )
            return .added(item)
        } catch {
            return .failed(reason(error))
        }
    }

    // MARK: - Naming

    /// The provider's suggested name (with the file's extension added when it lacks one),
    /// else the file's own name. The batch writer sanitises it (SR-17).
    nonisolated static func displayName(suggested: String?, file: URL) -> String {
        let fileName = file.lastPathComponent
        guard let suggested = suggested?.trimmingCharacters(in: .whitespacesAndNewlines), !suggested.isEmpty else {
            return fileName
        }
        let ext = file.pathExtension
        if ext.isEmpty || !(suggested as NSString).pathExtension.isEmpty {
            return suggested
        }
        return "\(suggested).\(ext)"
    }

    /// The MIME type from the file extension, else from the loaded type identifier.
    nonisolated static func mimeType(file: URL, typeIdentifier: String?) -> String? {
        if let fromExt = UTType(filenameExtension: file.pathExtension)?.preferredMIMEType {
            return fromExt
        }
        if let typeIdentifier, let fromType = UTType(typeIdentifier)?.preferredMIMEType {
            return fromType
        }
        return FileNames.mimeType(forExtension: file.pathExtension)
    }

    /// A short reason for a failed item.
    nonisolated private static func reason(_ error: (any Error)?) -> String {
        if let sinkError = error as? BatchSinkError {
            switch sinkError {
            case .noAppGroup: return "PlayDrop's shared folder is missing"
            case .closed: return "Cancelled"
            }
        }
        return error?.localizedDescription ?? "The app did not hand over this item"
    }
}

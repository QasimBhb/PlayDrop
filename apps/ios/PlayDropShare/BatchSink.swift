import Foundation
import PlayDropShared

// Thread-safe owner of the one outgoing batch a share builds. `OutgoingBatchWriter` is used
// on one thread at a time, but NSItemProvider calls back on arbitrary queues and its file
// callbacks must move the temporary file before they return. Every writer call therefore
// goes through one lock: callbacks are serialised (which also keeps disk and memory use to
// one copy at a time), and cancel waits for the copy in progress before deleting the batch.

/// Errors the sink reports to the share flow.
enum BatchSinkError: Error, Equatable {
    /// The App Group container is missing (entitlement or provisioning problem).
    case noAppGroup
    /// The user cancelled, or the batch was already committed or aborted.
    case closed
}

/// Owns the queue and the open batch writer; see the file comment.
final class BatchSink: @unchecked Sendable {
    // Guards every member below; held for the whole of each writer call.
    private let lock = NSLock()
    private let queue: OutgoingQueue
    private var writer: OutgoingBatchWriter?

    /// Opens the App Group queue and begins a staging batch.
    init() throws {
        guard let container = FileManager.default.containerURL(
            forSecurityApplicationGroupIdentifier: PlayDropIdentifiers.appGroup
        ) else {
            throw BatchSinkError.noAppGroup
        }
        queue = OutgoingQueue(root: OutgoingQueue.root(inContainer: container))
        writer = try queue.beginBatch()
    }

    /// Moves a temporary file (from `loadFileRepresentation`) into the batch. Call it inside
    /// the provider callback: the file is deleted once the callback returns.
    func addFile(movingFrom src: URL, displayName: String, mimeType: String?) throws -> OutgoingItem {
        try withWriter { try $0.addFile(movingFrom: src, displayName: displayName, mimeType: mimeType) }
    }

    /// Copies a file the extension may read but does not own (a file URL from `loadItem`).
    func addFile(copyingFrom src: URL, displayName: String, mimeType: String?) throws -> OutgoingItem {
        try withWriter { try $0.addFile(copyingFrom: src, displayName: displayName, mimeType: mimeType) }
    }

    /// Adds a text item (a shared link or a text selection) as a UTF-8 `.txt` file.
    func addText(_ text: String, displayName: String) throws -> OutgoingItem {
        try withWriter { try $0.addText(text, displayName: displayName) }
    }

    /// Sets the hex Device ID the app should send this batch to.
    func setPreferredPeer(_ peer: String?) throws {
        try withWriter { $0.setPreferredPeer(peer) }
    }

    /// Items added so far.
    var itemCount: Int {
        lock.lock()
        defer { lock.unlock() }
        return writer?.itemCount ?? 0
    }

    /// Commits the batch so the app can see it. Closes the sink.
    func commit() throws -> OutgoingBatch {
        lock.lock()
        defer { lock.unlock() }
        guard let open = writer else { throw BatchSinkError.closed }
        writer = nil
        do {
            return try queue.commit(open)
        } catch {
            queue.abort(open)
            throw error
        }
    }

    /// Deletes the staging batch. Idempotent; later adds throw `BatchSinkError.closed`.
    func abort() {
        lock.lock()
        defer { lock.unlock() }
        guard let open = writer else { return }
        writer = nil
        queue.abort(open)
    }

    private func withWriter<T>(_ body: (OutgoingBatchWriter) throws -> T) throws -> T {
        lock.lock()
        defer { lock.unlock() }
        guard let open = writer else { throw BatchSinkError.closed }
        return try body(open)
    }
}

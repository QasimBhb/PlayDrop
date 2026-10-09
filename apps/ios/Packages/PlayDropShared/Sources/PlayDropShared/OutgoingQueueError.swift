// Errors the outgoing queue and its batch writer throw. Kept apart so the app and the share
// extension can map each case to a message without importing the queue's internals.

/// Why an outgoing queue operation failed.
public enum OutgoingQueueError: Error, Equatable, Sendable {
    /// `commit` was called on a writer with no items.
    case emptyBatch
    /// The writer was already committed or aborted.
    case writerClosed
    /// The source is not a regular file (a directory, a missing path, a device).
    case notARegularFile(path: String)
    /// A batch id that is not 16 lowercase hex characters.
    case invalidBatchId(String)
    /// The text could not be encoded as UTF-8.
    case textNotEncodable
    /// A file operation failed; `detail` is the system's description (never file contents).
    case io(detail: String)
}

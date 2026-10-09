// One file waiting in the outgoing queue. The share extension writes it, the main app reads
// it back from the batch manifest and sends it; the struct is the manifest's item record.

/// A file inside a queued outgoing batch.
public struct OutgoingItem: Codable, Equatable, Hashable, Sendable {
    /// Name of the file inside the batch directory. Sanitised (SR-17) and unique inside the
    /// batch, case-insensitively, so it is also safe to send as the wire name.
    public let fileName: String
    /// What the user shared, for display only; may differ from `fileName`.
    public let displayName: String
    /// Size in bytes as written into the queue.
    public let size: UInt64
    /// MIME type when known.
    public let mimeType: String?
    /// Modification time in Unix milliseconds; 0 when unknown.
    public let modifiedMs: Int64

    /// Creates an item record.
    public init(fileName: String, displayName: String, size: UInt64, mimeType: String?, modifiedMs: Int64) {
        self.fileName = fileName
        self.displayName = displayName
        self.size = size
        self.mimeType = mimeType
        self.modifiedMs = modifiedMs
    }
}

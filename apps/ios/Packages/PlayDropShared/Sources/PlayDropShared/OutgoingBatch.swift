// One committed share: everything the user picked in a single share sheet, sent together as
// one transfer. Stored as `manifest.json` in the batch directory of the outgoing queue.

/// A committed group of outgoing items.
public struct OutgoingBatch: Codable, Equatable, Identifiable, Sendable {
    /// Batch id: 16 lowercase hex characters, also the batch directory's name.
    public let id: String
    /// When the batch was committed, in Unix milliseconds.
    public let createdAtMs: Int64
    /// The files, in the order the user shared them.
    public var items: [OutgoingItem]
    /// Optional note sent with the transfer.
    public var note: String?
    /// Hex Device ID the extension chose, if any; otherwise the app's default peer is used.
    public var preferredPeer: String?

    /// Sum of the item sizes.
    public var totalBytes: UInt64 {
        items.reduce(0) { $0 &+ $1.size }
    }

    /// Creates a batch record.
    public init(id: String, createdAtMs: Int64, items: [OutgoingItem], note: String?, preferredPeer: String?) {
        self.id = id
        self.createdAtMs = createdAtMs
        self.items = items
        self.note = note
        self.preferredPeer = preferredPeer
    }
}

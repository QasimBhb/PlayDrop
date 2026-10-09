// What a scan of the outgoing queue found: the batches that can be sent, and the batch
// directories that were skipped and why, so the app can show or log them (without names
// of files, which stay private) instead of losing them silently.

/// A batch directory the queue could not use.
public struct OutgoingQueueProblem: Equatable, Sendable {
    /// Why the directory was skipped.
    public enum Reason: Error, Equatable, Sendable {
        /// `manifest.json` is missing.
        case missingManifest
        /// `manifest.json` is not valid JSON for this layout.
        case unreadableManifest
        /// `manifest.json` was written by another layout version.
        case unsupportedVersion(Int)
        /// The manifest's id does not match its directory, or an item name is unsafe.
        case inconsistentManifest
        /// A listed file is missing or has a different size.
        case missingFile
    }

    /// The batch directory's name (a batch id).
    public let batchId: String
    /// Why it was skipped.
    public let reason: Reason

    /// Creates a problem record.
    public init(batchId: String, reason: Reason) {
        self.batchId = batchId
        self.reason = reason
    }
}

/// The result of `OutgoingQueue.listBatchesWithProblems()`.
public struct OutgoingQueueListing: Equatable, Sendable {
    /// Usable committed batches, oldest first.
    public let batches: [OutgoingBatch]
    /// Committed batch directories that were skipped.
    public let problems: [OutgoingQueueProblem]

    /// Creates a listing.
    public init(batches: [OutgoingBatch], problems: [OutgoingQueueProblem]) {
        self.batches = batches
        self.problems = problems
    }
}

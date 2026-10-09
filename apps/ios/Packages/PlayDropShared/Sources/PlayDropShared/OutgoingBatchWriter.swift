// Fills one batch in the outgoing queue's staging area. The share extension gets a writer
// from `OutgoingQueue.beginBatch()`, adds every shared item, then hands it back to
// `OutgoingQueue.commit(_:)`. Until that commit nothing is visible to the main app, so a
// share that is cancelled or killed half-way never produces a half batch.
import Foundation

/// Adds files and text to one staged batch. Not Sendable: use it from one thread, then pass
/// it to `OutgoingQueue.commit(_:)` or `OutgoingQueue.abort(_:)`.
public final class OutgoingBatchWriter {
    /// The batch's id; also the staging and final directory name.
    public let batchId: String

    /// `<root>/.staging/<batchId>`.
    let stagingDir: URL
    /// `<stagingDir>/files`, where the items go (kept apart from `manifest.json` so no
    /// shared name can collide with it).
    let filesDir: URL
    /// Items added so far, in order.
    private(set) var items: [OutgoingItem] = []
    /// The note set with `setNote`.
    private(set) var note: String?
    /// The peer set with `setPreferredPeer`.
    private(set) var preferredPeer: String?
    /// Lower-cased names in use, so names stay unique case-insensitively (SR-17 refuses
    /// case-insensitive duplicates in a manifest).
    private var usedNames: Set<String> = []
    /// True once committed or aborted.
    private(set) var isClosed = false

    /// Number of items added so far.
    public var itemCount: Int { items.count }

    /// Only `OutgoingQueue.beginBatch()` creates writers.
    init(batchId: String, stagingDir: URL) {
        self.batchId = batchId
        self.stagingDir = stagingDir
        self.filesDir = stagingDir.appendingPathComponent(OutgoingQueue.filesFolderName, isDirectory: true)
    }

    /// Copies the regular file at `src` into the batch with a streaming 1 MiB copy, never
    /// loading it into memory. The stored name is `displayName` (or the source's own name
    /// when that is blank) sanitised and made unique. `mimeType` falls back to the
    /// extension table. The source is left untouched.
    public func addFile(copyingFrom src: URL, displayName: String, mimeType: String?) throws -> OutgoingItem {
        try ensureOpen()
        let modifiedMs = try regularFileModifiedMs(src)
        let (name, dst) = reserveName(for: displayName, source: src)
        let size: UInt64
        do {
            size = try StreamingCopy.copy(from: src, to: dst)
        } catch {
            release(name)
            throw Self.wrap(error)
        }
        return record(name: name, displayName: displayName, size: size, mimeType: mimeType, modifiedMs: modifiedMs)
    }

    /// Moves the regular file at `src` into the batch: a rename when both are on one
    /// volume, otherwise a streaming copy followed by deleting `src`. Naming as in
    /// `addFile(copyingFrom:displayName:mimeType:)`.
    public func addFile(movingFrom src: URL, displayName: String, mimeType: String?) throws -> OutgoingItem {
        try ensureOpen()
        let modifiedMs = try regularFileModifiedMs(src)
        let (name, dst) = reserveName(for: displayName, source: src)
        let fm = FileManager.default
        let size: UInt64
        do {
            do {
                try fm.moveItem(at: src, to: dst)
                size = try Self.fileSize(dst)
            } catch {
                // Different volume or a rename the system refused: copy, then delete.
                if fm.fileExists(atPath: dst.path) { try? fm.removeItem(at: dst) }
                size = try StreamingCopy.copy(from: src, to: dst)
                try? fm.removeItem(at: src)
            }
        } catch {
            release(name)
            throw Self.wrap(error)
        }
        return record(name: name, displayName: displayName, size: size, mimeType: mimeType, modifiedMs: modifiedMs)
    }

    /// Stores `text` as a UTF-8 file named `<displayName>.txt` (the `.txt` is not doubled
    /// when `displayName` already ends in it). Text is never logged (SR-32).
    public func addText(_ text: String, displayName: String) throws -> OutgoingItem {
        try ensureOpen()
        var wanted = displayName.trimmingCharacters(in: .whitespacesAndNewlines)
        if wanted.isEmpty { wanted = "text" }
        if FileNames.fileExtension(of: wanted) != "txt" { wanted += ".txt" }
        let (name, dst) = reserveName(for: wanted, source: nil)
        let data = Data(text.utf8)
        do {
            try data.write(to: dst, options: [.withoutOverwriting])
        } catch {
            release(name)
            throw Self.wrap(error)
        }
        let nowMs = Int64((Date().timeIntervalSince1970 * 1000).rounded(.down))
        return record(name: name, displayName: wanted, size: UInt64(data.count), mimeType: "text/plain", modifiedMs: nowMs)
    }

    /// Sets or clears the note sent with the batch.
    public func setNote(_ note: String?) {
        self.note = note
    }

    /// Sets or clears the hex Device ID this batch should go to.
    public func setPreferredPeer(_ peer: String?) {
        self.preferredPeer = peer
    }

    /// Marks the writer finished after a commit or an abort.
    func close() {
        isClosed = true
    }

    /// Throws when the writer is finished.
    private func ensureOpen() throws {
        if isClosed { throw OutgoingQueueError.writerClosed }
    }

    /// Picks the stored name for an item and claims it.
    private func reserveName(for displayName: String, source: URL?) -> (String, URL) {
        var wanted = displayName.trimmingCharacters(in: .whitespacesAndNewlines)
        if wanted.isEmpty, let source { wanted = source.lastPathComponent }
        let clean = FileNames.sanitized(wanted)
        let name = FileNames.unique(clean) { usedNames.contains($0.lowercased()) }
        usedNames.insert(name.lowercased())
        return (name, filesDir.appendingPathComponent(name, isDirectory: false))
    }

    /// Frees a claimed name after a failed add.
    private func release(_ name: String) {
        usedNames.remove(name.lowercased())
    }

    /// Appends and returns the item record.
    private func record(name: String, displayName: String, size: UInt64, mimeType: String?, modifiedMs: Int64) -> OutgoingItem {
        let mime = mimeType ?? FileNames.fileExtension(of: name).flatMap(FileNames.mimeType(forExtension:))
        let item = OutgoingItem(fileName: name, displayName: displayName, size: size, mimeType: mime, modifiedMs: modifiedMs)
        items.append(item)
        return item
    }

    /// The modification time of `url` in Unix milliseconds (0 when unknown), after
    /// checking that it is a regular file.
    private func regularFileModifiedMs(_ url: URL) throws -> Int64 {
        let attrs: [FileAttributeKey: Any]
        do {
            attrs = try FileManager.default.attributesOfItem(atPath: url.path)
        } catch {
            throw OutgoingQueueError.notARegularFile(path: url.lastPathComponent)
        }
        guard (attrs[.type] as? FileAttributeType) == .typeRegular else {
            throw OutgoingQueueError.notARegularFile(path: url.lastPathComponent)
        }
        guard let date = attrs[.modificationDate] as? Date else { return 0 }
        return Int64((date.timeIntervalSince1970 * 1000).rounded(.down))
    }

    /// Size of the file at `url` in bytes.
    static func fileSize(_ url: URL) throws -> UInt64 {
        let attrs = try FileManager.default.attributesOfItem(atPath: url.path)
        return (attrs[.size] as? NSNumber)?.uint64Value ?? 0
    }

    /// Queue errors pass through; anything else becomes `.io` with its description.
    static func wrap(_ error: Error) -> OutgoingQueueError {
        if let e = error as? OutgoingQueueError { return e }
        return .io(detail: String(describing: error))
    }
}

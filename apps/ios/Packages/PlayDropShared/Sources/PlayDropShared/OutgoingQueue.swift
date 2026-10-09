// The share extension's hand-off to the main app: a folder in the App Group container. The
// extension cannot host the core (iOS gives it seconds and little memory), so it stages the
// shared items under `.staging/<id>/files/`, writes `manifest.json`, and renames the directory to
// `<id>/` in one step; the app lists committed batches, sends them and removes them. A
// rename within one volume is atomic, so the app never sees a half-written batch, and a
// crash leaves only a staging directory that `sweep` deletes later.
import Foundation

/// The outgoing queue rooted at `<App Group container>/outgoing`.
public struct OutgoingQueue: Sendable {
    /// Name of the staging folder under the root.
    static let stagingFolderName = ".staging"
    /// Prefix of a batch directory being deleted (moved into staging first so a half-deleted
    /// batch is never listed).
    static let removingPrefix = "removing-"
    /// Name of the folder inside a batch directory that holds the items.
    static let filesFolderName = "files"

    /// The queue's root directory.
    public let root: URL

    /// A queue at `root`; directories are created on first write.
    public init(root: URL) {
        self.root = root
    }

    /// The queue's root inside an App Group container.
    public static func root(inContainer container: URL) -> URL {
        container.appendingPathComponent(PlayDropIdentifiers.outgoingQueueFolderName, isDirectory: true)
    }

    /// `<root>/.staging`.
    var stagingRoot: URL {
        root.appendingPathComponent(Self.stagingFolderName, isDirectory: true)
    }

    /// Starts a batch: creates `<root>/.staging/<id>/files/` with a fresh random id.
    public func beginBatch() throws -> OutgoingBatchWriter {
        let fm = FileManager.default
        do {
            try fm.createDirectory(at: stagingRoot, withIntermediateDirectories: true)
        } catch {
            throw OutgoingBatchWriter.wrap(error)
        }
        // A clash among 2^64 ids is next to impossible; retry a few times rather than trust it.
        var lastError: Error = OutgoingQueueError.io(detail: "no free batch id")
        for _ in 0..<8 {
            let id = OutgoingBatchId.random()
            if fm.fileExists(atPath: batchDir(id).path) { continue }
            let dir = stagingRoot.appendingPathComponent(id, isDirectory: true)
            do {
                // Without intermediate directories this fails if `dir` already exists.
                try fm.createDirectory(at: dir, withIntermediateDirectories: false)
                let writer = OutgoingBatchWriter(batchId: id, stagingDir: dir)
                try fm.createDirectory(at: writer.filesDir, withIntermediateDirectories: false)
                return writer
            } catch {
                lastError = error
            }
        }
        throw OutgoingBatchWriter.wrap(lastError)
    }

    /// Writes `manifest.json` into the staging directory, then renames it to `<root>/<id>`
    /// in one atomic step. Throws `.emptyBatch` for a writer with no items (the writer stays
    /// open so the caller can still add items or abort) and `.writerClosed` when it was
    /// already committed or aborted.
    public func commit(_ writer: OutgoingBatchWriter) throws -> OutgoingBatch {
        if writer.isClosed { throw OutgoingQueueError.writerClosed }
        if writer.items.isEmpty { throw OutgoingQueueError.emptyBatch }
        let batch = OutgoingBatch(
            id: writer.batchId,
            createdAtMs: Self.unixMs(Date()),
            items: writer.items,
            note: writer.note,
            preferredPeer: writer.preferredPeer
        )
        do {
            let data = try OutgoingManifest.encode(batch)
            let manifestURL = writer.stagingDir.appendingPathComponent(OutgoingManifest.fileName, isDirectory: false)
            try data.write(to: manifestURL, options: [.atomic])
            try FileManager.default.moveItem(at: writer.stagingDir, to: batchDir(batch.id))
        } catch {
            throw OutgoingBatchWriter.wrap(error)
        }
        writer.close()
        return batch
    }

    /// Deletes the writer's staging directory and closes it. Safe to call twice; does
    /// nothing after a commit.
    public func abort(_ writer: OutgoingBatchWriter) {
        guard !writer.isClosed else { return }
        writer.close()
        try? FileManager.default.removeItem(at: writer.stagingDir)
    }

    /// Committed batches, oldest first. Batches whose manifest cannot be used are skipped;
    /// `listBatchesWithProblems()` says which and why.
    public func listBatches() throws -> [OutgoingBatch] {
        try listBatchesWithProblems().batches
    }

    /// Committed batches, oldest first, plus every batch directory that was skipped.
    /// A missing root is an empty queue.
    public func listBatchesWithProblems() throws -> OutgoingQueueListing {
        var batches: [OutgoingBatch] = []
        var problems: [OutgoingQueueProblem] = []
        for id in try committedIds() {
            switch loadBatch(id) {
            case .success(let batch): batches.append(batch)
            case .failure(let reason): problems.append(OutgoingQueueProblem(batchId: id, reason: reason))
            }
        }
        batches.sort { ($0.createdAtMs, $0.id) < ($1.createdAtMs, $1.id) }
        problems.sort { $0.batchId < $1.batchId }
        return OutgoingQueueListing(batches: batches, problems: problems)
    }

    /// Where `item` of `batch` is stored.
    public func fileURL(for item: OutgoingItem, in batch: OutgoingBatch) -> URL {
        filesDir(batch.id).appendingPathComponent(item.fileName, isDirectory: false)
    }

    /// Deletes a committed batch. The directory is first renamed into staging, so a
    /// half-finished delete never shows up as a batch. Removing a batch that is not there
    /// is not an error.
    public func remove(batchId: String) throws {
        guard OutgoingBatchId.isValid(batchId) else { throw OutgoingQueueError.invalidBatchId(batchId) }
        let fm = FileManager.default
        let dir = batchDir(batchId)
        guard fm.fileExists(atPath: dir.path) else { return }
        do {
            try fm.createDirectory(at: stagingRoot, withIntermediateDirectories: true)
            let doomed = stagingRoot.appendingPathComponent(
                "\(Self.removingPrefix)\(batchId)-\(OutgoingBatchId.random())", isDirectory: true)
            try fm.moveItem(at: dir, to: doomed)
            try fm.removeItem(at: doomed)
        } catch {
            throw OutgoingBatchWriter.wrap(error)
        }
    }

    /// Housekeeping, run by the app on launch: deletes staging directories untouched for
    /// `staleStaging` seconds (a share that crashed or was killed), leftovers of interrupted
    /// removals, committed batches older than `expiredBatches`, and unusable batch
    /// directories untouched for `staleStaging`. Returns how many entries it removed.
    public func sweep(now: Date = Date(), staleStaging: TimeInterval = 3600, expiredBatches: TimeInterval = 7 * 86400) throws -> Int {
        let fm = FileManager.default
        var removed = 0
        let stagingCutoff = now.addingTimeInterval(-staleStaging)

        for name in try entries(of: stagingRoot) {
            let url = stagingRoot.appendingPathComponent(name)
            let leftover = name.hasPrefix(Self.removingPrefix)
            // Adding items touches the files folder, not the staging directory itself.
            let filesURL = url.appendingPathComponent(Self.filesFolderName, isDirectory: true)
            let touched = [Self.modificationDate(url), Self.modificationDate(filesURL)].compactMap { $0 }.max()
            let stale = (touched ?? .distantPast) < stagingCutoff
            if leftover || stale, (try? fm.removeItem(at: url)) != nil { removed += 1 }
        }

        let expiryMs = Self.unixMs(now.addingTimeInterval(-expiredBatches))
        for id in try committedIds() {
            let expired: Bool
            switch loadBatch(id) {
            case .success(let batch):
                expired = batch.createdAtMs < expiryMs
            case .failure:
                expired = (Self.modificationDate(batchDir(id)) ?? .distantPast) < stagingCutoff
            }
            if expired, (try? remove(batchId: id)) != nil { removed += 1 }
        }
        return removed
    }

    /// `<root>/<id>`.
    private func batchDir(_ id: String) -> URL {
        root.appendingPathComponent(id, isDirectory: true)
    }

    /// `<root>/<id>/files`.
    private func filesDir(_ id: String) -> URL {
        batchDir(id).appendingPathComponent(Self.filesFolderName, isDirectory: true)
    }

    /// Names of committed batch directories (valid ids only; anything else is ignored).
    private func committedIds() throws -> [String] {
        try entries(of: root).filter { name in
            guard OutgoingBatchId.isValid(name) else { return false }
            var isDir: ObjCBool = false
            return FileManager.default.fileExists(atPath: batchDir(name).path, isDirectory: &isDir) && isDir.boolValue
        }
    }

    /// Entry names of `dir`; empty when it does not exist.
    private func entries(of dir: URL) throws -> [String] {
        let fm = FileManager.default
        guard fm.fileExists(atPath: dir.path) else { return [] }
        do {
            return try fm.contentsOfDirectory(atPath: dir.path)
        } catch {
            throw OutgoingBatchWriter.wrap(error)
        }
    }

    /// Reads and checks one committed batch: the manifest must decode, name its own
    /// directory, list only safe and distinct file names, and every file must exist with
    /// the recorded size.
    private func loadBatch(_ id: String) -> Result<OutgoingBatch, OutgoingQueueProblem.Reason> {
        let dir = batchDir(id)
        let manifestURL = dir.appendingPathComponent(OutgoingManifest.fileName, isDirectory: false)
        guard let data = FileManager.default.contents(atPath: manifestURL.path) else {
            return .failure(.missingManifest)
        }
        let batch: OutgoingBatch
        switch OutgoingManifest.decode(data) {
        case .success(let b): batch = b
        case .failure(.unreadable): return .failure(.unreadableManifest)
        case .failure(.unsupportedVersion(let v)): return .failure(.unsupportedVersion(v))
        }
        guard batch.id == id, !batch.items.isEmpty else { return .failure(.inconsistentManifest) }
        var seen: Set<String> = []
        for item in batch.items {
            let name = item.fileName
            guard FileNames.sanitized(name) == name, seen.insert(name.lowercased()).inserted else {
                return .failure(.inconsistentManifest)
            }
            let url = filesDir(id).appendingPathComponent(name, isDirectory: false)
            guard let size = try? OutgoingBatchWriter.fileSize(url), size == item.size else {
                return .failure(.missingFile)
            }
        }
        return .success(batch)
    }

    /// Modification date of `url`, or nil when it cannot be read.
    private static func modificationDate(_ url: URL) -> Date? {
        (try? FileManager.default.attributesOfItem(atPath: url.path))?[.modificationDate] as? Date
    }

    /// `date` in whole Unix milliseconds.
    private static func unixMs(_ date: Date) -> Int64 {
        Int64((date.timeIntervalSince1970 * 1000).rounded(.down))
    }
}

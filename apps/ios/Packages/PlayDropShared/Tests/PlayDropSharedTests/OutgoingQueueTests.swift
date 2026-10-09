// Round trips through the share extension's outgoing queue: staging, commit, listing,
// removal, abort and housekeeping, on a temporary directory.
import Foundation
import XCTest
@testable import PlayDropShared

final class OutgoingQueueTests: XCTestCase {
    private var base: URL!
    private var queue: OutgoingQueue!
    private let fm = FileManager.default

    override func setUpWithError() throws {
        base = fm.temporaryDirectory.appendingPathComponent("pd-queue-\(UUID().uuidString)", isDirectory: true)
        try fm.createDirectory(at: base, withIntermediateDirectories: true)
        queue = OutgoingQueue(root: OutgoingQueue.root(inContainer: base))
    }

    override func tearDownWithError() throws {
        try? fm.removeItem(at: base)
    }

    /// A source file outside the queue with `size` bytes of a repeating pattern.
    private func makeSource(_ name: String, size: Int) throws -> URL {
        let url = base.appendingPathComponent(name)
        var data = Data(count: size)
        for i in 0..<size { data[i] = UInt8(truncatingIfNeeded: i &* 31) }
        try data.write(to: url)
        return url
    }

    private func size(_ url: URL) throws -> UInt64 {
        (try fm.attributesOfItem(atPath: url.path)[.size] as? NSNumber)?.uint64Value ?? 0
    }

    func testRootIsTheOutgoingFolder() {
        XCTAssertEqual(queue.root.lastPathComponent, PlayDropIdentifiers.outgoingQueueFolderName)
        XCTAssertEqual(queue.root.deletingLastPathComponent().standardizedFileURL, base.standardizedFileURL)
    }

    func testBatchIdsAreSixteenLowercaseHex() {
        for _ in 0..<200 {
            let id = OutgoingBatchId.random()
            XCTAssertTrue(OutgoingBatchId.isValid(id), id)
        }
        XCTAssertFalse(OutgoingBatchId.isValid("ABCDEF0123456789"))
        XCTAssertFalse(OutgoingBatchId.isValid("abc"))
        XCTAssertFalse(OutgoingBatchId.isValid("../../../../etc1"))
    }

    func testRoundTripCopyMoveAndText() throws {
        // Larger than one copy chunk, so the streaming loop runs more than once.
        let big = try makeSource("video.mov", size: StreamingCopy.chunkSize * 2 + 123)
        let moved = try makeSource("photo.heic", size: 4096)

        let writer = try queue.beginBatch()
        XCTAssertTrue(OutgoingBatchId.isValid(writer.batchId))
        XCTAssertTrue(fm.fileExists(atPath: queue.stagingRoot.appendingPathComponent(writer.batchId).path))

        let a = try writer.addFile(copyingFrom: big, displayName: "video.mov", mimeType: nil)
        let b = try writer.addFile(movingFrom: moved, displayName: "photo.heic", mimeType: "image/heic")
        let c = try writer.addText("hello there", displayName: "Note")
        let d = try writer.addFile(copyingFrom: big, displayName: "VIDEO.MOV", mimeType: "video/quicktime")
        writer.setNote("for the laptop")
        writer.setPreferredPeer(String(repeating: "a1", count: 32))
        XCTAssertEqual(writer.itemCount, 4)

        XCTAssertEqual(a.fileName, "video.mov")
        XCTAssertEqual(a.size, UInt64(StreamingCopy.chunkSize * 2 + 123))
        XCTAssertEqual(a.mimeType, "video/quicktime")
        XCTAssertGreaterThan(a.modifiedMs, 0)
        XCTAssertTrue(fm.fileExists(atPath: big.path), "copy leaves the source")
        XCTAssertFalse(fm.fileExists(atPath: moved.path), "move removes the source")
        XCTAssertEqual(b.size, 4096)
        XCTAssertEqual(c.fileName, "Note.txt")
        XCTAssertEqual(c.size, UInt64("hello there".utf8.count))
        XCTAssertEqual(c.mimeType, "text/plain")
        XCTAssertEqual(d.fileName, "VIDEO (2).MOV", "names are unique ignoring case")
        XCTAssertEqual(d.displayName, "VIDEO.MOV")

        // Nothing is visible before the commit.
        XCTAssertEqual(try queue.listBatches(), [])

        let batch = try queue.commit(writer)
        XCTAssertEqual(batch.id, writer.batchId)
        XCTAssertEqual(batch.items, [a, b, c, d])
        XCTAssertEqual(batch.note, "for the laptop")
        XCTAssertEqual(batch.preferredPeer, String(repeating: "a1", count: 32))
        XCTAssertEqual(batch.totalBytes, a.size + b.size + c.size + d.size)
        XCTAssertFalse(fm.fileExists(atPath: queue.stagingRoot.appendingPathComponent(batch.id).path))

        let listed = try queue.listBatches()
        XCTAssertEqual(listed, [batch])
        for item in listed[0].items {
            let url = queue.fileURL(for: item, in: listed[0])
            XCTAssertTrue(fm.fileExists(atPath: url.path), item.fileName)
            XCTAssertEqual(try size(url), item.size, item.fileName)
        }
        XCTAssertEqual(try Data(contentsOf: queue.fileURL(for: c, in: batch)), Data("hello there".utf8))
        XCTAssertEqual(try Data(contentsOf: queue.fileURL(for: a, in: batch)), try Data(contentsOf: big))

        try queue.remove(batchId: batch.id)
        XCTAssertEqual(try queue.listBatches(), [])
        XCTAssertNoThrow(try queue.remove(batchId: batch.id), "removing twice is fine")
    }

    func testTextNameIsNotDoubledAndBlankNamesFallBack() throws {
        let writer = try queue.beginBatch()
        XCTAssertEqual(try writer.addText("x", displayName: "list.txt").fileName, "list.txt")
        XCTAssertEqual(try writer.addText("y", displayName: "  ").fileName, "text.txt")
        let src = try makeSource("from-provider.pdf", size: 10)
        XCTAssertEqual(try writer.addFile(copyingFrom: src, displayName: "", mimeType: nil).fileName, "from-provider.pdf")
        XCTAssertEqual(try writer.addFile(copyingFrom: src, displayName: "../evil/name.pdf", mimeType: nil).fileName,
                       ".._evil_name.pdf")
        queue.abort(writer)
    }

    func testEmptyCommitThrowsAndKeepsTheWriterOpen() throws {
        let writer = try queue.beginBatch()
        XCTAssertThrowsError(try queue.commit(writer)) { XCTAssertEqual($0 as? OutgoingQueueError, .emptyBatch) }
        _ = try writer.addText("now", displayName: "a")
        XCTAssertNoThrow(try queue.commit(writer))
        XCTAssertThrowsError(try queue.commit(writer)) { XCTAssertEqual($0 as? OutgoingQueueError, .writerClosed) }
        XCTAssertThrowsError(try writer.addText("late", displayName: "b")) {
            XCTAssertEqual($0 as? OutgoingQueueError, .writerClosed)
        }
    }

    func testAbortDeletesTheStagingDirectory() throws {
        let writer = try queue.beginBatch()
        _ = try writer.addText("draft", displayName: "draft")
        let staging = queue.stagingRoot.appendingPathComponent(writer.batchId)
        XCTAssertTrue(fm.fileExists(atPath: staging.path))
        queue.abort(writer)
        XCTAssertFalse(fm.fileExists(atPath: staging.path))
        queue.abort(writer)
        XCTAssertThrowsError(try queue.commit(writer))
        XCTAssertEqual(try queue.listBatches(), [])
    }

    func testDirectoriesAndMissingFilesAreRefused() throws {
        let writer = try queue.beginBatch()
        XCTAssertThrowsError(try writer.addFile(copyingFrom: base, displayName: "dir", mimeType: nil)) {
            guard case .notARegularFile = $0 as? OutgoingQueueError else { return XCTFail("\($0)") }
        }
        let missing = base.appendingPathComponent("missing.bin")
        XCTAssertThrowsError(try writer.addFile(movingFrom: missing, displayName: "m", mimeType: nil))
        XCTAssertEqual(writer.itemCount, 0)
        // A failed add frees its name.
        let src = try makeSource("dir", size: 1)
        XCTAssertEqual(try writer.addFile(copyingFrom: src, displayName: "dir", mimeType: nil).fileName, "dir")
        queue.abort(writer)
    }

    func testListingIsOldestFirst() throws {
        var ids: [String] = []
        for i in 0..<3 {
            let w = try queue.beginBatch()
            _ = try w.addText("\(i)", displayName: "t\(i)")
            ids.append(try queue.commit(w).id)
            Thread.sleep(forTimeInterval: 0.005)
        }
        XCTAssertEqual(try queue.listBatches().map(\.id), ids)
    }

    func testCorruptManifestIsSkippedAndReported() throws {
        let good = try queue.beginBatch()
        _ = try good.addText("ok", displayName: "ok")
        let goodBatch = try queue.commit(good)

        let bad = try queue.beginBatch()
        _ = try bad.addText("broken", displayName: "broken")
        let badBatch = try queue.commit(bad)
        let manifest = queue.root.appendingPathComponent(badBatch.id).appendingPathComponent(OutgoingManifest.fileName)
        try Data("{not json".utf8).write(to: manifest)

        let missing = try queue.beginBatch()
        _ = try missing.addText("gone", displayName: "gone")
        let missingBatch = try queue.commit(missing)
        try fm.removeItem(at: queue.fileURL(for: missingBatch.items[0], in: missingBatch))

        // A stray non-batch entry is ignored entirely.
        try fm.createDirectory(at: queue.root.appendingPathComponent("not-a-batch"), withIntermediateDirectories: true)

        let listing = try queue.listBatchesWithProblems()
        XCTAssertEqual(listing.batches, [goodBatch])
        XCTAssertEqual(Set(listing.problems.map(\.batchId)), [badBatch.id, missingBatch.id])
        XCTAssertTrue(listing.problems.contains(OutgoingQueueProblem(batchId: badBatch.id, reason: .unreadableManifest)))
        XCTAssertTrue(listing.problems.contains(OutgoingQueueProblem(batchId: missingBatch.id, reason: .missingFile)))
        XCTAssertEqual(try queue.listBatches(), [goodBatch])
    }

    func testTamperedManifestNamesAreRejected() throws {
        let w = try queue.beginBatch()
        _ = try w.addText("x", displayName: "x")
        var batch = try queue.commit(w)
        batch.items = [OutgoingItem(fileName: "../x.txt", displayName: "x", size: 1, mimeType: nil, modifiedMs: 0)]
        let data = try OutgoingManifest.encode(batch)
        try data.write(to: queue.root.appendingPathComponent(batch.id).appendingPathComponent(OutgoingManifest.fileName))
        let listing = try queue.listBatchesWithProblems()
        XCTAssertEqual(listing.problems, [OutgoingQueueProblem(batchId: batch.id, reason: .inconsistentManifest)])
    }

    func testUnknownManifestVersionIsSkipped() throws {
        let w = try queue.beginBatch()
        _ = try w.addText("x", displayName: "x")
        let batch = try queue.commit(w)
        let url = queue.root.appendingPathComponent(batch.id).appendingPathComponent(OutgoingManifest.fileName)
        var json = try XCTUnwrap(try JSONSerialization.jsonObject(with: Data(contentsOf: url)) as? [String: Any])
        json["version"] = 99
        try JSONSerialization.data(withJSONObject: json).write(to: url)
        XCTAssertEqual(try queue.listBatchesWithProblems().problems,
                       [OutgoingQueueProblem(batchId: batch.id, reason: .unsupportedVersion(99))])
    }

    func testRemoveRefusesBadIds() {
        XCTAssertThrowsError(try queue.remove(batchId: "../outgoing")) {
            XCTAssertEqual($0 as? OutgoingQueueError, .invalidBatchId("../outgoing"))
        }
    }

    func testSweepRemovesStaleStagingAndExpiredBatches() throws {
        let fresh = try queue.beginBatch()
        _ = try fresh.addText("in progress", displayName: "wip")
        let stale = try queue.beginBatch()
        _ = try stale.addText("crashed", displayName: "crashed")

        let keep = try queue.beginBatch()
        _ = try keep.addText("recent", displayName: "recent")
        let keptBatch = try queue.commit(keep)

        let corrupt = try queue.beginBatch()
        _ = try corrupt.addText("c", displayName: "c")
        let corruptBatch = try queue.commit(corrupt)
        try fm.removeItem(at: queue.root.appendingPathComponent(corruptBatch.id).appendingPathComponent(OutgoingManifest.fileName))

        // A leftover from a removal that was interrupted.
        let leftover = queue.stagingRoot.appendingPathComponent("\(OutgoingQueue.removingPrefix)0123456789abcdef-x")
        try fm.createDirectory(at: leftover, withIntermediateDirectories: true)

        // Age the stale staging directory (and its files folder) by two hours.
        let old = Date().addingTimeInterval(-7200)
        for url in [stale.stagingDir, stale.filesDir] {
            try fm.setAttributes([.modificationDate: old], ofItemAtPath: url.path)
        }

        // Nothing expired yet: removes the stale staging dir and the leftover only.
        XCTAssertEqual(try queue.sweep(), 2)
        XCTAssertTrue(fm.fileExists(atPath: fresh.stagingDir.path))
        XCTAssertFalse(fm.fileExists(atPath: stale.stagingDir.path))
        XCTAssertFalse(fm.fileExists(atPath: leftover.path))
        XCTAssertEqual(try queue.listBatches(), [keptBatch])
        XCTAssertEqual(try queue.listBatchesWithProblems().problems.map(\.reason), [.missingManifest])

        // Eight days later the batch has expired, and the corrupt directory and the
        // abandoned staging directory are stale.
        let later = Date().addingTimeInterval(8 * 86400)
        XCTAssertEqual(try queue.sweep(now: later), 3)
        XCTAssertEqual(try queue.listBatchesWithProblems(), OutgoingQueueListing(batches: [], problems: []))
        XCTAssertFalse(fm.fileExists(atPath: fresh.stagingDir.path))
        XCTAssertThrowsError(try queue.commit(fresh), "a swept staging directory cannot be committed")
    }

    func testSweepOnAnEmptyQueue() throws {
        XCTAssertEqual(try queue.sweep(), 0)
        XCTAssertEqual(try queue.listBatches(), [])
    }

    func testBatchCodableRoundTrip() throws {
        let batch = OutgoingBatch(
            id: "0123456789abcdef",
            createdAtMs: 1_791_000_000_000,
            items: [OutgoingItem(fileName: "a.jpg", displayName: "A.jpg", size: 3, mimeType: "image/jpeg", modifiedMs: 5)],
            note: nil,
            preferredPeer: nil
        )
        let data = try OutgoingManifest.encode(batch)
        XCTAssertEqual(try OutgoingManifest.decode(data).get(), batch)
    }
}

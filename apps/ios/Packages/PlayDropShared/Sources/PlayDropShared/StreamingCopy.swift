// Copies a file in fixed-size chunks so a multi-gigabyte video shared into the extension
// never sits in memory (the share extension has a tight memory limit). Used by the outgoing
// batch writer for copies and for moves across volumes.
import Foundation

/// Chunked file copy.
enum StreamingCopy {
    /// Bytes read and written per step.
    static let chunkSize = 1 << 20

    /// Copies `src` to a new file at `dst` and returns the bytes copied. `dst` must not
    /// exist. On failure the partial `dst` is removed and the error rethrown.
    static func copy(from src: URL, to dst: URL) throws -> UInt64 {
        let fm = FileManager.default
        guard fm.createFile(atPath: dst.path, contents: nil) else {
            throw OutgoingQueueError.io(detail: "cannot create \(dst.lastPathComponent)")
        }
        do {
            let reader = try FileHandle(forReadingFrom: src)
            defer { try? reader.close() }
            let writer = try FileHandle(forWritingTo: dst)
            defer { try? writer.close() }
            var total: UInt64 = 0
            while true {
                let done: Bool = try withAutoreleasePool {
                    guard let chunk = try reader.read(upToCount: chunkSize), !chunk.isEmpty else {
                        return true
                    }
                    try writer.write(contentsOf: chunk)
                    total += UInt64(chunk.count)
                    return false
                }
                if done { break }
            }
            try writer.synchronize()
            return total
        } catch {
            try? fm.removeItem(at: dst)
            throw error
        }
    }

    /// Runs `body` in an autorelease pool where one exists (Darwin), so each chunk's buffer
    /// is freed before the next is read; elsewhere it just runs `body`.
    private static func withAutoreleasePool<T>(_ body: () throws -> T) rethrows -> T {
        #if canImport(ObjectiveC)
        return try autoreleasepool(invoking: body)
        #else
        return try body()
        #endif
    }
}

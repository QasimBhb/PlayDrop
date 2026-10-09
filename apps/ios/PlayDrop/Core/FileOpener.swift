import Darwin
import Foundation
import PlayDropCore
import PlayDropShared
import UniformTypeIdentifiers

// Opens one file the user picked so the core can stream it (`SendSource.stream`). The core
// calls `open()` at offer time and again for every upload, resume or bundle member, from its
// own threads, and owns and closes every descriptor it gets (crates/playdrop-ffi traits.rs:
// "the side that receives one owns it").
//
// Security-scoped URLs from the document picker stay readable only while the scope is held,
// so the opener starts accessing the scope in `init` and holds it until `release()` (called
// by CoreHostImpl when the transfer completes or fails) or `deinit`. Only regular files are
// handed out: a FIFO or a directory could not be resumed.

/// A reopenable read-only handle on one file, for `SendSource.stream`.
final class FileOpener: SourceOpener, @unchecked Sendable {
    /// The file this opener reads.
    let url: URL

    /// Name shown in errors; already sanitised (SR-17).
    private let displayName: String
    private let lock = NSLock()
    private var holdsScope: Bool

    /// An opener for `url`. With `securityScoped`, starts accessing the URL's security scope
    /// now; the scope is held until `release()`.
    init(url: URL, securityScoped: Bool) {
        self.url = url
        self.displayName = FileNames.sanitized(url.lastPathComponent)
        self.holdsScope = securityScoped ? url.startAccessingSecurityScopedResource() : false
    }

    deinit {
        if holdsScope {
            url.stopAccessingSecurityScopedResource()
        }
    }

    /// A fresh read-only descriptor at offset 0, detached: the core owns and closes it.
    /// Throws `PlayDropError.Io` naming the file when it cannot be opened or is not a
    /// regular file.
    func open() throws -> Int32 {
        let fd: Int32 = url.withUnsafeFileSystemRepresentation { path -> Int32 in
            guard let path else {
                errno = EINVAL
                return -1
            }
            var result: Int32
            repeat {
                result = Darwin.open(path, O_RDONLY | O_CLOEXEC)
            } while result < 0 && errno == EINTR
            return result
        }
        guard fd >= 0 else {
            throw PlayDropError.Io(detail: "Cannot read \(displayName): \(Self.lastErrorText())")
        }
        var info = stat()
        guard fstat(fd, &info) == 0 else {
            let text = Self.lastErrorText()
            close(fd)
            throw PlayDropError.Io(detail: "Cannot read \(displayName): \(text)")
        }
        guard (mode_t(info.st_mode) & mode_t(S_IFMT)) == mode_t(S_IFREG) else {
            close(fd)
            throw PlayDropError.Io(detail: "Cannot send \(displayName): it is not a regular file")
        }
        return fd
    }

    /// Stops accessing the security scope. Idempotent; later `open()` calls may fail.
    func release() {
        lock.lock()
        let held = holdsScope
        holdsScope = false
        lock.unlock()
        if held {
            url.stopAccessingSecurityScopedResource()
        }
    }

    /// Name, size, modification time (Unix ms, 0 if unknown) and MIME type of `url`. For a
    /// security-scoped URL, call it while an opener holds the scope. Throws
    /// `PlayDropError.Io` when the item cannot be read or is not a regular file.
    static func metadata(of url: URL) throws -> (name: String, size: UInt64, mtimeMs: Int64, mime: String?) {
        let name = FileNames.sanitized(url.lastPathComponent)
        let attributes: [FileAttributeKey: Any]
        do {
            attributes = try FileManager.default.attributesOfItem(atPath: url.path(percentEncoded: false))
        } catch {
            Log.send.error("reading attributes failed: \(Log.describe(error), privacy: .public)")
            throw PlayDropError.Io(detail: "Cannot read \(name): it is missing or not downloaded yet")
        }
        if let type = attributes[.type] as? FileAttributeType, type != .typeRegular {
            throw PlayDropError.Io(detail: "Cannot send \(name): it is not a regular file")
        }
        let size = (attributes[.size] as? NSNumber)?.uint64Value ?? 0
        let mtimeMs: Int64
        if let date = attributes[.modificationDate] as? Date {
            mtimeMs = max(0, Int64((date.timeIntervalSince1970 * 1000).rounded(.down)))
        } else {
            mtimeMs = 0
        }
        let ext = url.pathExtension
        let mime = ext.isEmpty
            ? nil
            : (UTType(filenameExtension: ext)?.preferredMIMEType ?? FileNames.mimeType(forExtension: ext))
        return (name, size, mtimeMs, mime)
    }

    /// `strerror(errno)` as text.
    private static func lastErrorText() -> String {
        String(cString: strerror(errno))
    }
}

import Foundation
import PlayDropShared

// Stages photos and videos picked in the Send sheet. `PhotosPickerItem` hands its files over
// as temporary URLs that the system deletes soon after, so each one is moved (or copied when
// a move is not possible) into `<caches>/outgoing-picks/<uuid>/<name>`, which the app owns.
// The core reads it as a plain path (`SendItem.privateFile`), and the Send sheet calls
// `cleanUp` once the transfer has ended. `sweepStale` removes picks left behind by a crash.
// Files are never read into memory: a multi-gigabyte video is renamed, not loaded.

/// Errors staging can raise.
enum PhotoExportError: Error, Equatable {
    /// The caches directory could not be located.
    case noCachesDirectory
}

/// Staging area for picked media; see the file comment.
enum PhotoExport {
    /// Folder under Caches that holds staged picks.
    static let folderName = "outgoing-picks"

    /// Moves `fileURL` (a temporary file the picker produced) into a fresh staging folder
    /// under the sanitised `displayName` (SR-17) and returns it as a private send item.
    static func stage(fileURL: URL, displayName: String) throws -> SendItem {
        let fm = FileManager.default
        let dir = try stagingRoot().appendingPathComponent(UUID().uuidString, isDirectory: true)
        try fm.createDirectory(at: dir, withIntermediateDirectories: true)
        let fallbackName = fileURL.lastPathComponent
        let name = FileNames.sanitized(displayName.isEmpty ? fallbackName : displayName)
        let dest = dir.appendingPathComponent(name, isDirectory: false)
        do {
            do {
                try fm.moveItem(at: fileURL, to: dest)
            } catch {
                // Different volume or a source we may not move: copy instead.
                try fm.copyItem(at: fileURL, to: dest)
            }
        } catch {
            try? fm.removeItem(at: dir)
            throw error
        }
        return .privateFile(dest)
    }

    /// Deletes the staging folder of `item` if `stage` created it. Items outside the staging
    /// area (for example App Group batch files) are never touched.
    static func cleanUp(_ item: SendItem) {
        guard case .privateFile(let url) = item, let root = try? stagingRoot() else { return }
        let dir = url.deletingLastPathComponent().resolvingSymlinksInPath()
        guard dir.deletingLastPathComponent().path == root.resolvingSymlinksInPath().path else { return }
        try? FileManager.default.removeItem(at: dir)
    }

    /// Removes staging folders older than `maxAge`. Call once per launch; returns how many
    /// were removed.
    @discardableResult
    static func sweepStale(now: Date = Date(), maxAge: TimeInterval = 86_400) -> Int {
        let fm = FileManager.default
        guard let root = try? stagingRoot(),
              let entries = try? fm.contentsOfDirectory(
                  at: root, includingPropertiesForKeys: [.contentModificationDateKey], options: [.skipsHiddenFiles]
              )
        else { return 0 }
        var removed = 0
        for entry in entries {
            let modified = (try? entry.resourceValues(forKeys: [.contentModificationDateKey]))?.contentModificationDate
            guard let modified, now.timeIntervalSince(modified) > maxAge else { continue }
            if (try? fm.removeItem(at: entry)) != nil { removed += 1 }
        }
        return removed
    }

    private static func stagingRoot() throws -> URL {
        guard let caches = FileManager.default.urls(for: .cachesDirectory, in: .userDomainMask).first else {
            throw PhotoExportError.noCachesDirectory
        }
        return caches.appendingPathComponent(folderName, isDirectory: true)
    }
}

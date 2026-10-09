import Foundation
import PlayDropCore
import PlayDropShared

// Turns what the user picked (`SendItem`) into the core's `SendSource`s, and keeps the
// security-scoped openers alive for exactly as long as their transfer runs.
//
// - `privateFile`: a file in the app's sandbox or App Group container; sent by path.
// - `scopedFile`: a document-picker URL; sent as a stream through a `FileOpener` that holds
//   the security scope, because the core reopens the file for every upload and resume.
//
// The openers are registered under the transfer ID `send` returned and released on that
// transfer's complete or failed event. A tiny transfer can finish before `send` returns to
// the caller, so the registry remembers recently ended IDs and releases a late registration
// at once instead of holding its scope forever.

/// Sources ready for `PlayDropHost.send`, plus the openers that keep them readable.
struct PreparedSend: Sendable {
    /// The sources, in the order given.
    let sources: [SendSource]
    /// Openers holding security scopes; release them when the transfer ends.
    let openers: [FileOpener]
    /// Sum of the item sizes that were known up front (streams only).
    let streamBytes: UInt64

    /// Releases every opener now (the send failed to start or was abandoned).
    func release() {
        openers.forEach { $0.release() }
    }

    /// Builds the sources for `items`. Blocking file metadata reads; call it off the main
    /// actor. Throws `PlayDropError.Io` naming the first unreadable item; nothing stays held
    /// on failure.
    static func prepare(_ items: [SendItem]) throws -> PreparedSend {
        var sources: [SendSource] = []
        var openers: [FileOpener] = []
        var bytes: UInt64 = 0
        do {
            for item in items {
                switch item {
                case .privateFile(let url):
                    let path = url.path(percentEncoded: false)
                    guard FileManager.default.isReadableFile(atPath: path) else {
                        throw PlayDropError.Io(detail: "Cannot read \(FileNames.sanitized(url.lastPathComponent)): it is missing")
                    }
                    sources.append(.path(path: path))
                case .scopedFile(let url):
                    let opener = FileOpener(url: url, securityScoped: true)
                    openers.append(opener)
                    let meta = try FileOpener.metadata(of: url)
                    bytes &+= meta.size
                    sources.append(.stream(
                        name: meta.name, size: meta.size, mtimeMs: meta.mtimeMs, mime: meta.mime, opener: opener
                    ))
                }
            }
        } catch {
            openers.forEach { $0.release() }
            throw error
        }
        return PreparedSend(sources: sources, openers: openers, streamBytes: bytes)
    }
}

/// Openers held per transfer ID until the transfer ends; see the file comment. Main actor
/// only: the event router and `sendFiles` both run there.
@MainActor
final class HeldOpeners {
    /// How many ended transfer IDs are remembered for late registrations.
    static let endedMemory = 256

    private var byTransfer: [String: [FileOpener]] = [:]
    private var recentlyEnded: [String] = []
    private var recentlyEndedSet: Set<String> = []

    /// An empty registry.
    init() {}

    /// Holds `openers` until transfer `id` ends; releases them now if it already ended.
    func hold(_ openers: [FileOpener], for id: String) {
        guard !openers.isEmpty else {
            return
        }
        if recentlyEndedSet.contains(id) {
            openers.forEach { $0.release() }
            return
        }
        byTransfer[id, default: []].append(contentsOf: openers)
    }

    /// Transfer `id` completed, failed or was cancelled: release its openers.
    func transferEnded(_ id: String) {
        byTransfer.removeValue(forKey: id)?.forEach { $0.release() }
        if recentlyEndedSet.insert(id).inserted {
            recentlyEnded.append(id)
            if recentlyEnded.count > Self.endedMemory {
                recentlyEndedSet.remove(recentlyEnded.removeFirst())
            }
        }
    }

    /// The core stopped: no transfer will use any opener again.
    func releaseAll() {
        for openers in byTransfer.values {
            openers.forEach { $0.release() }
        }
        byTransfer.removeAll()
    }
}

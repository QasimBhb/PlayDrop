// The on-disk form of a committed batch (`manifest.json`). A version number wraps the batch
// so a later app can change the layout and an older reader skips what it does not know
// instead of misreading it.
import Foundation

/// The JSON document stored in each batch directory.
struct OutgoingManifest: Codable, Sendable {
    /// Layout version this code writes and reads.
    static let currentVersion = 1
    /// File name inside the batch directory.
    static let fileName = "manifest.json"

    /// Layout version of this document.
    let version: Int
    /// The batch it describes.
    let batch: OutgoingBatch

    /// Encodes `batch` as a current-version manifest.
    static func encode(_ batch: OutgoingBatch) throws -> Data {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys]
        return try encoder.encode(OutgoingManifest(version: currentVersion, batch: batch))
    }

    /// Decodes a manifest, or explains why it cannot be used.
    static func decode(_ data: Data) -> Result<OutgoingBatch, ManifestProblem> {
        let manifest: OutgoingManifest
        do {
            manifest = try JSONDecoder().decode(OutgoingManifest.self, from: data)
        } catch {
            return .failure(.unreadable)
        }
        guard manifest.version == currentVersion else {
            return .failure(.unsupportedVersion(manifest.version))
        }
        return .success(manifest.batch)
    }

    /// Why a manifest was rejected.
    enum ManifestProblem: Error, Equatable {
        /// Not valid JSON for this layout.
        case unreadable
        /// Written by a different layout version.
        case unsupportedVersion(Int)
    }
}

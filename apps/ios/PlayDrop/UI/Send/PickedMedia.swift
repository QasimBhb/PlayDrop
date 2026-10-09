import CoreTransferable
import Foundation
import UniformTypeIdentifiers

// Photos and videos picked with `PhotosPicker`, received as files rather than as `Data`.
//
// Why a file representation: `loadTransferable(type: Data.self)` reads the whole asset into
// memory, and a 4K video is easily 2 GB, which gets the app killed. `FileRepresentation`
// hands us a file the system wrote for us instead. That file is deleted as soon as the
// importing closure returns, so the closure stages it with `PhotoExport.stage` (a rename,
// or a streaming copy across volumes; never a read into memory) into an app-private folder
// under Caches, and the result is sent as a private file. Caches is right for it: it is
// app-private, not backed up, and only needed until the transfer ends, after which
// `SendModel` deletes it (`PhotoExport.cleanUp`).

/// A video from the photo library, staged as a private file.
struct PickedMovie: Transferable, Sendable {
    /// The staged file.
    let item: SendItem

    static var transferRepresentation: some TransferRepresentation {
        FileRepresentation(importedContentType: .movie) { received in
            PickedMovie(item: try PhotoExport.stage(
                fileURL: received.file,
                displayName: received.file.lastPathComponent
            ))
        }
    }
}

/// A photo from the photo library (the original HEIC, JPEG or PNG), staged as a private
/// file.
struct PickedImage: Transferable, Sendable {
    /// The staged file.
    let item: SendItem

    static var transferRepresentation: some TransferRepresentation {
        FileRepresentation(importedContentType: .image) { received in
            PickedImage(item: try PhotoExport.stage(
                fileURL: received.file,
                displayName: received.file.lastPathComponent
            ))
        }
    }
}

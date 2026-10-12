import Foundation
import Observation
import PhotosUI
// PhotosPickerItem lives in the PhotosUI + SwiftUI cross-import overlay. Xcode loads it on
// its own; the swift-build backend xtool uses on Linux does not, so it is named here.
import _PhotosUI_SwiftUI
import PlayDropCore
import SwiftUI
import UniformTypeIdentifiers

// What the Send sheet does: turn picked photos, videos and documents into `SendItem`s, hand
// them to `CoreHost.sendFiles`, then follow the transfer in `CoreHost.transfers`. Photos and
// videos become staged private files (`PhotoExport`, see `PickedMedia.swift`); documents
// stay where they are and go as security-scoped URLs, which the core host keeps open until
// the transfer ends. Staged files are deleted once the transfer finishes either way; ones
// left by a crash are swept by `PhotoExport.sweepStale`.

/// State of one Send sheet.
@MainActor
@Observable
final class SendModel {
    /// Where the sheet stands.
    enum Phase: Equatable {
        /// Nothing picked yet.
        case idle
        /// Copying picked media: `done` of `total`.
        case preparing(done: Int, total: Int)
        /// Handed to the core with this transfer ID.
        case sending(transferId: String)
        /// The send could not start; the words are in `message`.
        case failed(message: String)
    }

    /// Hex Device ID of the target.
    let peer: String
    /// Where the sheet stands.
    private(set) var phase: Phase = .idle

    @ObservationIgnored private let core: CoreHost
    @ObservationIgnored private var staged: [SendItem] = []

    /// Creates the model for `peer`.
    init(peer: String, core: CoreHost = .shared) {
        self.peer = peer
        self.core = core
    }

    /// True while picking more would interfere (copying or a send that has not finished).
    var busy: Bool {
        switch phase {
        case .preparing: return true
        case .sending: return !(transfer.map(Self.isFinished) ?? false)
        case .idle, .failed: return false
        }
    }

    /// The snapshot of the running transfer, if any.
    var transfer: TransferSnapshot? {
        guard case .sending(let id) = phase else { return nil }
        return core.transfers.first { $0.id == id }
    }

    /// Stages the picked photos and videos, then sends them.
    func send(photos: [PhotosPickerItem]) async {
        guard !photos.isEmpty else { return }
        phase = .preparing(done: 0, total: photos.count)
        var items: [SendItem] = []
        for (i, picked) in photos.enumerated() {
            do {
                if let item = try await Self.load(picked) { items.append(item) }
            } catch {
                items.forEach(PhotoExport.cleanUp)
                phase = .failed(message: UiMessage.textWithDetail(for: error))
                return
            }
            phase = .preparing(done: i + 1, total: photos.count)
        }
        staged.append(contentsOf: items)
        await start(items)
    }

    /// Sends documents picked with the file importer (security-scoped URLs).
    func send(documents result: Result<[URL], Error>) async {
        switch result {
        case .success(let urls):
            guard !urls.isEmpty else { return }
            await start(urls.map(SendItem.scopedFile))
        case .failure(let error):
            if let m = UiMessage.forError(error) { phase = .failed(message: m.text) }
        }
    }

    /// Deletes the staged files once the transfer reached an end state.
    func cleanUpIfFinished() {
        guard let t = transfer, Self.isFinished(t) else { return }
        discardStaged()
    }

    /// True for `complete`, `failed` and `cancelled`.
    static func isFinished(_ t: TransferSnapshot) -> Bool {
        t.state == .complete || t.state == .failed || t.state == .cancelled
    }

    private func start(_ items: [SendItem]) async {
        guard !items.isEmpty else {
            phase = .idle
            return
        }
        do {
            let id = try await core.sendFiles(peer: peer, items: items)
            phase = .sending(transferId: id)
            await core.refreshTransfers()
        } catch {
            discardStaged()
            phase = .failed(message: UiMessage.textWithDetail(for: error))
        }
    }

    private func discardStaged() {
        staged.forEach(PhotoExport.cleanUp)
        staged = []
    }

    /// Loads one picked item as a staged private file: a movie when it offers a movie type,
    /// otherwise an image.
    private static func load(_ picked: PhotosPickerItem) async throws -> SendItem? {
        if picked.supportedContentTypes.contains(where: { $0.conforms(to: .movie) }) {
            return try await picked.loadTransferable(type: PickedMovie.self)?.item
        }
        return try await picked.loadTransferable(type: PickedImage.self)?.item
    }
}

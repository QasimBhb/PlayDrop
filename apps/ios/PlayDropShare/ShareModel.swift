import Foundation
import Observation
import PlayDropShared

// State machine of one share: copy every shared item into a new App Group batch, commit it,
// then wait for the user to open PlayDrop or close the sheet. The extension never hosts the
// core (PLAN.md iOS share extension row, SR-30); the app drains the queue when it opens.
// Items are loaded one after another so at most one file is being copied at any moment,
// which keeps the extension far below its memory ceiling.

/// Where a share is.
enum SharePhase: Equatable {
    /// Copying items into the batch.
    case preparing
    /// The batch is committed; `deviceLabel` is the default device shown with its Short ID, if known.
    case queued(deviceLabel: String?)
    /// Nothing could be queued; the message says why.
    case failed(String)
}

/// Observable model behind `ShareView`; see the file comment.
@MainActor
@Observable
final class ShareModel {
    /// Current phase.
    private(set) var phase: SharePhase = .preparing
    /// Number of providers the host app handed over.
    private(set) var totalItems = 0
    /// Providers processed so far (added, skipped or failed).
    private(set) var processedItems = 0
    /// Bytes queued so far.
    private(set) var queuedBytes: UInt64 = 0
    /// Items queued so far.
    private(set) var queuedItems = 0
    /// Items that could not be read.
    private(set) var failedItems = 0

    @ObservationIgnored private var sink: BatchSink?
    @ObservationIgnored private var task: Task<Void, Never>?
    @ObservationIgnored private var seenTexts = Set<String>()
    @ObservationIgnored private let settings = SharedSettingsReader()

    /// Starts copying the attachments of `items` into a new batch.
    func start(items: [NSExtensionItem]) {
        guard task == nil else { return }
        let providers = items.flatMap { $0.attachments ?? [] }
        totalItems = providers.count
        let sink: BatchSink
        do {
            sink = try BatchSink()
        } catch {
            phase = .failed(Self.message(forStartError: error))
            return
        }
        self.sink = sink
        task = Task { [weak self] in
            for provider in providers {
                if Task.isCancelled { return }
                let result = await ShareItemLoader.load(provider, into: sink)
                guard let self else { return }
                self.record(result, sink: sink)
            }
            self?.finish(sink: sink)
        }
    }

    /// Stops copying and deletes the partial batch.
    func cancel() {
        task?.cancel()
        task = nil
        sink?.abort()
        sink = nil
    }

    // MARK: - Private

    private func record(_ result: ShareItemResult, sink: BatchSink) {
        processedItems += 1
        switch result {
        case .added(let item):
            queuedItems += 1
            queuedBytes += item.size
        case .text(let text, let displayName):
            // Safari can hand the same link over as a URL and as text; queue it once.
            guard seenTexts.insert(text).inserted else { return }
            do {
                let item = try sink.addText(text, displayName: displayName)
                queuedItems += 1
                queuedBytes += item.size
            } catch {
                failedItems += 1
            }
        case .failed:
            failedItems += 1
        case .unsupported:
            failedItems += 1
        }
    }

    private func finish(sink: BatchSink) {
        task = nil
        guard sink.itemCount > 0 else {
            sink.abort()
            self.sink = nil
            phase = .failed(failedItems > 0 ? "These items could not be read." : "Nothing to send")
            return
        }
        let peer = settings.defaultPeer
        do {
            try sink.setPreferredPeer(peer)
            _ = try sink.commit()
            self.sink = nil
            phase = .queued(deviceLabel: peer.flatMap { settings.displayLabel(of: $0) })
        } catch {
            sink.abort()
            self.sink = nil
            phase = .failed("Could not queue the items: \(error.localizedDescription)")
        }
    }

    private static func message(forStartError error: Error) -> String {
        if let sinkError = error as? BatchSinkError, sinkError == .noAppGroup {
            return "PlayDrop's shared folder is missing. Reinstall PlayDrop."
        }
        return "Could not prepare the items: \(error.localizedDescription)"
    }
}

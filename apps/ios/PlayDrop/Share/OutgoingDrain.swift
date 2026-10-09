import Foundation
import Observation
import PlayDropCore
import PlayDropShared

// Sends what the share extension queued. The extension cannot host the core, so it commits
// batches into the App Group outgoing queue and asks the user to open PlayDrop (SR-30). Every
// time the app comes forward, or `playdrop://` is opened, `drainIfPossible()` sends each
// batch to its preferred peer (the default device when the share happened) or the current
// default device, as soon as that peer is online. Batches with no usable target stay in
// `pendingBatches` for the UI, which shows a device picker and calls `send(batch:to:)`.
//
// A batch is deleted only after the core reports `transferComplete` for the transfer that
// carried it; on `transferFailed` it is kept and can be retried. Batch files live in the App
// Group container, which only this app and its extension can read, so they go to the core as
// plain paths (`SendItem.privateFile`). Nothing here survives a process exit except the queue
// itself: a batch that was mid-send when iOS killed the app is simply sent again.

/// Drains the App Group outgoing queue; see the file comment.
@MainActor
@Observable
public final class OutgoingDrain {
    /// The shared instance.
    public static let shared = OutgoingDrain()

    /// Committed batches not yet delivered, oldest first (including those being sent).
    public private(set) var pendingBatches: [OutgoingBatch] = []

    /// Batch ID to the transfer ID currently carrying it.
    public private(set) var sending: [String: String] = [:]

    /// The last problem, in words a person can read; nil after a clean drain.
    public private(set) var lastError: String?

    /// Why a drain was started; only explicit drains retry batches that already failed.
    private enum Trigger {
        /// Foreground, `playdrop://`, or a UI request.
        case explicit
        /// A peer came online.
        case peerOnline
    }

    @ObservationIgnored private let queue: OutgoingQueue?
    @ObservationIgnored private var eventTask: Task<Void, Never>?
    @ObservationIgnored private var draining = false
    /// Set when a drain is requested while one runs; the running drain makes one more pass
    /// with this trigger (explicit wins over peer-online).
    @ObservationIgnored private var againTrigger: Trigger?
    /// Batches whose send is being started (between the call and the returned transfer ID).
    @ObservationIgnored private var starting = Set<String>()
    /// Batches that failed during this process run; peer-online drains do not retry them.
    @ObservationIgnored private var failedThisRun = Set<String>()
    /// Outcomes of send transfers that ended before `send` recorded their ID (tiny batches
    /// can finish before `sendFiles` returns to us). Transfer ID to failure reason, nil on success.
    @ObservationIgnored private var unclaimedOutcomes: [String: String?] = [:]
    @ObservationIgnored private var unclaimedOrder: [String] = []
    private static let unclaimedLimit = 64

    private init() {
        if let container = FileManager.default.containerURL(
            forSecurityApplicationGroupIdentifier: PlayDropIdentifiers.appGroup
        ) {
            queue = OutgoingQueue(root: OutgoingQueue.root(inContainer: container))
        } else {
            queue = nil
        }
    }

    /// Re-lists the committed batches and sweeps stale staging directories (shares that
    /// were interrupted) and expired batches. Also starts following core events once.
    public func refresh() {
        startObservingIfNeeded()
        guard let queue else {
            pendingBatches = []
            lastError = "PlayDrop's shared folder is missing, so shared items cannot be read. Reinstall PlayDrop."
            return
        }
        do {
            _ = try queue.sweep()
        } catch {
            // A failed sweep only leaves stale files behind; listing still works.
        }
        do {
            pendingBatches = try queue.listBatches()
        } catch {
            lastError = "Could not read the shared items: \(UserFacingError.reason(error))"
        }
    }

    /// Sends every pending batch whose target is paired and online. Safe to call on every
    /// foreground and every `playdrop://` open: batches already being sent are skipped, and
    /// concurrent calls collapse into one more pass.
    public func drainIfPossible() async {
        await drain(trigger: .explicit)
    }

    /// Sends `batch` to `peer` (hex Device ID) and returns the transfer ID. The batch is
    /// removed when the transfer completes and kept when it fails. Throws `PlayDropError`
    /// or `CoreHostError` when the send cannot start.
    @discardableResult
    public func send(batch: OutgoingBatch, to peer: String) async throws -> String {
        guard let queue else { throw DrainError.noAppGroup }
        if let existing = sending[batch.id] { return existing }
        guard starting.insert(batch.id).inserted else { throw DrainError.alreadyStarting }
        defer { starting.remove(batch.id) }

        let items = batch.items.map { SendItem.privateFile(queue.fileURL(for: $0, in: batch)) }
        let transferId: String
        do {
            transferId = try await CoreHost.shared.sendFiles(peer: peer, items: items, note: batch.note)
        } catch {
            failedThisRun.insert(batch.id)
            lastError = "Could not send to \(CoreHost.shared.labelOf(peer: peer)): \(UserFacingError.reason(error))"
            throw error
        }
        sending[batch.id] = transferId
        if let outcome = unclaimedOutcomes.removeValue(forKey: transferId) {
            unclaimedOrder.removeAll { $0 == transferId }
            finishTransfer(transferId, peer: peer, failure: outcome)
        }
        return transferId
    }

    /// Deletes `batch` and its files without sending it.
    public func discard(batch: OutgoingBatch) {
        sending.removeValue(forKey: batch.id)
        failedThisRun.remove(batch.id)
        pendingBatches.removeAll { $0.id == batch.id }
        do {
            try queue?.remove(batchId: batch.id)
        } catch {
            lastError = "Could not delete the shared items: \(UserFacingError.reason(error))"
        }
    }

    /// The peer `drainIfPossible` would send `batch` to: its preferred peer while that is
    /// still paired, else the default device when paired. Nil means the UI must ask.
    public func intendedPeer(for batch: OutgoingBatch) -> String? {
        Self.intendedPeer(
            for: batch,
            paired: Set(CoreHost.shared.peers.map(\.deviceId)),
            defaultPeer: AppSettings.shared.defaultPeer
        )
    }

    /// The rule behind `intendedPeer(for:)`, free of shared state so it can be tested: the
    /// batch's preferred peer while it is in `paired`, else `defaultPeer` while it is in
    /// `paired`, else nil.
    nonisolated static func intendedPeer(
        for batch: OutgoingBatch, paired: Set<String>, defaultPeer: String?
    ) -> String? {
        if let preferred = batch.preferredPeer, paired.contains(preferred) {
            return preferred
        }
        if let fallback = defaultPeer, paired.contains(fallback) {
            return fallback
        }
        return nil
    }

    // MARK: - Draining

    private func drain(trigger: Trigger) async {
        if draining {
            againTrigger = (againTrigger == .explicit || trigger == .explicit) ? .explicit : .peerOnline
            return
        }
        draining = true
        defer {
            draining = false
            againTrigger = nil
        }
        var current: Trigger? = trigger
        while let pass = current {
            againTrigger = nil
            refresh()
            guard CoreHost.shared.running else { return }
            await reconcileWithCore()
            await sendReadyBatches(trigger: pass)
            current = againTrigger
        }
    }

    /// Drops `sending` entries the core no longer knows (the core was restarted while the
    /// transfer ran) so those batches are sent again, and finishes ones that completed.
    private func reconcileWithCore() async {
        guard !sending.isEmpty else { return }
        await CoreHost.shared.refreshTransfers()
        let byId = Dictionary(
            CoreHost.shared.transfers.map { ($0.id, $0) }, uniquingKeysWith: { first, _ in first }
        )
        for (batchId, transferId) in sending {
            guard let snapshot = byId[transferId] else {
                sending.removeValue(forKey: batchId)
                continue
            }
            switch snapshot.state {
            case .offered, .active:
                continue
            case .complete:
                finishTransfer(transferId, peer: snapshot.peer, failure: nil)
            case .failed, .cancelled:
                finishTransfer(transferId, peer: snapshot.peer, failure: snapshot.note ?? "The transfer stopped")
            }
        }
    }

    private func sendReadyBatches(trigger: Trigger) async {
        let online = Set(CoreHost.shared.peers.filter(\.online).map(\.deviceId))
        var hadError = false
        for batch in pendingBatches {
            guard sending[batch.id] == nil, !starting.contains(batch.id) else { continue }
            if trigger == .peerOnline && failedThisRun.contains(batch.id) { continue }
            guard let peer = intendedPeer(for: batch), online.contains(peer) else { continue }
            do {
                try await send(batch: batch, to: peer)
            } catch {
                hadError = true
            }
        }
        if !hadError && trigger == .explicit && pendingBatches.allSatisfy({ !failedThisRun.contains($0.id) }) {
            lastError = nil
        }
    }

    // MARK: - Events

    private func startObservingIfNeeded() {
        guard eventTask == nil else { return }
        let events = CoreHost.shared.events()
        eventTask = Task { [weak self] in
            for await event in events {
                guard let self else { return }
                self.handle(event)
            }
        }
    }

    private func handle(_ event: HostEvent) {
        switch event {
        case .transferComplete(let id, let peer, let direction, _, _, _, _, _):
            guard direction == .send else { return }
            routeOutcome(transferId: id, peer: peer, failure: nil)
        case .transferFailed(let id, let peer, let direction, let reason):
            guard direction == .send else { return }
            routeOutcome(transferId: id, peer: peer, failure: reason)
        case .peerOnline(let peer, _, _, _):
            let waiting = pendingBatches.contains { batch in
                sending[batch.id] == nil && !failedThisRun.contains(batch.id) && intendedPeer(for: batch) == peer
            }
            if waiting {
                Task { await self.drain(trigger: .peerOnline) }
            }
        default:
            return
        }
    }

    /// Applies a transfer outcome to the batch it carried, or parks it until `send` records
    /// the transfer ID.
    private func routeOutcome(transferId: String, peer: String, failure: String?) {
        if sending.values.contains(transferId) {
            finishTransfer(transferId, peer: peer, failure: failure)
            return
        }
        if starting.isEmpty { return }
        unclaimedOutcomes[transferId] = .some(failure)
        unclaimedOrder.append(transferId)
        if unclaimedOrder.count > Self.unclaimedLimit {
            let dropped = unclaimedOrder.removeFirst()
            unclaimedOutcomes.removeValue(forKey: dropped)
        }
    }

    private func finishTransfer(_ transferId: String, peer: String, failure: String?) {
        guard let batchId = sending.first(where: { $0.value == transferId })?.key else { return }
        sending.removeValue(forKey: batchId)
        if let failure {
            failedThisRun.insert(batchId)
            lastError = "Could not send to \(CoreHost.shared.labelOf(peer: peer)): \(failure)"
            return
        }
        failedThisRun.remove(batchId)
        pendingBatches.removeAll { $0.id == batchId }
        do {
            try queue?.remove(batchId: batchId)
        } catch {
            lastError = "Sent, but could not delete the shared copy: \(UserFacingError.reason(error))"
        }
    }
}

/// Errors only the drain raises.
public enum DrainError: Error, Equatable, Sendable {
    /// A send of this batch is already being started.
    case alreadyStarting
    /// The App Group container is missing (entitlement or provisioning problem).
    case noAppGroup
}

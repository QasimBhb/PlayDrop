import Foundation
import PlayDropCore

// Derives what a transfer row shows from the core's snapshots: direction, peer label, state,
// progress, rate, time left, and the received files to open. Offers still waiting for an
// answer are left out here because the Transfers tab shows them as offer cards from
// `CoreHost.pendingOffers` (which carry the file names). Plain logic for unit tests. Port of
// Android `ui/transfers/TransferRows.kt`.

/// A transfer as a row shows it.
struct TransferRow: Identifiable, Equatable, Sendable {
    /// Transfer ID.
    let id: String
    /// True for an incoming transfer.
    let receive: Bool
    /// Label of the other device, or its Short ID when it is no longer paired.
    let peerLabel: String
    /// Short ID of the other device (SR-8).
    let shortId: String
    /// Where it stands.
    let state: TransferStatus
    /// Bytes moved.
    let bytes: UInt64
    /// Total bytes.
    let total: UInt64
    /// Recent rate, bytes per second.
    let bytesPerSec: UInt64
    /// Seconds left at the current rate, while active.
    let etaSeconds: UInt64?
    /// Files in the transfer.
    let fileCount: UInt64
    /// Sender's note or failure reason.
    let note: String?
    /// Cancel is offered.
    let canCancel: Bool
    /// Forget is offered: a failed send the core keeps for resume. It calls the same cancel.
    let canForget: Bool
    /// Paths of the delivered files on this iPhone (receive side, once complete).
    let deliveredPaths: [String]

    /// Progress 0...1.
    var fraction: Double { UIFormat.fraction(bytes, of: total) }
}

/// See the file comment.
enum TransferRows {
    /// Rows newest first, without the offers in `pendingOfferIds`.
    static func derive(
        snapshots: [TransferSnapshot],
        pendingOfferIds: Set<String>,
        label: (String) -> String
    ) -> [TransferRow] {
        snapshots
            .filter { !pendingOfferIds.contains($0.id) }
            .enumerated()
            .sorted { a, b in
                a.element.startedAt != b.element.startedAt
                    ? a.element.startedAt > b.element.startedAt
                    : a.offset > b.offset
            }
            .map { row($0.element, label: label($0.element.peer)) }
    }

    /// Incoming offers waiting for the user, for the tab badge.
    static func pendingOfferCount(_ offers: [PendingOffer]) -> Int { offers.count }

    /// Paths of delivered files as file URLs; skips locations that are not local paths.
    static func fileURLs(_ paths: [String]) -> [URL] {
        paths.compactMap { location in
            if location.hasPrefix("/") { return URL(fileURLWithPath: location) }
            if let url = URL(string: location), url.isFileURL { return url }
            return nil
        }
    }

    private static func row(_ s: TransferSnapshot, label: String) -> TransferRow {
        let receive = s.direction == .receive
        let finished = s.state == .complete || s.state == .failed || s.state == .cancelled
        let active = s.state == .active
        return TransferRow(
            id: s.id,
            receive: receive,
            peerLabel: label,
            shortId: s.shortId,
            state: s.state,
            bytes: s.bytes,
            total: s.total,
            bytesPerSec: s.bytesPerSec,
            etaSeconds: active
                ? UIFormat.etaSeconds(remaining: s.total > s.bytes ? s.total - s.bytes : 0, bytesPerSec: s.bytesPerSec)
                : nil,
            fileCount: s.fileCount,
            note: s.note,
            canCancel: !finished && !(receive && s.state == .offered),
            canForget: !receive && s.state == .failed && s.resumable,
            deliveredPaths: receive && s.state == .complete ? s.delivered.map(\.location) : []
        )
    }
}

import PlayDropCore
import SwiftUI

// One transfer in the Transfers tab: direction, peer, state, file count and size, a progress
// bar with bytes, rate and time left while active, the note or failure reason, Cancel for
// running transfers, Forget for a failed send waiting to resume, and Open plus the Files
// location for received ones. Port of Android `TransferCard`.

/// One live or recent transfer.
struct TransferRowView: View {
    /// The row data.
    let row: TransferRow
    /// Cancels the transfer, or forgets a failed send that waits to resume.
    let onCancel: () -> Void
    /// Opens the first received file.
    let onOpen: (URL) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(alignment: .firstTextBaseline) {
                Image(systemName: row.receive ? "arrow.down.circle" : "arrow.up.circle")
                    .foregroundStyle(.tint)
                    .accessibilityLabel(Text(row.receive ? L10n.Transfers.incoming : L10n.Transfers.outgoing))
                Text(row.receive ? L10n.Transfers.from(row.peerLabel) : L10n.Transfers.to(row.peerLabel))
                    .font(.headline)
                    .lineLimit(1)
                Spacer(minLength: 0)
                Text(stateText)
                    .font(.caption.weight(.semibold))
                    .foregroundStyle(stateColor)
                    .monospacedDigit()
            }
            Text(verbatim: sizeText)
                .font(.subheadline)
            if row.state == .active {
                ProgressView(value: row.fraction)
                Text(verbatim: progressText)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .monospacedDigit()
            }
            if let note = row.note, !note.isEmpty {
                Text(verbatim: note)
                    .font(.caption)
                    .foregroundStyle(row.state == .failed ? Color.red : Color.secondary)
            }
            if !row.deliveredPaths.isEmpty {
                Text(L10n.Transfers.savedIn)
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            if row.canCancel || row.canForget || openURL != nil {
                HStack(spacing: 8) {
                    if let url = openURL {
                        Button(L10n.Transfers.open) { onOpen(url) }
                            .buttonStyle(.bordered)
                    }
                    if row.canForget {
                        // Same call as Cancel: the core forgets the waiting send and tells the receiver.
                        Button(L10n.Transfers.forget, role: .destructive, action: onCancel)
                            .buttonStyle(.borderless)
                    }
                    if row.canCancel {
                        Button(L10n.cancel, role: .destructive, action: onCancel)
                            .buttonStyle(.borderless)
                    }
                }
                .controlSize(.small)
            }
        }
        .padding(.vertical, 4)
    }

    private var openURL: URL? { TransferRows.fileURLs(row.deliveredPaths).first }

    private var sizeText: String {
        row.fileCount > 0
            ? "\(L10n.Transfers.files(Int(row.fileCount))), \(UIFormat.bytes(row.total))"
            : UIFormat.bytes(row.total)
    }

    private var progressText: String {
        var parts = [L10n.Transfers.of(UIFormat.bytes(row.bytes), UIFormat.bytes(row.total))]
        if row.bytesPerSec > 0 { parts.append(UIFormat.rate(row.bytesPerSec)) }
        if let eta = row.etaSeconds { parts.append(L10n.Transfers.left(UIFormat.duration(seconds: eta))) }
        return parts.joined(separator: " · ")
    }

    private var stateText: String {
        switch row.state {
        case .offered: return row.receive ? L10n.Transfers.waitingYou : L10n.Transfers.waitingPeer
        case .active: return L10n.percent(UIFormat.percent(row.bytes, of: row.total))
        case .complete: return L10n.Transfers.stateDone
        case .failed: return L10n.Transfers.stateFailed
        case .cancelled: return L10n.Transfers.stateCancelled
        }
    }

    private var stateColor: Color {
        switch row.state {
        case .failed: return .red
        case .offered: return .orange
        default: return .secondary
        }
    }
}

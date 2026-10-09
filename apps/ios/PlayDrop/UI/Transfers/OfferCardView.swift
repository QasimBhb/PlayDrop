import PlayDropCore
import SwiftUI

// An incoming offer from a device that may not send files without asking (no `files` grant,
// SR-14): who sends (label with Short ID, SR-8), how many files and how big, the first names
// and the note, with Accept and Decline. Used in the Transfers tab and the root offers sheet.

/// One pending offer with its answer buttons.
struct OfferCardView: View {
    /// The offer.
    let offer: PendingOffer

    @Environment(CoreHost.self) private var core
    @Environment(MessageCenter.self) private var messages
    @State private var answering = false

    /// How many file names the card lists before "and N more".
    static let namesShown = 3

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(alignment: .firstTextBaseline) {
                Image(systemName: "arrow.down.circle")
                    .foregroundStyle(.tint)
                    .accessibilityLabel(Text(L10n.Transfers.incoming))
                VStack(alignment: .leading, spacing: 2) {
                    Text(L10n.Transfers.offerTitle(core.labelOf(peer: offer.peer)))
                        .font(.headline)
                    ShortIdText(shortId: offer.shortId)
                }
                Spacer(minLength: 0)
                Text(L10n.Transfers.waitingYou)
                    .font(.caption.weight(.semibold))
                    .foregroundStyle(.orange)
            }
            Text(verbatim: "\(L10n.Transfers.files(Int(offer.fileCount))), \(UIFormat.bytes(offer.totalBytes))")
                .font(.subheadline)
            if !offer.files.isEmpty {
                Text(verbatim: names)
                    .font(.footnote)
                    .lineLimit(3)
            }
            if let note = offer.note, !note.isEmpty {
                Text(verbatim: note)
                    .font(.footnote)
                    .foregroundStyle(.secondary)
            }
            HStack(spacing: 8) {
                Button(L10n.Transfers.accept) { answer(true) }
                    .buttonStyle(.borderedProminent)
                Button(L10n.Transfers.decline) { answer(false) }
                    .buttonStyle(.bordered)
            }
            .disabled(answering)
        }
        .padding(.vertical, 4)
    }

    private var names: String {
        let shown = offer.files.prefix(Self.namesShown).map(\.path).joined(separator: ", ")
        let rest = Int(offer.fileCount) - min(Self.namesShown, offer.files.count)
        return rest > 0 ? "\(shown) \(L10n.Transfers.more(rest))" : shown
    }

    private func answer(_ accept: Bool) {
        answering = true
        let id = offer.transferId
        Task {
            do {
                try await core.respondOffer(transferId: id, accept: accept)
            } catch {
                if let m = UiMessage.forError(error) {
                    messages.show(.error(L10n.Transfers.offerAnswerFailed(m.text)))
                }
            }
            answering = false
        }
    }
}

/// The root sheet that lists every pending offer, so an offer is seen from any tab.
struct PendingOffersSheet: View {
    @Environment(CoreHost.self) private var core
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            List(core.pendingOffers) { offer in
                OfferCardView(offer: offer)
            }
            .navigationTitle(L10n.Transfers.offersSheetTitle)
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button(L10n.done) { dismiss() }
                }
            }
            .onChange(of: core.pendingOffers.isEmpty) { _, empty in
                if empty { dismiss() }
            }
        }
        .presentationDetents([.medium, .large])
    }
}

import PlayDropCore
import QuickLook
import SwiftUI

// The Transfers tab: incoming offers waiting for an answer on top, then the items the share
// extension queued that have not gone out yet, then live and recent transfers newest first.
// Received files open in Quick Look and live in Files > On My iPhone > PlayDrop. Port of
// Android `ui/transfers/TransfersScreen.kt`.

/// The Transfers tab.
struct TransfersView: View {
    @Environment(CoreHost.self) private var core
    @Environment(OutgoingDrain.self) private var drain
    @Environment(MessageCenter.self) private var messages

    @State private var batches = OutgoingBatchController()
    @State private var preview: URL?

    var body: some View {
        NavigationStack {
            let offerIds = Set(core.pendingOffers.map(\.transferId))
            let rows = TransferRows.derive(snapshots: core.transfers, pendingOfferIds: offerIds) { core.labelOf(peer: $0) }
            List {
                if !core.pendingOffers.isEmpty {
                    Section(L10n.Transfers.incoming) {
                        ForEach(core.pendingOffers) { offer in
                            OfferCardView(offer: offer)
                        }
                    }
                }
                if !drain.pendingBatches.isEmpty {
                    Section {
                        ForEach(drain.pendingBatches) { batch in
                            OutgoingBatchRow(batch: batch, controller: batches)
                        }
                    } header: {
                        Text(L10n.Outgoing.title)
                    } footer: {
                        Text(L10n.Outgoing.hint)
                    }
                }
                if !rows.isEmpty {
                    Section {
                        ForEach(rows) { row in
                            TransferRowView(
                                row: row,
                                onCancel: { cancel(row.id) },
                                onOpen: { preview = $0 }
                            )
                        }
                    }
                }
            }
            .overlay {
                if rows.isEmpty && core.pendingOffers.isEmpty && drain.pendingBatches.isEmpty {
                    ContentUnavailableView {
                        Label(L10n.Transfers.title, systemImage: "arrow.up.arrow.down")
                    } description: {
                        Text(L10n.Transfers.empty)
                    }
                }
            }
            .navigationTitle(L10n.Transfers.title)
            .refreshable {
                drain.refresh()
                await core.refreshTransfers()
            }
            .task {
                drain.refresh()
                await core.refreshTransfers()
            }
            .outgoingBatchFlow(batches)
            .quickLookPreview($preview)
        }
    }

    private func cancel(_ id: String) {
        Task {
            await CoreCall.run(messages) {
                try await core.withHost { host in try await host.cancelTransfer(id: id) }
            }
            await core.refreshTransfers()
        }
    }
}

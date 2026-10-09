import PlayDropShared
import SwiftUI

// The root sheet for items shared from other apps that have nowhere to go: no device was
// chosen in the share sheet and no default device is set. Asking right away keeps the
// promise that a share is never lost silently (anti-pattern 18); picking with "Always send
// here" sets the default so later shares go out with no question.

/// Lists queued batches that need a device.
struct PendingBatchesSheet: View {
    /// The batches to show.
    let batches: [OutgoingBatch]

    @Environment(\.dismiss) private var dismiss
    @State private var controller = OutgoingBatchController()

    var body: some View {
        NavigationStack {
            List {
                Section {
                    ForEach(batches) { batch in
                        OutgoingBatchRow(batch: batch, controller: controller)
                    }
                } footer: {
                    Text(L10n.Outgoing.hint)
                }
            }
            .navigationTitle(L10n.Outgoing.needsDeviceTitle)
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button(L10n.done) { dismiss() }
                }
            }
            .outgoingBatchFlow(controller)
        }
        .presentationDetents([.medium, .large])
    }
}

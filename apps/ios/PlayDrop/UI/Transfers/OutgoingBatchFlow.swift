import Observation
import PlayDropCore
import PlayDropShared
import SwiftUI

// Sending or discarding the batches the share extension queued (`OutgoingDrain`). A batch
// goes to the device it was shared to, else the default device; when neither is paired the
// user picks one, and the choice can become the default (one-tap goal). The controller holds
// which batch is being picked for or discarded; `outgoingBatchFlow(_:)` presents the picker
// and the discard confirmation, so list rows stay plain views with buttons.

/// Where a batch goes and the actions on it.
@MainActor
@Observable
final class OutgoingBatchController {
    /// The batch the device picker is open for.
    var picking: OutgoingBatch?
    /// The batch waiting for discard confirmation.
    var discarding: OutgoingBatch?

    @ObservationIgnored private let core: CoreHost
    @ObservationIgnored private let settings: AppSettings
    @ObservationIgnored private let drain: OutgoingDrain

    /// Creates the controller over the shared instances.
    init(core: CoreHost = .shared, settings: AppSettings = .shared, drain: OutgoingDrain = .shared) {
        self.core = core
        self.settings = settings
        self.drain = drain
    }

    /// The device a batch goes to without asking: the one it was shared to, else the
    /// default device, when still paired (the drain's own rule, so both always agree).
    func target(of batch: OutgoingBatch) -> String? {
        drain.intendedPeer(for: batch)
    }

    /// True while the drain is sending `batch`.
    func isSending(_ batch: OutgoingBatch) -> Bool { drain.sending[batch.id] != nil }

    /// Sends `batch` to `peer` and says so; `remember` makes `peer` the default.
    func send(_ batch: OutgoingBatch, to peer: String, remember: Bool = false, messages: MessageCenter) async {
        if remember { settings.defaultPeer = peer }
        let label = core.labelOf(peer: peer)
        let id = await CoreCall.run(messages) { try await self.drain.send(batch: batch, to: peer) }
        if id != nil { messages.info(L10n.Outgoing.sendingItems(batch.items.count, label)) }
    }

    /// Removes `batch` from the queue.
    func discard(_ batch: OutgoingBatch) {
        drain.discard(batch: batch)
    }
}

/// Presents the device picker and the discard confirmation for `controller`.
private struct OutgoingBatchFlow: ViewModifier {
    @Bindable var controller: OutgoingBatchController
    @Environment(CoreHost.self) private var core
    @Environment(MessageCenter.self) private var messages

    func body(content: Content) -> some View {
        content
            .sheet(item: $controller.picking) { batch in
                DevicePickerView(title: L10n.Picker.whichDevice, peers: core.peers) { peer, remember in
                    controller.picking = nil
                    Task { await controller.send(batch, to: peer, remember: remember, messages: messages) }
                }
            }
            .confirmationDialog(
                L10n.Outgoing.discardTitle,
                isPresented: Binding(
                    get: { controller.discarding != nil },
                    set: { if !$0 { controller.discarding = nil } }
                ),
                titleVisibility: .visible,
                presenting: controller.discarding
            ) { batch in
                Button(L10n.Outgoing.discard, role: .destructive) { controller.discard(batch) }
                Button(L10n.cancel, role: .cancel) {}
            } message: { _ in
                Text(L10n.Outgoing.discardBody)
            }
    }
}

extension View {
    /// Adds the picker and discard confirmation of `controller`.
    func outgoingBatchFlow(_ controller: OutgoingBatchController) -> some View {
        modifier(OutgoingBatchFlow(controller: controller))
    }
}

/// One queued batch: what it holds, and Send, Choose device and Discard.
struct OutgoingBatchRow: View {
    /// The batch.
    let batch: OutgoingBatch
    /// The flow it belongs to.
    let controller: OutgoingBatchController

    @Environment(CoreHost.self) private var core
    @Environment(MessageCenter.self) private var messages

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(alignment: .firstTextBaseline) {
                Image(systemName: "tray.and.arrow.up")
                    .foregroundStyle(.tint)
                    .accessibilityHidden(true)
                Text(L10n.Outgoing.summary(batch.items.count, UIFormat.bytes(batch.totalBytes)))
                    .font(.headline)
                Spacer(minLength: 0)
                if controller.isSending(batch) {
                    ProgressView()
                    Text(L10n.Outgoing.sending).font(.caption).foregroundStyle(.secondary)
                }
            }
            Text(verbatim: names)
                .font(.footnote)
                .lineLimit(2)
            TimelineView(.periodic(from: .now, by: 60)) { context in
                Text(L10n.Outgoing.sharedAgo(UIFormat.ago(
                    UInt64(max(0, batch.createdAtMs / 1000)),
                    now: UIFormat.nowSecs(context.date)
                )))
                .font(.caption)
                .foregroundStyle(.secondary)
            }
            if let note = batch.note, !note.isEmpty {
                Text(verbatim: note).font(.caption).foregroundStyle(.secondary)
            }
            if !controller.isSending(batch) {
                HStack(spacing: 8) {
                    if let target = controller.target(of: batch) {
                        Button(L10n.Outgoing.sendTo(core.labelOf(peer: target))) {
                            Task { await controller.send(batch, to: target, messages: messages) }
                        }
                        .buttonStyle(.borderedProminent)
                    }
                    if !core.peers.isEmpty {
                        Button(L10n.Outgoing.chooseDevice) { controller.picking = batch }
                            .buttonStyle(.bordered)
                    }
                    Spacer(minLength: 0)
                    Button(L10n.Outgoing.discard, role: .destructive) { controller.discarding = batch }
                        .buttonStyle(.borderless)
                }
                .controlSize(.small)
            }
        }
        .padding(.vertical, 4)
    }

    /// The first names as the other device will receive them: `fileName` is the wire name
    /// (sanitised and unique in the batch), `displayName` only what the user shared.
    private var names: String {
        let shown = batch.items.prefix(3).map(\.fileName).joined(separator: ", ")
        let rest = batch.items.count - min(3, batch.items.count)
        return rest > 0 ? "\(shown) \(L10n.Transfers.more(rest))" : shown
    }
}

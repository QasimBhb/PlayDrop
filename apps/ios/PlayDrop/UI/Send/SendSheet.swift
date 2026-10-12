import PhotosUI
// PhotosPickerItem lives in the PhotosUI + SwiftUI cross-import overlay. Xcode loads it on
// its own; the swift-build backend xtool uses on Linux does not, so it is named here.
import _PhotosUI_SwiftUI
import PlayDropCore
import SwiftUI
import UniformTypeIdentifiers

// "Send files" to one device: pick photos and videos (PhotosPicker, any number) or any
// documents (the Files picker), then watch the transfer go. One tap on the device or the
// Send button opens this; one more pick sends, with no confirm step in between (the
// one-tap goal). Errors are shown as words (anti-pattern 18).

/// The Send sheet for one paired device.
struct SendSheet: View {
    /// Hex Device ID of the target.
    let peer: String

    @Environment(CoreHost.self) private var core
    @Environment(\.dismiss) private var dismiss

    @State private var model: SendModel
    @State private var photoItems: [PhotosPickerItem] = []
    @State private var importing = false

    /// Creates the sheet for `peer`.
    init(peer: String) {
        self.peer = peer
        _model = State(initialValue: SendModel(peer: peer))
    }

    var body: some View {
        NavigationStack {
            List {
                Section {
                    if let p = core.peers.first(where: { $0.deviceId == peer }) {
                        HStack(spacing: 12) {
                            StatusDot(online: p.online)
                            PeerLabelText(label: p.label, shortId: p.shortId, font: .headline)
                            Spacer(minLength: 0)
                            if !p.online {
                                Text(L10n.Common.offline).font(.caption).foregroundStyle(.secondary)
                            }
                        }
                    }
                }
                Section {
                    PhotosPicker(
                        selection: $photoItems,
                        maxSelectionCount: nil,
                        selectionBehavior: .ordered,
                        matching: .any(of: [.images, .videos]),
                        preferredItemEncoding: .current
                    ) {
                        Label(L10n.Send.photos, systemImage: "photo.on.rectangle")
                    }
                    .disabled(model.busy)
                    Button {
                        importing = true
                    } label: {
                        Label(L10n.Send.files, systemImage: "folder")
                    }
                    .disabled(model.busy)
                } footer: {
                    Text(L10n.Send.hint)
                }
                statusSection
            }
            .navigationTitle(L10n.Picker.sendTo)
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button(L10n.done) { dismiss() }
                }
            }
            .fileImporter(isPresented: $importing, allowedContentTypes: [.item], allowsMultipleSelection: true) { result in
                Task { await model.send(documents: result) }
            }
            .onChange(of: photoItems) { _, items in
                guard !items.isEmpty else { return }
                photoItems = []
                Task { await model.send(photos: items) }
            }
            .onChange(of: model.transfer?.state) { _, _ in
                model.cleanUpIfFinished()
            }
        }
    }

    @ViewBuilder
    private var statusSection: some View {
        switch model.phase {
        case .idle:
            EmptyView()
        case .preparing(let done, let total):
            Section {
                HStack(spacing: 12) {
                    ProgressView()
                    Text(L10n.Send.preparing(done, total))
                }
            }
        case .sending:
            if let t = model.transfer {
                Section {
                    TransferRowView(
                        row: TransferRows.derive(snapshots: [t], pendingOfferIds: []) { core.labelOf(peer: $0) }[0],
                        onCancel: { cancel(t.id) },
                        onOpen: { _ in }
                    )
                }
            } else {
                Section {
                    HStack(spacing: 12) {
                        ProgressView()
                        Text(L10n.Send.starting)
                    }
                }
            }
        case .failed(let message):
            Section {
                Label(message, systemImage: "exclamationmark.triangle.fill")
                    .foregroundStyle(.red)
            }
        }
    }

    private func cancel(_ id: String) {
        Task {
            _ = try? await core.withHost { host in try await host.cancelTransfer(id: id) }
            await core.refreshTransfers()
        }
    }
}

extension L10n {
    /// Send sheet.
    enum Send {
        /// "Photos and videos".
        static var photos: String { String(localized: "ui_send_photos", defaultValue: "Photos and videos") }
        /// "Files".
        static var files: String { String(localized: "ui_send_files_picker", defaultValue: "Files") }
        /// Explains the pickers.
        static var hint: String {
            String(
                localized: "ui_send_hint",
                defaultValue: "Picked items go out right away. Keep PlayDrop open until the transfer is done."
            )
        }
        /// "Preparing 2 of 5".
        static func preparing(_ done: Int, _ total: Int) -> String {
            String(localized: "share_preparing_count", defaultValue: "Preparing \(done) of \(total)")
        }
        /// "Starting".
        static var starting: String { String(localized: "ui_send_starting", defaultValue: "Starting") }
    }
}

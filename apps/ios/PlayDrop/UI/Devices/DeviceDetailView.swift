import PlayDropCore
import PlayDropShared
import SwiftUI

// The Device screen: status, the actions (send files, send clipboard, ping, find, connect,
// set as default), remote input for computers that accept it, the media players the device
// reports, the permissions this iPhone grants the device (SR-14), the last-known addresses
// and unpair. Rename lives in the toolbar. Port of Android
// `ui/device/DeviceDetailScreen.kt`.

/// One paired device.
struct DeviceDetailView: View {
    /// Hex Device ID.
    let peerId: String

    @Environment(CoreHost.self) private var core
    @Environment(AppSettings.self) private var settings
    @Environment(MessageCenter.self) private var messages
    @Environment(DeviceActions.self) private var actions
    @Environment(\.dismiss) private var dismiss

    @State private var model: DeviceDetailModel
    @State private var sending = false
    @State private var renaming = false
    @State private var renameText = ""
    @State private var pinging = false
    @State private var pingText = ""
    @State private var confirmUnpair = false

    /// Creates the screen for `peerId`.
    init(peerId: String) {
        self.peerId = peerId
        _model = State(initialValue: DeviceDetailModel(peerId: peerId))
    }

    var body: some View {
        Group {
            if let peer = core.peers.first(where: { $0.deviceId == peerId }) {
                form(peer)
                    .navigationTitle(peer.label)
                    .toolbar {
                        ToolbarItem(placement: .primaryAction) {
                            Button {
                                renameText = peer.label
                                renaming = true
                            } label: {
                                Image(systemName: "pencil")
                            }
                            .accessibilityLabel(Text(L10n.Detail.rename))
                        }
                    }
                    .alert(L10n.Detail.renameTitle, isPresented: $renaming) {
                        TextField(L10n.Detail.title, text: $renameText)
                            .textInputAutocapitalization(.words)
                        Button(L10n.Detail.rename) {
                            let text = renameText
                            Task { await model.rename(to: text, messages: messages) }
                        }
                        .disabled(DeviceDetailModel.cleanLabel(renameText).isEmpty)
                        Button(L10n.cancel, role: .cancel) {}
                    } message: {
                        Text(L10n.Detail.renameBody)
                    }
                    .alert(L10n.Detail.ping, isPresented: $pinging) {
                        TextField(L10n.Detail.pingPlaceholder, text: $pingText)
                        Button(L10n.Detail.pingSend) {
                            let text = pingText
                            Task { await actions.ping(peerId, message: text, messages: messages) }
                        }
                        Button(L10n.cancel, role: .cancel) {}
                    } message: {
                        Text(L10n.Detail.pingBody)
                    }
                    .confirmationDialog(
                        L10n.Detail.unpairTitle(peer.label),
                        isPresented: $confirmUnpair,
                        titleVisibility: .visible
                    ) {
                        Button(L10n.Detail.unpair, role: .destructive) {
                            Task {
                                if await actions.unpair(peerId, messages: messages) { dismiss() }
                            }
                        }
                        Button(L10n.cancel, role: .cancel) {}
                    } message: {
                        Text(verbatim: "\(L10n.Detail.unpairBody)\n\(peer.shortId)")
                    }
                    .sheet(isPresented: $sending) { SendSheet(peer: peerId) }
            } else {
                ContentUnavailableView(L10n.Detail.gone, systemImage: "questionmark.circle")
                    .navigationTitle(L10n.Detail.title)
            }
        }
        .navigationBarTitleDisplayMode(.inline)
        .onDisappear {
            let id = peerId
            Task { await actions.stopFindIfRinging(id) }
        }
    }

    /// The remote input entry: a link while the computer is online and accepts pointer or
    /// key input, a disabled row saying why when it accepts neither, nothing while offline
    /// (its capabilities are unknown then).
    @ViewBuilder
    private func remoteInputSection(_ peer: PeerSummary) -> some View {
        switch RemoteInputAvailability.of(online: peer.online, capabilities: peer.capabilities) {
        case .available:
            Section {
                NavigationLink(value: DevicesRoute.remoteInput(peerId)) {
                    Label(L10n.Input.title, systemImage: "cursorarrow.motionlines")
                }
            }
        case .unsupported:
            Section {
                Label(L10n.Input.unsupported, systemImage: "cursorarrow.slash")
                    .foregroundStyle(.secondary)
            }
        case .offline:
            EmptyView()
        }
    }

    private func form(_ peer: PeerSummary) -> some View {
        let row = DeviceRows.derive(peers: [peer], status: core.peerStatus, defaultPeer: settings.defaultPeer)[0]
        let ringing = actions.ringing.contains(peerId)
        return Form {
            if peer.removedByPeer {
                Section {
                    Label(L10n.Detail.removed, systemImage: "exclamationmark.triangle.fill")
                        .foregroundStyle(.red)
                    NavigationLink(value: DevicesRoute.addDevice) { Text(L10n.Detail.pairAgain) }
                }
            }
            Section {
                HStack {
                    ShortIdText(shortId: peer.shortId)
                    Spacer()
                    if let level = row.batteryLevel, row.online {
                        BatteryLabel(level: level, charging: row.charging)
                    }
                }
                TimelineView(.periodic(from: .now, by: 30)) { context in
                    Text(verbatim: DeviceRows.statusText(row, now: UIFormat.nowSecs(context.date))
                        + "  ·  " + L10n.Detail.pairedOn(UIFormat.date(unixSecs: peer.pairedAt)))
                        .font(.subheadline)
                        .foregroundStyle(.secondary)
                }
            }
            Section {
                Button {
                    sending = true
                } label: {
                    Label(L10n.Devices.sendFiles, systemImage: "paperplane")
                }
                Button {
                    Task { await actions.sendClipboard(to: peerId, label: peer.label, messages: messages) }
                } label: {
                    Label(L10n.Detail.sendClipboard, systemImage: "doc.on.clipboard")
                }
                Button {
                    pingText = ""
                    pinging = true
                } label: {
                    Label(L10n.Detail.ping, systemImage: "bell")
                }
                Button {
                    Task { await actions.toggleFind(peerId, messages: messages) }
                } label: {
                    Label(
                        ringing ? L10n.Detail.findStop : L10n.Detail.find,
                        systemImage: ringing ? "speaker.slash" : "speaker.wave.3"
                    )
                }
                if !peer.online {
                    Button {
                        Task { await actions.connect(peerId, messages: messages) }
                    } label: {
                        Label(L10n.Detail.connect, systemImage: "arrow.triangle.2.circlepath")
                    }
                }
            } footer: {
                if ringing { Text(L10n.Detail.ringing) }
            }
            remoteInputSection(peer)
            NowPlayingSection(
                peerId: peerId,
                players: core.mediaPlayers[peerId] ?? [],
                controllable: peerAcceptsMediaControl(online: peer.online, capabilities: peer.capabilities)
            )
            Section {
                if row.isDefault {
                    Label(L10n.Detail.isDefault, systemImage: "star.fill")
                } else {
                    Button {
                        actions.setDefault(peerId, messages: messages)
                    } label: {
                        Label(L10n.Detail.setDefault, systemImage: "star")
                    }
                }
            }
            Section {
                let grants = model.grants(of: peer)
                ForEach(GrantKind.allCases) { kind in
                    Toggle(isOn: Binding(
                        get: { kind.value(in: grants) },
                        set: { value in
                            Task { await model.setGrant(kind, to: value, current: grants, messages: messages) }
                        }
                    )) {
                        VStack(alignment: .leading, spacing: 2) {
                            Text(kind.title)
                            Text(kind.detail).font(.footnote).foregroundStyle(.secondary)
                        }
                    }
                }
            } header: {
                Text(L10n.Detail.grantsTitle)
            } footer: {
                // The `input` grant (control this iPhone) has no switch: iOS does not allow it.
                Text(L10n.Detail.inputNotPossible)
            }
            Section {
                DisclosureGroup(L10n.Detail.addresses) {
                    if peer.lastKnown.isEmpty {
                        Text(L10n.Detail.noAddresses).foregroundStyle(.secondary)
                    } else {
                        ForEach(peer.lastKnown, id: \.self) { addr in
                            Text(verbatim: addr)
                                .font(.system(.footnote, design: .monospaced))
                                .textSelection(.enabled)
                        }
                    }
                }
            }
            Section {
                Button(L10n.Detail.unpair, role: .destructive) { confirmUnpair = true }
            }
        }
    }
}

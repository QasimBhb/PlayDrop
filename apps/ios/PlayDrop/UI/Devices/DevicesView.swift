import PlayDropCore
import PlayDropShared
import SwiftUI

// The Devices tab: one row per paired device with swipe actions (ping, find, unpair), the
// "Add device" button, the one-tap "Send files" button and the empty state. When the core is
// down, or the Local Network permission looks denied, a banner on top leads to the fix-it
// screen (anti-pattern 18). Port of Android `ui/devices/DevicesScreen.kt`.

/// Destinations pushed on the Devices tab's navigation stack.
enum DevicesRoute: Hashable {
    /// The Device screen for a hex Device ID.
    case device(String)
    /// The Add device screen.
    case addDevice
    /// The Local Network fix-it screen.
    case localNetworkFixIt
    /// The remote input screen for a hex Device ID.
    case remoteInput(String)
}

/// The Devices tab.
struct DevicesView: View {
    @Environment(CoreHost.self) private var core
    @Environment(AppSettings.self) private var settings
    @Environment(MessageCenter.self) private var messages
    @Environment(DeviceActions.self) private var actions

    @State private var sheet: DevicesSheet?
    @State private var confirmUnpair: DeviceRow?

    var body: some View {
        NavigationStack {
            content
                .navigationTitle(L10n.Devices.title)
                .toolbar {
                    ToolbarItem(placement: .primaryAction) {
                        NavigationLink(value: DevicesRoute.addDevice) {
                            Image(systemName: "plus")
                        }
                        .accessibilityLabel(Text(L10n.Devices.addDevice))
                    }
                }
                .navigationDestination(for: DevicesRoute.self) { route in
                    switch route {
                    case .device(let id): DeviceDetailView(peerId: id)
                    case .addDevice: AddDeviceView()
                    case .localNetworkFixIt: LocalNetworkFixItView(reason: core.startError)
                    case .remoteInput(let id): RemoteInputView(peerId: id)
                    }
                }
        }
        .sheet(item: $sheet) { which in
            switch which {
            case .pick:
                DevicePickerView(title: L10n.Picker.sendTo, peers: core.peers) { peer, remember in
                    if remember { settings.defaultPeer = peer }
                    sheet = .send(peer)
                }
            case .send(let peer):
                SendSheet(peer: peer)
            }
        }
        .confirmationDialog(
            L10n.Detail.unpairTitle(confirmUnpair?.label ?? ""),
            isPresented: Binding(get: { confirmUnpair != nil }, set: { if !$0 { confirmUnpair = nil } }),
            titleVisibility: .visible,
            presenting: confirmUnpair
        ) { row in
            Button(L10n.Detail.unpair, role: .destructive) {
                Task { await actions.unpair(row.deviceId, messages: messages) }
            }
            Button(L10n.cancel, role: .cancel) {}
        } message: { row in
            Text(verbatim: "\(L10n.Detail.unpairBody)\n\(row.shortId)")
        }
        .task(id: core.running) {
            if core.running { await core.refreshDiagnostics() }
        }
    }

    @ViewBuilder
    private var content: some View {
        let rows = DeviceRows.derive(peers: core.peers, status: core.peerStatus, defaultPeer: settings.defaultPeer)
        List {
            if !core.running {
                Section { NotRunningBanner(startError: core.startError) }
                    .listRowInsets(EdgeInsets(top: 8, leading: 16, bottom: 8, trailing: 16))
            } else if localNetworkLikelyDenied {
                Section {
                    NavigationLink(value: DevicesRoute.localNetworkFixIt) {
                        Label(L10n.Devices.localNetworkBanner, systemImage: "wifi.exclamationmark")
                            .foregroundStyle(.red)
                    }
                }
            }
            if !rows.isEmpty {
                Section {
                    ForEach(rows) { row in
                        NavigationLink(value: DevicesRoute.device(row.deviceId)) {
                            DeviceRowView(row: row, ringing: actions.ringing.contains(row.deviceId))
                        }
                        .swipeActions(edge: .leading, allowsFullSwipe: true) {
                            Button {
                                Task { await actions.ping(row.deviceId, message: nil, messages: messages) }
                            } label: {
                                Label(L10n.Detail.ping, systemImage: "bell")
                            }
                            .tint(.blue)
                        }
                        .swipeActions(edge: .trailing, allowsFullSwipe: false) {
                            Button {
                                confirmUnpair = row
                            } label: {
                                Label(L10n.Detail.unpair, systemImage: "minus.circle")
                            }
                            .tint(.red)
                            Button {
                                Task { await actions.toggleFind(row.deviceId, messages: messages) }
                            } label: {
                                if actions.ringing.contains(row.deviceId) {
                                    Label(L10n.Detail.findStop, systemImage: "speaker.slash")
                                } else {
                                    Label(L10n.Detail.find, systemImage: "speaker.wave.3")
                                }
                            }
                            .tint(.orange)
                        }
                    }
                }
            }
        }
        .overlay {
            if rows.isEmpty { emptyState }
        }
        .safeAreaInset(edge: .bottom) {
            if !rows.isEmpty { sendButton }
        }
        .refreshable { await core.refreshPeers() }
    }

    private var emptyState: some View {
        ContentUnavailableView {
            Label(L10n.Devices.emptyTitle, systemImage: "laptopcomputer.and.iphone")
        } description: {
            Text(L10n.Devices.emptyBody)
        } actions: {
            NavigationLink(value: DevicesRoute.addDevice) {
                Text(L10n.Devices.addDevice)
            }
            .buttonStyle(.borderedProminent)
            NavigationLink {
                ConnectivityHelpView()
            } label: {
                Text(L10n.Connect.title)
            }
        }
    }

    private var sendButton: some View {
        Button {
            if let target = DeviceRows.sendTarget(peers: core.peers, defaultPeer: settings.defaultPeer) {
                sheet = .send(target)
            } else {
                sheet = .pick
            }
        } label: {
            Label(L10n.Devices.sendFiles, systemImage: "paperplane.fill")
                .frame(maxWidth: .infinity)
        }
        .buttonStyle(.borderedProminent)
        .controlSize(.large)
        .padding(.horizontal, 16)
        .padding(.bottom, 8)
    }

    /// True when the diagnostics suggest the Local Network permission is off.
    private var localNetworkLikelyDenied: Bool {
        guard let d = core.diagnostics else { return false }
        let access = LocalNetworkInference.infer(
            from: DiagnosticsMapping.snapshot(d),
            peerOnlineThisRun: core.peerSeenOnlineThisRun
        )
        if case .likelyDenied = access { return true }
        return false
    }
}

/// Which sheet the Devices tab shows.
private enum DevicesSheet: Identifiable, Hashable {
    /// "Send to which device?".
    case pick
    /// The Send sheet for a hex Device ID.
    case send(String)

    var id: Self { self }
}

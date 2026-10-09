import PlayDropCore
import PlayDropShared
import SwiftUI

// The app's single UI entry point. Puts the shared observable objects in the environment
// (`CoreHost`, `AppSettings`, `OutgoingDrain`, the message center and the device actions),
// shows onboarding until it is done, then the three tabs (Devices, Transfers, Settings) with
// the root sheet (pairing prompt, pending offers, shared items that need a device) and the
// message toast and the find-my-phone alert over everything. `App/PlayDropApp.swift` only
// has to show `RootView()`.

/// The root of the app's UI.
public struct RootView: View {
    @State private var messages = MessageCenter()
    @State private var deviceActions = DeviceActions()

    /// Creates the root.
    public init() {}

    public var body: some View {
        RootContent()
            .environment(CoreHost.shared)
            .environment(AppSettings.shared)
            .environment(OutgoingDrain.shared)
            .environment(messages)
            .environment(deviceActions)
            .messageBanner(messages)
            .findRingerAlert()
    }
}

/// The tabs of the main screen.
enum RootTab: Hashable {
    /// Devices.
    case devices
    /// Transfers.
    case transfers
    /// Settings.
    case settings
}

/// Onboarding or the tabs, plus the root sheet.
private struct RootContent: View {
    @Environment(CoreHost.self) private var core
    @Environment(AppSettings.self) private var settings
    @Environment(OutgoingDrain.self) private var drain

    @State private var tab: RootTab = .devices
    @State private var snoozedOffers: Set<String> = []
    @State private var snoozedBatches: Set<String> = []

    var body: some View {
        if settings.onboardingDone {
            tabs
        } else {
            OnboardingView()
        }
    }

    private var tabs: some View {
        TabView(selection: $tab) {
            DevicesView()
                .tabItem { Label(L10n.Tab.devices, systemImage: "laptopcomputer.and.iphone") }
                .tag(RootTab.devices)
            TransfersView()
                .tabItem { Label(L10n.Tab.transfers, systemImage: "arrow.up.arrow.down") }
                .badge(core.pendingOffers.count)
                .tag(RootTab.transfers)
            SettingsView()
                .tabItem { Label(L10n.Tab.settings, systemImage: "gear") }
                .tag(RootTab.settings)
        }
        .sheet(item: sheetBinding) { sheet in
            switch sheet {
            case .pairing:
                if let pending = core.pairingPrompt {
                    PairingSheet(pending: pending) { accept in
                        Task { await core.answerPairing(accept: accept) }
                    }
                    .id(pending.peer + pending.shortId + Self.promptKind(pending.prompt))
                }
            case .offers:
                PendingOffersSheet()
            case .batches:
                PendingBatchesSheet(batches: batchesNeedingDevice)
            }
        }
    }

    private var batchesNeedingDevice: [OutgoingBatch] {
        RootSheet.batchesNeedingDevice(
            drain.pendingBatches,
            sending: drain.sending,
            hasPeers: !core.peers.isEmpty,
            intendedPeer: drain.intendedPeer(for:)
        )
    }

    private var currentSheet: RootSheet? {
        RootSheet.current(
            pairingWaiting: core.pairingPrompt != nil,
            offerIds: core.pendingOffers.map(\.id),
            onTransfersTab: tab == .transfers,
            batchIdsNeedingDevice: batchesNeedingDevice.map(\.id),
            snoozedOffers: snoozedOffers,
            snoozedBatches: snoozedBatches
        )
    }

    /// Reads the computed sheet; a user dismissal snoozes what was on screen.
    private var sheetBinding: Binding<RootSheet?> {
        Binding(
            get: { currentSheet },
            set: { newValue in
                guard newValue == nil, let shown = currentSheet else { return }
                switch shown {
                case .pairing:
                    break
                case .offers:
                    snoozedOffers.formUnion(core.pendingOffers.map(\.id))
                case .batches:
                    snoozedBatches.formUnion(batchesNeedingDevice.map(\.id))
                }
            }
        )
    }

    /// Distinguishes a SAS prompt from a scanner confirmation in the sheet's identity, so a
    /// new prompt gets fresh button state. The SAS digits are not part of it.
    private static func promptKind(_ prompt: PairingPrompt) -> String {
        if case .sas = prompt { return "sas" }
        return "confirm"
    }
}

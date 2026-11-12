import PlayDropCore
import PlayDropShared
import SwiftUI
import UIKit

// The remote input screen: trackpad, keyboard and presenter tabs for one computer, shown
// only for tabs the computer accepts (its `input.*` capabilities). The session opens when
// the screen appears and closes when it goes away (`RemoteInputModel`). The screen keeps the
// iPhone awake while open. When the session ends for good (the computer refuses input from
// this phone, is locked, was stopped, went idle...) the panes give way to a sentence saying
// why and what to do; it stays until the user goes back, so it cannot be missed.

/// Remote input for one peer.
struct RemoteInputView: View {
    /// Hex Device ID.
    let peerId: String

    @Environment(CoreHost.self) private var core
    @Environment(AppSettings.self) private var settings
    @Environment(MessageCenter.self) private var messages
    @Environment(\.dismiss) private var dismiss

    @State private var model: RemoteInputModel
    @State private var tab: RemoteInputTab = .trackpad
    @State private var lastTabs: [RemoteInputTab] = []
    @State private var showingOptions = false
    @State private var idleTimerWasDisabled = false

    /// Creates the screen for `peerId`.
    init(peerId: String) {
        self.peerId = peerId
        _model = State(initialValue: RemoteInputModel(
            peerId: peerId, sensitivity: AppSettings.shared.trackpadSensitivity
        ))
    }

    var body: some View {
        let peer = core.peers.first { $0.deviceId == peerId }
        let caps = peer?.capabilities ?? []
        let available = RemoteInputTab.available(for: caps)
        // Offline peers report no capabilities: keep the tabs the screen already had.
        let tabs = available.isEmpty ? lastTabs : available
        VStack(spacing: 12) {
            if let ended = model.ended {
                endedContent(ended)
            } else {
                panes(tabs: tabs, caps: caps)
            }
        }
        .padding()
        .navigationTitle(L10n.Input.title)
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                Button {
                    showingOptions = true
                } label: {
                    Image(systemName: "slider.horizontal.3")
                }
                .accessibilityLabel(Text(L10n.Input.options))
                .popover(isPresented: $showingOptions) {
                    optionsPopover.presentationCompactAdaptation(.popover)
                }
            }
        }
        .onAppear {
            if !available.isEmpty { lastTabs = available }
            if let first = available.first, !available.contains(tab) { tab = first }
            idleTimerWasDisabled = UIApplication.shared.isIdleTimerDisabled
            UIApplication.shared.isIdleTimerDisabled = true
        }
        .task { await model.appear() }
        .onDisappear {
            UIApplication.shared.isIdleTimerDisabled = idleTimerWasDisabled
            model.disappear()
        }
        .onChange(of: available) { _, new in
            if !new.isEmpty { lastTabs = new }
        }
        .onChange(of: model.ended) { _, ended in
            guard let ended else { return }
            // VoiceOver hears the reason when the panes are replaced.
            AccessibilityNotification.Announcement(
                L10n.Input.endNotice(ended, label: core.labelOf(peer: peerId))
            ).post()
        }
        .onChange(of: model.failure) { _, failure in
            guard let failure else { return }
            messages.show(failure)
            model.failureShown()
        }
    }

    /// The tabs and the pane of the open session.
    @ViewBuilder
    private func panes(tabs: [RemoteInputTab], caps: [String]) -> some View {
        if tabs.count > 1 {
            Picker(L10n.Input.title, selection: $tab) {
                ForEach(tabs, id: \.self) { t in
                    Text(Self.title(of: t)).tag(t)
                }
            }
            .pickerStyle(.segmented)
        }
        if !model.sessionReady {
            Label(L10n.Input.connecting, systemImage: "antenna.radiowaves.left.and.right")
                .font(.footnote)
                .foregroundStyle(.secondary)
        }
        switch tabs.contains(tab) ? tab : (tabs.first ?? .trackpad) {
        case .trackpad:
            TrackpadPane(model: model)
        case .keyboard:
            KeyboardPane(model: model)
        case .presenter:
            PresenterPane(model: model, volumeKeys: caps.contains(PeerCapability.inputKey) || lastTabs.contains(.keyboard))
        }
    }

    /// The reason the session is over, with the way back.
    private func endedContent(_ ended: InputEndNotice) -> some View {
        VStack(spacing: 16) {
            Spacer(minLength: 0)
            Image(systemName: ended.isRefusal ? "hand.raised" : "cursorarrow.slash")
                .font(.largeTitle)
                .foregroundStyle(.secondary)
                .accessibilityHidden(true)
            Text(L10n.Input.endedTitle).font(.headline)
            Text(L10n.Input.endNotice(ended, label: core.labelOf(peer: peerId)))
                .multilineTextAlignment(.center)
                .foregroundStyle(.secondary)
            Button(L10n.back) { dismiss() }
                .buttonStyle(.borderedProminent)
            Spacer(minLength: 0)
        }
        .frame(maxWidth: .infinity)
    }

    private var optionsPopover: some View {
        @Bindable var settings = settings
        return VStack(alignment: .leading, spacing: 8) {
            Text(L10n.Input.sensitivity).font(.headline)
            Slider(value: $settings.trackpadSensitivity, in: TrackpadGestures.sensitivityRange) {
                Text(L10n.Input.sensitivity)
            }
            .onChange(of: settings.trackpadSensitivity) { _, value in model.setSensitivity(value) }
        }
        .padding()
        .frame(minWidth: 260)
    }

    private static func title(of tab: RemoteInputTab) -> String {
        switch tab {
        case .trackpad: return L10n.Input.trackpad
        case .keyboard: return L10n.Input.keyboard
        case .presenter: return L10n.Input.presenter
        }
    }
}

/// Click feedback for the input panes.
enum InputHaptics {
    /// A light tap for a click or a button press.
    @MainActor
    static func click() {
        let generator = UIImpactFeedbackGenerator(style: .light)
        generator.impactOccurred()
    }

    /// Plays the click feedback when `outputs` contain a click or a button press.
    @MainActor
    static func play(for outputs: [TrackpadOutput]) {
        let clicked = outputs.contains { output in
            if case .button(_, let action) = output { return action != .up }
            return false
        }
        if clicked { click() }
    }
}

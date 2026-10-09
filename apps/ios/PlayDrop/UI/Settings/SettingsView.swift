import PlayDropCore
import PlayDropShared
import SwiftUI
import UIKit
import UserNotifications

// The Settings tab: this iPhone's name, the default device, where received files go, the
// clipboard and outbox switches, notifications, the port, Diagnostics and About. Name and
// port apply the next time the core starts (it restarts whenever the app comes back to the
// foreground). Port of Android `ui/settings/SettingsScreen.kt` without the Android-only
// battery and foreground-service parts.

/// The Settings tab.
struct SettingsView: View {
    @Environment(CoreHost.self) private var core
    @Environment(AppSettings.self) private var settings
    @Environment(MessageCenter.self) private var messages
    @Environment(\.openURL) private var openURL
    @Environment(\.scenePhase) private var scenePhase

    @State private var name = ""
    @State private var port = ""

    /// The project's public repository.
    static let repositoryURL = URL(string: "https://github.com/QasimBhb/PlayDrop")!

    var body: some View {
        @Bindable var settings = settings
        NavigationStack {
            Form {
                Section {
                    HStack {
                        TextField(L10n.Settings.name, text: $name)
                            .textInputAutocapitalization(.words)
                            .submitLabel(.done)
                            .onSubmit(saveName)
                        Button(L10n.save, action: saveName)
                            .disabled(Self.cleanName(name) == settings.deviceName || Self.cleanName(name).isEmpty)
                    }
                } header: {
                    Text(L10n.Settings.nameSection)
                } footer: {
                    Text(L10n.Settings.nameHint)
                }

                Section {
                    Picker(L10n.Settings.defaultDevice, selection: $settings.defaultPeer) {
                        Text(L10n.Settings.defaultNone).tag(String?.none)
                        ForEach(core.peers, id: \.deviceId) { p in
                            Text(verbatim: "\(p.label) (\(p.shortId))").tag(Optional(p.deviceId))
                        }
                    }
                } footer: {
                    Text(L10n.Settings.defaultHint)
                }

                Section {
                    Label(L10n.Settings.receiveHint, systemImage: "folder")
                        .font(.callout)
                    Toggle(isOn: $settings.pullOutboxOnOpen) {
                        VStack(alignment: .leading, spacing: 2) {
                            Text(L10n.Settings.pullOutbox)
                            Text(L10n.Settings.pullOutboxHint).font(.footnote).foregroundStyle(.secondary)
                        }
                    }
                } header: {
                    Text(L10n.Settings.receiveSection)
                }

                Section(L10n.Settings.clipboard) {
                    Toggle(isOn: $settings.applyIncomingClipboard) {
                        VStack(alignment: .leading, spacing: 2) {
                            Text(L10n.Settings.applyClipboard)
                            Text(L10n.Settings.applyClipboardHint).font(.footnote).foregroundStyle(.secondary)
                        }
                    }
                }

                notificationsSection

                Section {
                    HStack {
                        TextField(L10n.Settings.port, text: $port)
                            .keyboardType(.numberPad)
                            .font(.system(.body, design: .monospaced))
                        Button(L10n.save, action: savePort)
                            .disabled(port == String(settings.port))
                    }
                    NavigationLink {
                        DiagnosticsView()
                    } label: {
                        VStack(alignment: .leading, spacing: 2) {
                            Text(L10n.Diag.title)
                            Text(L10n.Settings.diagnosticsHint).font(.footnote).foregroundStyle(.secondary)
                        }
                    }
                } header: {
                    Text(L10n.Settings.network)
                } footer: {
                    Text(L10n.Settings.portHint)
                }

                aboutSection
            }
            .navigationTitle(L10n.Settings.title)
            .onAppear {
                name = settings.deviceName
                port = String(settings.port)
            }
            .task { await LocalNotifications.shared.refreshAuthorizationStatus() }
            .onChange(of: scenePhase) { _, phase in
                if phase == .active { Task { await LocalNotifications.shared.refreshAuthorizationStatus() } }
            }
        }
    }

    @ViewBuilder
    private var notificationsSection: some View {
        Section(L10n.Settings.notifications) {
            switch LocalNotifications.shared.authorizationStatus {
            case .authorized, .provisional, .ephemeral:
                Text(L10n.Settings.notificationsOn)
            default:
                HStack {
                    Text(L10n.Settings.notificationsOff)
                    Spacer()
                    Button(L10n.allow) { Task { await allowNotifications() } }
                }
            }
        }
    }

    private var aboutSection: some View {
        Section(L10n.Settings.about) {
            if let id = core.identity {
                LabeledContent(L10n.Common.shortId) { ShortIdText(shortId: id.shortId) }
                VStack(alignment: .leading, spacing: 4) {
                    Text(L10n.Settings.deviceId)
                    Text(verbatim: id.deviceId)
                        .font(.system(.caption, design: .monospaced))
                        .foregroundStyle(.secondary)
                        .textSelection(.enabled)
                }
            } else {
                Text(L10n.Add.identityUnknown).foregroundStyle(.secondary)
            }
            LabeledContent(L10n.Settings.appVersion, value: Self.appVersion)
            LabeledContent(L10n.Settings.coreVersion, value: coreVersion())
            Text(L10n.Settings.licence).font(.footnote).foregroundStyle(.secondary)
            Link(destination: Self.repositoryURL) {
                Label(L10n.Settings.sourceCode, systemImage: "chevron.left.forwardslash.chevron.right")
            }
        }
    }

    private func saveName() {
        let clean = Self.cleanName(name)
        guard !clean.isEmpty else {
            messages.show(.error(L10n.Settings.nameEmpty))
            return
        }
        guard clean != settings.deviceName else { return }
        settings.deviceName = clean
        name = clean
        messages.info(L10n.Settings.savedRestart)
    }

    private func savePort() {
        switch PortRules.check(port) {
        case .ok(let value):
            settings.port = value
            port = String(value)
            messages.info(L10n.Settings.savedRestart)
        case .invalid:
            messages.show(.error(L10n.Settings.portInvalid))
        case .reserved:
            messages.show(.error(L10n.Settings.portReserved))
        }
    }

    /// Asks when iOS has not asked yet; after a "Don't Allow" only Settings can change it.
    private func allowNotifications() async {
        if LocalNotifications.shared.authorizationStatus == .notDetermined {
            _ = await LocalNotifications.shared.requestPermission()
        } else if let url = URL(string: UIApplication.openNotificationSettingsURLString) {
            openURL(url)
        }
    }

    /// The name as `AppSettings` will store it (trimmed, sanitised, at most 63 bytes).
    static func cleanName(_ name: String) -> String { AppSettings.cleanDeviceName(name) }

    /// "1.0 (12)" from the bundle.
    static var appVersion: String {
        let info = Bundle.main.infoDictionary
        let version = info?["CFBundleShortVersionString"] as? String ?? "?"
        let build = info?["CFBundleVersion"] as? String ?? "?"
        return "\(version) (\(build))"
    }
}

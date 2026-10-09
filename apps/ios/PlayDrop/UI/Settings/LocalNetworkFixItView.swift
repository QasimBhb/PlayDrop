import PlayDropShared
import SwiftUI
import UIKit

// The Local Network fix-it screen. iOS asks for Local Network access once and offers no API
// to read the answer, so PlayDrop infers it (`LocalNetworkInference`) and, when it looks
// denied or the core could not start, explains what happened, where the switch is
// (Settings > Privacy & Security > Local Network > PlayDrop), opens Settings, and retries the
// start. Anti-pattern 18: a feature the OS blocks is explained, never silently broken.

/// Explains the Local Network permission and retries the start.
struct LocalNetworkFixItView: View {
    /// What PlayDrop saw: the start error or the inference reason, if any.
    let reason: String?

    @Environment(CoreHost.self) private var core
    @Environment(MessageCenter.self) private var messages
    @Environment(\.openURL) private var openURL
    @State private var retrying = false

    var body: some View {
        List {
            Section {
                Label {
                    Text(L10n.LocalNetwork.whatHappened)
                } icon: {
                    Image(systemName: "wifi.exclamationmark").foregroundStyle(.red)
                }
            }
            Section(L10n.LocalNetwork.howTo) {
                Text(verbatim: L10n.LocalNetwork.path)
                    .font(.body.weight(.semibold))
                Text(L10n.LocalNetwork.after)
                    .foregroundStyle(.secondary)
                Button {
                    if let url = URL(string: UIApplication.openSettingsURLString) { openURL(url) }
                } label: {
                    Label(L10n.LocalNetwork.openSettings, systemImage: "gear")
                }
            }
            Section {
                Button {
                    Task { await retry() }
                } label: {
                    HStack {
                        Text(L10n.tryAgain)
                        if retrying {
                            Spacer()
                            ProgressView()
                        }
                    }
                }
                .disabled(retrying)
                if core.running {
                    Label(L10n.LocalNetwork.nowRunning, systemImage: "checkmark.circle.fill")
                        .foregroundStyle(.green)
                }
            }
            if let detail = reason ?? core.startError {
                Section(L10n.LocalNetwork.detail) {
                    Text(verbatim: detail)
                        .font(.system(.footnote, design: .monospaced))
                        .textSelection(.enabled)
                }
            }
        }
        .navigationTitle(L10n.LocalNetwork.title)
        .navigationBarTitleDisplayMode(.inline)
    }

    /// Restarts the core the way a return to the foreground does (start, presence, outbox
    /// pull, queued shares), so a fixed permission takes effect without reopening the app.
    private func retry() async {
        retrying = true
        defer { retrying = false }
        if core.running { await core.stop() }
        await AppServices.sceneDidBecomeActive()
        await core.refreshDiagnostics()
        if core.running {
            messages.info(L10n.LocalNetwork.nowRunning)
        } else {
            messages.show(.error(L10n.Err.startFailed(core.startError ?? L10n.notRunningBanner)))
        }
    }
}

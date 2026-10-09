import AppIntents
import Foundation
import PlayDropCore

// The "Send Clipboard" Shortcut action. iOS lets an app read the pasteboard only while it is
// in the foreground, and the core runs only then, so the action opens the app
// (`openAppWhenRun`), waits briefly for the core, pushes the current clipboard through
// `ClipboardSender` and reports the outcome in the dialog (anti-pattern 18: a push that did
// not happen says why). The clipboard text itself is never shown or logged (SR-32).

/// Sends the current clipboard to every eligible device, or to one chosen device.
struct SendClipboardIntent: AppIntent {
    /// Action name in Shortcuts.
    static let title: LocalizedStringResource = "Send Clipboard"
    /// Action description in Shortcuts.
    static let description: IntentDescription? = IntentDescription(
        "Sends what is on this iPhone's clipboard to your paired devices. PlayDrop opens briefly, because iOS lets an app read the clipboard only while it is on screen."
    )
    /// The pasteboard is readable only with the app in the foreground, and the core must run.
    static let openAppWhenRun = true

    /// The device to send to; every device with the clipboard grant when empty.
    @Parameter(title: "Device", description: "Leave empty to send to every device that accepts this phone's clipboard.")
    var device: DeviceEntity?

    /// How long to wait for the core after the app opens.
    private static let startTimeout: Duration = .seconds(5)
    private static let pollInterval: Duration = .milliseconds(100)

    /// Runs the push and returns the outcome as a dialog.
    @MainActor
    func perform() async throws -> some IntentResult & ProvidesDialog {
        guard await Self.waitForCore() else {
            return .result(dialog: "PlayDrop did not start in time. Open PlayDrop and try again.")
        }
        do {
            let push = try await ClipboardSender.pushCurrent(to: device?.id)
            let outcome = ClipboardIntentOutcome.of(push, pairedCount: CoreHost.shared.peers.count)
            return .result(dialog: "\(outcome.message)")
        } catch {
            return .result(dialog: "Could not send: \(UserFacingError.reason(error))")
        }
    }

    /// Polls `CoreHost.shared.running` until it is true or the timeout passes.
    @MainActor
    private static func waitForCore() async -> Bool {
        let clock = ContinuousClock()
        let deadline = clock.now.advanced(by: startTimeout)
        while !CoreHost.shared.running {
            if clock.now >= deadline { return false }
            do {
                try await Task.sleep(for: pollInterval)
            } catch {
                return CoreHost.shared.running
            }
        }
        return true
    }
}

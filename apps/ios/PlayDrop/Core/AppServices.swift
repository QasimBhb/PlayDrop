import Foundation
import UIKit

// Wires the app's long-lived services to the scene lifecycle. `App/PlayDropApp.swift` calls
// `start()` once at launch and the two scene hooks on every phase change.
//
// - `start()`: the outgoing drain (first, so it is subscribed before the first
//   `peerOnline`), the peer directory mirror, notifications router, find-my-phone ringer,
//   clipboard applier, outbox puller and battery monitoring subscribe to
//   `CoreHost.shared.events()` once per process, and picks left in the photo staging area
//   by a crash are swept. The event streams outlive core restarts, so nothing re-subscribes
//   on foreground.
// - Foreground: start the core, report battery and presence `active` (plus a network nudge
//   so dead sessions are redialled), pull outboxes from peers already online, then send what
//   the share extension queued. Until onboarding is done the core is not started: starting
//   it makes iOS ask for Local Network access, and that question must come after the
//   onboarding page that explains it. `onboardingDidFinish()` starts it then.
// - Remote input: an open input screen stops its session on background and starts it
//   again once the core runs in the foreground.
// - Background: presence `asleep` ("background"), stop ringing, drop offer notifications
//   whose buttons would no longer work, stop the core. iOS suspends the app soon after, so
//   the stop runs inside a background task assertion to finish the shutdown cleanly. The
//   app never claims to receive in the background (PLAN.md anti-pattern 11).

/// Process-wide service wiring; see the file comment.
@MainActor
public enum AppServices {
    private static var started = false
    /// True between `sceneDidBecomeActive` and `sceneDidEnterBackground`.
    private static var sceneActive = false

    /// Starts the event subscribers. Idempotent; call once at launch.
    public static func start() {
        guard !started else {
            return
        }
        started = true
        // The drain first: its refresh subscribes to events before anything can start the
        // core, so it never misses the `peerOnline` that should send a queued batch.
        OutgoingDrain.shared.refresh()
        PeerDirectoryMirror.shared.startObserving()
        LocalNotifications.shared.start()
        FindRinger.shared.start()
        ClipboardApplier.start()
        OutboxPuller.start()
        DeviceSignals.start()
        PhotoExport.sweepStale()
    }

    /// Onboarding just finished: start the core now if the scene is in front (the scene
    /// hook skipped it while onboarding was showing).
    public static func onboardingDidFinish() {
        guard sceneActive else {
            return
        }
        Task { await sceneDidBecomeActive() }
    }

    /// The scene came to the foreground: start the core and catch up.
    public static func sceneDidBecomeActive() async {
        start()
        sceneActive = true
        guard AppSettings.shared.onboardingDone else {
            // The Local Network prompt waits for the onboarding page that explains it.
            return
        }
        do {
            try await CoreHost.shared.start()
        } catch {
            // `CoreHost.startError` carries the message for the fix-it screen.
            Log.services.error("core start failed: \(Log.describe(error), privacy: .public)")
            return
        }
        await DeviceSignals.reportBattery()
        await CoreHost.shared.setForeground(true)
        await RemoteInputModel.sceneDidBecomeActive()
        if AppSettings.shared.pullOutboxOnOpen {
            await OutboxPuller.pullAll()
        }
        await OutgoingDrain.shared.drainIfPossible()
    }

    /// The scene went to the background: say goodbye to peers and stop the core.
    public static func sceneDidEnterBackground() async {
        sceneActive = false
        let assertion = BackgroundAssertion(name: "PlayDrop core shutdown")
        defer { assertion.end() }
        FindRinger.shared.stop()
        // An open remote input session ends now: the core stops below, and the computer
        // must not wait for its idle timeout to release held keys.
        await RemoteInputModel.sceneDidEnterBackground()
        await CoreHost.shared.setForeground(false)
        LocalNotifications.shared.appDidEnterBackground()
        await CoreHost.shared.stop()
    }
}

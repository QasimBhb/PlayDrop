import AudioToolbox
import Foundation
import Observation
import PlayDropCore
import UIKit

// Answers "find my phone" (`HostEvent.find`) on iOS, the counterpart of Android's
// `service/FindRinger.kt`. Without this the event would be dropped silently, which PLAN.md
// anti-pattern 18 forbids.
//
// What iOS allows, said plainly: an app cannot raise the volume, override the silent switch
// or play an alarm from the background, and the core only runs while PlayDrop is open. So
// the iPhone rings only while PlayDrop is in the foreground: system sound 1005 plus a
// warning haptic every 2 s, for at most 60 s, until the peer sends `find stop`, the user
// taps Stop on the root alert (`FindRingerAlert`), or the app goes to the background. With
// the silent switch on, only the haptic is felt. A `find start` that arrives while the app
// is not in front is ignored (the core is about to stop anyway).

/// Rings the iPhone for "find my phone"; see the file comment.
@MainActor
@Observable
final class FindRinger {
    /// The shared instance; `AppServices.start()` starts it.
    static let shared = FindRinger()

    /// The system sound played on each beat.
    static let soundId: SystemSoundID = 1005
    /// Gap between two beats.
    static let beatInterval: Duration = .seconds(2)
    /// Longest ring, matching Android's 60 s.
    static let maxDuration: Duration = .seconds(60)

    /// Hex Device ID of the peer that is looking for this iPhone, while ringing.
    private(set) var ringingPeer: String?

    /// What the alert shows for that peer: its local label, else its Short ID (SR-8).
    private(set) var ringingLabel: String?

    /// True while ringing.
    var isRinging: Bool { ringingPeer != nil }

    @ObservationIgnored private var started = false
    @ObservationIgnored private var eventTask: Task<Void, Never>?
    @ObservationIgnored private var ringTask: Task<Void, Never>?
    /// Bumped on every start and stop, so a ring loop that was replaced never clears the
    /// state of the one that replaced it.
    @ObservationIgnored private var generation = 0

    private init() {}

    /// Subscribes to host events. Idempotent.
    func start() {
        guard !started else { return }
        started = true
        let events = CoreHost.shared.events()
        eventTask = Task { @MainActor [weak self] in
            for await event in events {
                guard case let .find(peer, start) = event else { continue }
                if start {
                    self?.ring(for: peer)
                } else {
                    // Like Android, any `find stop` silences the phone.
                    self?.stop()
                }
            }
        }
    }

    /// Stops ringing. Called by the Stop button, by `find stop` and when the app goes to
    /// the background.
    func stop() {
        generation += 1
        ringTask?.cancel()
        ringTask = nil
        if ringingPeer != nil {
            Log.notify.info("find: stopped")
        }
        ringingPeer = nil
        ringingLabel = nil
    }

    /// Starts ringing for `peer`, or restarts the 60 s limit when already ringing.
    private func ring(for peer: String) {
        guard UIApplication.shared.applicationState != .background else {
            Log.notify.info("find: ignored while in the background")
            return
        }
        generation += 1
        let mine = generation
        ringTask?.cancel()
        ringingPeer = peer
        ringingLabel = CoreHost.shared.labelOf(peer: peer)
        Log.notify.info("find: ringing")
        ringTask = Task { @MainActor [weak self] in
            let feedback = UINotificationFeedbackGenerator()
            feedback.prepare()
            let deadline = ContinuousClock.now + Self.maxDuration
            while !Task.isCancelled, ContinuousClock.now < deadline {
                AudioServicesPlaySystemSound(Self.soundId)
                feedback.notificationOccurred(.warning)
                try? await Task.sleep(for: Self.beatInterval)
            }
            guard let self, self.generation == mine else { return }
            self.stop()
        }
    }
}

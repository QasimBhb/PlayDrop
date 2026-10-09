import Foundation
import PlayDropCore
import UIKit

// Reports this iPhone's battery and presence to the core, which forwards them to peers
// (protocol.md 5.8 and 5.10; the core rate-limits battery to once per 60 s unless charging
// or low changes). Port of Android's `service/DeviceSignals.kt`, reduced to what iOS
// exposes to a foreground app:
// - Battery: `UIDevice` battery monitoring, pushed right after each core start (by
//   `AppServices`; the core repeats it to every new session) and on every level or state
//   notification while the core runs. Low means 20 % or less and not charging. An unknown
//   level (the simulator) is not reported.
// - Presence: `active` when the scene comes to the foreground, `asleep` with reason
//   "background" just before the core stops, so peers show the iPhone as asleep instead of
//   timing it out. iOS gives a foreground app no lock or Doze signal of its own beyond that.

/// Battery and presence reporting; see the file comment.
@MainActor
enum DeviceSignals {
    /// Battery percentage at or below which the battery counts as low when not charging.
    static let lowPercent = 20

    private static var started = false
    private static var observers: [NSObjectProtocol] = []

    /// Starts battery monitoring and the battery notifications. Idempotent; called once by
    /// `AppServices.start()`.
    static func start() {
        guard !started else {
            return
        }
        started = true
        UIDevice.current.isBatteryMonitoringEnabled = true
        let center = NotificationCenter.default
        for name in [UIDevice.batteryLevelDidChangeNotification, UIDevice.batteryStateDidChangeNotification] {
            let token = center.addObserver(forName: name, object: nil, queue: .main) { _ in
                Task { @MainActor in await reportBattery() }
            }
            observers.append(token)
        }
    }

    /// Pushes the current battery level and charging state when the level is known. Quietly
    /// does nothing while the core is stopped.
    static func reportBattery() async {
        let device = UIDevice.current
        let level = device.batteryLevel
        guard level >= 0 else {
            return
        }
        let percent = min(100, max(0, Int((level * 100).rounded())))
        let charging = device.batteryState == .charging || device.batteryState == .full
        let low = percent <= lowPercent && !charging
        do {
            try await CoreHost.shared.withHost {
                $0.setBattery(level: UInt8(percent), charging: charging, low: low)
            }
        } catch {
            Log.signals.debug("battery report skipped: \(Log.describe(error), privacy: .public)")
        }
    }

    /// Reports `active` (foreground) or `asleep` with reason "background". Awaited by
    /// `CoreHost.setForeground`, so the asleep frame is queued before the core stops.
    static func reportPresence(foreground: Bool) async {
        let state: PresenceState = foreground ? .active : .asleep
        let reason: String? = foreground ? nil : "background"
        do {
            try await CoreHost.shared.withHost {
                $0.setPresence(state: state, reason: reason, expectBackMs: nil)
            }
        } catch {
            Log.signals.debug("presence report skipped: \(Log.describe(error), privacy: .public)")
        }
    }
}

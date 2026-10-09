import Foundation
import PlayDropCore

// What a "Send Clipboard" run achieved, reduced to the one sentence Shortcuts shows. A port
// of Android's `share/ClipboardOutcome.kt` with the same wording, kept pure so the rules are
// easy to check by reading.

/// The result of one manual clipboard push.
enum ClipboardIntentOutcome: Equatable {
    /// Sent to this many devices.
    case sent(Int)
    /// The echo rule skipped it: the clip came from a peer or was just sent.
    case alreadySent
    /// No paired device at all.
    case noPairedDevice
    /// Devices are online but none has the clipboard grant for this phone.
    case noGrant
    /// No eligible device is online.
    case noDeviceOnline

    /// Picks the outcome from the core's counters.
    static func of(_ push: ClipboardPush, pairedCount: Int) -> ClipboardIntentOutcome {
        if !push.sentTo.isEmpty { return .sent(push.sentTo.count) }
        if push.skippedEcho { return .alreadySent }
        if pairedCount == 0 { return .noPairedDevice }
        if push.skippedNoGrant > 0 && push.skippedOffline == 0 { return .noGrant }
        return .noDeviceOnline
    }

    /// The sentence to show.
    var message: String {
        switch self {
        case .sent(let n):
            return n == 1 ? "Sent to 1 device" : "Sent to \(n) devices"
        case .alreadySent:
            return "Already sent"
        case .noPairedDevice:
            return "No paired devices. Open PlayDrop to add one."
        case .noGrant:
            return "No device accepts this phone's clipboard. Turn it on in the device settings."
        case .noDeviceOnline:
            return "No device online"
        }
    }
}

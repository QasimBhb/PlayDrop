import Foundation
import PlayDropCore
import PlayDropShared

// The one place that words a failed transfer for the user. The Transfers rows and the
// "failed" notification both come here, so no HTTP status or wire code ("507 Insufficient
// Storage no_space") reaches the screen. The logic is `TransferFailureWording` in
// PlayDropShared; this file adds the sentences (`L10n.Transfers.failure`) and reads the core's
// own failure code (`TransferFailure`, `playdrop-ffi` 0.6.0).

extension TransferFailureCode {
    /// The package's code for the core's `TransferFailure`. Exhaustive on purpose: a new core
    /// reason must be handled here before the app builds.
    init(_ failure: TransferFailure) {
        switch failure {
        case .noSpace: self = .noSpace
        case .declined: self = .declined
        case .tooLarge: self = .tooLarge
        case .rateLimited: self = .rateLimited
        case .cancelledByPeer: self = .cancelledByPeer
        case .cancelledByYou: self = .cancelledByYou
        case .integrity: self = .integrity
        case .connectionLost: self = .connectionLost
        case .timedOut: self = .timedOut
        case .other: self = .other
        }
    }
}

/// Words a failed transfer; see the file comment.
enum TransferFailureText {
    /// What a failed or cancelled transfer says, or nil when there is nothing to say. `failure`
    /// is the core's code when it sent one, `text` its English reason. `receive` is our side:
    /// it picks "this iPhone" or "the other device" in the sentence.
    static func sentence(failure: TransferFailure?, text: String?, receive: Bool) -> String? {
        switch TransferFailureWording.resolve(code: failure.map(TransferFailureCode.init), text: text) {
        case .known(let known)?: return L10n.Transfers.failure(known, receive: receive)
        case .plain(let plain)?: return plain
        case nil: return nil
        }
    }

    /// The line a Transfers row shows under the size: the failure sentence for a failed or
    /// cancelled transfer, the sender's note as it is for any other state.
    static func rowNote(_ snapshot: TransferSnapshot) -> String? {
        guard snapshot.state == .failed || snapshot.state == .cancelled else { return snapshot.note }
        return sentence(failure: snapshot.failure, text: snapshot.note, receive: snapshot.direction == .receive)
    }

    /// The words for a `transferFailed` event (a notification body, a batch's last error);
    /// never empty.
    static func message(failure: TransferFailure, reason: String, direction: TransferDirection) -> String {
        sentence(failure: failure, text: reason, receive: direction == .receive)
            ?? L10n.Transfers.failure(.other, receive: direction == .receive)
    }
}

extension L10n.Transfers {
    /// A plain sentence for why a transfer did not complete. `receive` is true on the
    /// receiving side, where "the other device" would be the sender.
    static func failure(_ code: TransferFailureCode, receive: Bool) -> String {
        switch code {
        case .noSpace:
            return receive
                ? String(localized: "ui_fail_no_space_receive", defaultValue: "This iPhone does not have enough free space.")
                : String(localized: "ui_fail_no_space_send", defaultValue: "The other device does not have enough free space.")
        case .declined:
            return receive
                ? String(localized: "ui_fail_declined_receive", defaultValue: "The files were declined.")
                : String(localized: "ui_fail_declined_send", defaultValue: "The other device declined the files.")
        case .tooLarge:
            return receive
                ? String(localized: "ui_fail_too_large_receive", defaultValue: "The files are too large for this iPhone to accept.")
                : String(localized: "ui_fail_too_large_send", defaultValue: "The files are too large for the other device to accept.")
        case .rateLimited:
            return receive
                ? String(localized: "ui_fail_busy_receive", defaultValue: "That device is sending too many transfers at once. Try again shortly.")
                : String(localized: "ui_fail_busy_send", defaultValue: "The other device is busy with other transfers from this iPhone. Try again shortly.")
        case .cancelledByPeer:
            return String(localized: "ui_fail_cancelled_peer", defaultValue: "The other device cancelled it.")
        case .cancelledByYou:
            return String(localized: "ui_fail_cancelled_you", defaultValue: "You cancelled it.")
        case .integrity:
            return String(localized: "ui_fail_integrity", defaultValue: "The files did not arrive intact.")
        case .connectionLost:
            return String(localized: "ui_fail_connection_lost", defaultValue: "The connection to the other device was lost.")
        case .timedOut:
            return String(localized: "ui_fail_timed_out", defaultValue: "The other device did not answer in time.")
        case .other:
            return String(localized: "ui_fail_other", defaultValue: "Something went wrong.")
        }
    }
}

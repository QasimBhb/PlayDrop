// What the phone does when the computer says why a remote input session ended
// (`HostEvent.inputSessionEnded`, protocol.md 5.12, `playdrop-ffi` 0.6.0). The core's reason
// code is mirrored here as `InputEndCode` (this package cannot import the generated bindings;
// the app maps one to the other with an exhaustive switch, so a new code stops the build).
//
// Two kinds of end exist. A lost connection is not the end of the screen: the session stops
// and starts again by itself when the computer is back, with nothing shown. Every other code
// closes the session for good and the screen says why in a sentence that stays until the user
// goes back, because a message that fades is what left people guessing before.
import Foundation

/// Why a session we control ended or never started; one case per core `InputEndReason`.
public enum InputEndCode: Equatable, Sendable, CaseIterable {
    /// The computer has remote input turned off for this iPhone.
    case grantOff
    /// The computer is locked.
    case locked
    /// The computer accepts no remote input at all.
    case notAccepted
    /// Someone pressed Stop (or the kill switch) on the computer.
    case stoppedByUser
    /// No input for ten minutes.
    case idleTimeout
    /// The connection to the computer ended, or there was none.
    case connectionLost
    /// Anything else, such as an older computer with its own message.
    case other
}

/// A reason the screen shows. The wording lives in the app's strings.
public enum InputEndNotice: Equatable, Sendable {
    /// Turn remote input on for this iPhone on the computer.
    case grantOff
    /// Unlock the computer.
    case locked
    /// The computer cannot be controlled remotely.
    case notAccepted
    /// Stopped on the computer.
    case stoppedByUser
    /// Ended after ten idle minutes.
    case idleTimeout
    /// Any other end; the computer's own words when it gave some.
    case other(reason: String?)

    /// The computer will not take input from this iPhone until something changes there (as
    /// opposed to a session that simply ended).
    public var isRefusal: Bool {
        switch self {
        case .grantOff, .notAccepted: return true
        case .locked, .stoppedByUser, .idleTimeout, .other: return false
        }
    }
}

/// What to do about an end.
public enum InputEndHandling: Equatable, Sendable {
    /// Nothing visible: the session resumes by itself when the computer is online again.
    case resumeWhenBack
    /// Stop for good and show this until the user goes back.
    case close(InputEndNotice)
}

/// The rules; see the file comment.
public enum InputEndRules {
    /// Longest device text shown for `.other`, in characters.
    public static let maxReasonLength = 160

    /// What to do when a session ended with `code`. `reason` is the computer's own text and is
    /// used only for `.other`.
    public static func handling(code: InputEndCode, reason: String) -> InputEndHandling {
        switch code {
        case .connectionLost: return .resumeWhenBack
        case .grantOff: return .close(.grantOff)
        case .locked: return .close(.locked)
        case .notAccepted: return .close(.notAccepted)
        case .stoppedByUser: return .close(.stoppedByUser)
        case .idleTimeout: return .close(.idleTimeout)
        case .other: return .close(.other(reason: cleanReason(reason)))
        }
    }

    /// The notice after a bare `forbidden` answer, which says only that input is refused.
    /// A specific end that already arrived is kept: it tells more.
    public static func afterRefusalAnswer(existing: InputEndNotice?) -> InputEndNotice {
        existing ?? .grantOff
    }

    /// The device's text, trimmed and cut, or nil when nothing is left.
    static func cleanReason(_ reason: String) -> String? {
        let trimmed = reason.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return nil }
        return String(trimmed.prefix(maxReasonLength))
    }
}

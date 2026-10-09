import Foundation
import PlayDropCore

// Turns core and host errors into text: the case name for logs, and a sentence for the
// user (the Local Network fix-it screen, toasts, the Shortcut result). The core writes its
// `detail` strings for people and keeps them free of secrets, so they are shown as they
// are; only `PortHeld`, which carries a number, gets its own sentence.

/// Text for `PlayDropError` and `CoreHostError` values.
public enum ErrorText {
    /// A sentence for the user describing `error`.
    public static func message(for error: Error) -> String {
        if let e = error as? PlayDropError {
            if case .PortHeld(let port) = e {
                return String(localized: "Port \(port) is already in use by another app. Close it or pick another port in Settings.")
            }
            return detail(of: e)
        }
        if let e = error as? CoreHostError {
            switch e {
            case .notRunning:
                return String(localized: "PlayDrop is not running. Open PlayDrop and try again.")
            case .startFailed(let message):
                return message
            }
        }
        return String(localized: "Something went wrong (\(String(describing: type(of: error)))).")
    }

    /// The case name of `error`, for logs (`"Io"`, `"NotPaired"`, ...).
    public static func kind(of error: PlayDropError) -> String {
        switch error {
        case .NotPaired: return "NotPaired"
        case .Forbidden: return "Forbidden"
        case .Locked: return "Locked"
        case .NotFound: return "NotFound"
        case .TooLarge: return "TooLarge"
        case .PortHeld: return "PortHeld"
        case .InsecurePermissions: return "InsecurePermissions"
        case .NoSpace: return "NoSpace"
        case .PathInvalid: return "PathInvalid"
        case .PairRejected: return "PairRejected"
        case .Io: return "Io"
        case .Other: return "Other"
        }
    }

    /// The core's own message carried by `error`.
    public static func detail(of error: PlayDropError) -> String {
        switch error {
        case .NotPaired(let d), .Forbidden(let d), .Locked(let d), .NotFound(let d),
             .TooLarge(let d), .InsecurePermissions(let d), .NoSpace(let d),
             .PathInvalid(let d), .PairRejected(let d), .Io(let d), .Other(let d):
            return d
        case .PortHeld(let port):
            return "port \(port) is held by another process"
        }
    }
}

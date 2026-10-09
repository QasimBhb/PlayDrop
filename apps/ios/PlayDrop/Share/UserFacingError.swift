import Foundation
import PlayDropCore

// One place that turns an error from the core, the host facade or the drain into a short
// reason a person can read, for the outgoing-queue drain and the Shortcut action. Core and
// host errors go through `ErrorText.message(for:)` (the core's own `detail`, plus a sentence
// for `PortHeld`, which carries a port number instead of a detail); the generated
// `PlayDropError.errorDescription` is a debug dump of the enum and is never shown. Only the
// drain's own errors are worded here. Never includes secrets (SR-32).

/// Short, readable reasons for errors; see the file comment.
enum UserFacingError {
    /// The reason to show for `error`.
    static func reason(_ error: any Error) -> String {
        if let drain = error as? DrainError {
            switch drain {
            case .alreadyStarting:
                return "These items are already being sent."
            case .noAppGroup:
                return "PlayDrop's shared folder is missing. Reinstall PlayDrop."
            }
        }
        if error is PlayDropError || error is CoreHostError {
            return ErrorText.message(for: error)
        }
        return error.localizedDescription
    }
}

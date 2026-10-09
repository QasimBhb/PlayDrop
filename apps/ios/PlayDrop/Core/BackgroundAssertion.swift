import UIKit

// A UIKit background task assertion with a scoped lifetime: it asks iOS for a little extra
// run time after the app leaves the foreground, so the core can say "asleep" to peers and
// shut down cleanly instead of being frozen mid-shutdown. Ended by `end()` or by iOS's
// expiration handler, whichever comes first; ending twice is harmless. This is not a way to
// keep the core alive in the background (PLAN.md anti-pattern 11).

/// One `beginBackgroundTask` assertion; see the file comment.
@MainActor
final class BackgroundAssertion {
    private var identifier: UIBackgroundTaskIdentifier = .invalid

    /// Begins the assertion under `name` (shown in debugging tools).
    init(name: String) {
        identifier = UIApplication.shared.beginBackgroundTask(withName: name) { [weak self] in
            MainActor.assumeIsolated {
                self?.end()
            }
        }
    }

    /// Ends the assertion. Idempotent.
    func end() {
        guard identifier != .invalid else {
            return
        }
        UIApplication.shared.endBackgroundTask(identifier)
        identifier = .invalid
    }
}

import Foundation
import PlayDropShared

// Handles `playdrop://` opens. Any app or web page can open this scheme, so it carries no
// commands at all (threat model T35, SR-30): host, path and query are ignored entirely, and
// the only effect is what opening the app does anyway, draining the outgoing queue the share
// extension filled. In particular a `playdrop://pair?...` link opened from outside is not a
// pairing request; QR pairing URIs are accepted only from the in-app scanner.

/// Entry point for `onOpenURL`; see the file comment.
@MainActor
enum URLSchemeHandler {
    /// Returns true when `url` uses our scheme (and starts a drain), false for anything else.
    @discardableResult
    static func handle(_ url: URL) -> Bool {
        guard url.scheme?.lowercased() == PlayDropIdentifiers.urlScheme else { return false }
        // SR-30: deliberately no look at url.host, url.path or url.query.
        let drain = OutgoingDrain.shared
        drain.refresh()
        Task { await drain.drainIfPossible() }
        return true
    }
}

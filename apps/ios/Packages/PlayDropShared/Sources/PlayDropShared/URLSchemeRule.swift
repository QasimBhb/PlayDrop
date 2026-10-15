import Foundation

// The `playdrop://` rule (threat model T35, SR-30). Any app or web page can open this
// scheme, so it carries no commands: opening it only opens the app and drains the share
// queue, which opening the app does anyway. The decision is therefore one question, "is
// this our scheme?", and nothing else about the URL is read.

/// Decides whether an opened URL is ours.
public enum URLSchemeRule {
    /// True when `url` uses the PlayDrop scheme, compared case-insensitively. The host,
    /// path, query and fragment are deliberately never read (SR-30, T35), so a link such
    /// as `playdrop://pair?id=...&secret=...` behaves exactly like `playdrop://`.
    public static func shouldHandle(_ url: URL) -> Bool {
        url.scheme?.lowercased() == PlayDropIdentifiers.urlScheme
    }
}

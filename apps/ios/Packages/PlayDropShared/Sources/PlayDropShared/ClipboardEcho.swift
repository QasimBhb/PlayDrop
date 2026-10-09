// Keeps a clip that arrived from a peer, and was written to this phone's pasteboard, from
// bouncing straight back to that peer. Ported from the Android app's
// `ui/clipboard/ClipboardEcho.kt`, with a time window instead of "last text forever", since
// on iOS the app only sees the pasteboard when it comes to the foreground: text the user
// copies again much later is a real new copy. Holds only the last applied text, in memory,
// and never logs it (SR-32).
import Foundation

/// The echo rule for incoming clipboard text.
public struct ClipboardEcho: Sendable {
    /// Text over this many UTF-8 bytes is not synced (SR-24).
    public static let maxBytes = 1024 * 1024

    /// How long after applying a clip the same text counts as an echo.
    public let window: TimeInterval
    /// The last text written to the pasteboard from a peer.
    private var lastApplied: String?
    /// When `lastApplied` was written.
    private var appliedAt: Date?

    /// An echo filter with the given window in seconds.
    public init(window: TimeInterval = 10) {
        self.window = window
    }

    /// Records that `appliedText` was just put on the pasteboard from a peer.
    public mutating func remember(appliedText: String, at: Date = Date()) {
        lastApplied = appliedText
        appliedAt = at
    }

    /// True when `text` is the clip applied within the last `window` seconds, so sending
    /// it would only echo it back.
    public func isEcho(_ text: String, at: Date = Date()) -> Bool {
        guard let lastApplied, let appliedAt, text == lastApplied else { return false }
        let age = at.timeIntervalSince(appliedAt)
        return age >= 0 && age <= window
    }
}

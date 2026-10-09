import Foundation
import Observation
import PlayDropCore

// A user-visible line of text: what a toast or banner says after an action, and the one
// place where core errors are turned into words (anti-pattern 18: every failure reaches the
// user as text). Port of Android `ui/common/UiMessage.kt`. Views post messages to the
// `MessageCenter` in the environment; `MessageBanner` shows the current one.

/// One message for the banner.
struct UiMessage: Identifiable, Equatable, Sendable {
    /// How the banner styles the message.
    enum Kind: Equatable, Sendable {
        /// Confirmation or progress ("Ping sent").
        case info
        /// Something failed.
        case error
    }

    /// Unique per message, so the same text posted twice shows twice.
    let id = UUID()
    /// The text, already localized.
    let text: String
    /// Info or error.
    let kind: Kind

    /// An informational message.
    static func info(_ text: String) -> UiMessage { UiMessage(text: text, kind: .info) }

    /// An error message.
    static func error(_ text: String) -> UiMessage { UiMessage(text: text, kind: .error) }

    /// The message for a failed call, or nil for task cancellation (which is never shown).
    static func forError(_ error: Error) -> UiMessage? {
        if error is CancellationError { return nil }
        return .error(text(for: error))
    }

    /// The words for `error` followed by the core's own detail when it adds something, for
    /// places where the exact reason matters (the Send sheet).
    static func textWithDetail(for error: Error) -> String {
        let text = text(for: error)
        guard let e = error as? PlayDropError else { return text }
        let detail = ErrorText.detail(of: e).trimmingCharacters(in: .whitespacesAndNewlines)
        if detail.isEmpty || text.contains(detail) { return text }
        return "\(text) (\(detail))"
    }

    /// The words for `error`. Details from the core are plain reasons (SR-32).
    static func text(for error: Error) -> String {
        if let e = error as? PlayDropError {
            switch e {
            case .NotPaired: return L10n.Err.notPaired
            case .Forbidden: return L10n.Err.forbidden
            case .Locked: return L10n.Err.locked
            case .NotFound: return L10n.Err.notFound
            case .TooLarge: return L10n.Err.tooLarge
            case .PortHeld(let port): return L10n.Err.portHeld(port)
            case .InsecurePermissions: return L10n.Err.insecure
            case .NoSpace: return L10n.Err.noSpace
            case .PathInvalid: return L10n.Err.pathInvalid
            case .PairRejected: return L10n.Err.pairRejected
            case .Io(let detail): return L10n.Err.io(detail)
            case .Other(let detail): return L10n.Err.other(detail)
            }
        }
        if let e = error as? CoreHostError {
            switch e {
            case .notRunning: return L10n.Err.notRunning
            case .startFailed(let detail): return L10n.Err.startFailed(detail)
            }
        }
        return L10n.Err.other(error.localizedDescription)
    }
}

/// Holds the message the banner shows and clears it after a few seconds. One instance per
/// window, created by `RootView` and put in the environment.
@MainActor
@Observable
final class MessageCenter {
    /// The message on screen, if any.
    private(set) var current: UiMessage?

    @ObservationIgnored private var dismissTask: Task<Void, Never>?

    /// Creates an empty center.
    init() {}

    /// Shows `message`, replacing the one on screen.
    func show(_ message: UiMessage) {
        current = message
        dismissTask?.cancel()
        let id = message.id
        let seconds: UInt64 = message.kind == .error ? 6 : 3
        dismissTask = Task { [weak self] in
            try? await Task.sleep(nanoseconds: seconds * 1_000_000_000)
            guard !Task.isCancelled else { return }
            self?.dismiss(id: id)
        }
    }

    /// Shows the words for `error`; cancellation is ignored.
    func show(error: Error) {
        if let m = UiMessage.forError(error) { show(m) }
    }

    /// Shows an informational line.
    func info(_ text: String) { show(.info(text)) }

    /// Removes the message on screen.
    func dismiss() {
        dismissTask?.cancel()
        current = nil
    }

    private func dismiss(id: UUID) {
        if current?.id == id { current = nil }
    }
}

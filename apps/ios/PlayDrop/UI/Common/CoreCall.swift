import Foundation

// The glue every screen uses to call the core: run the call, and on failure post the words
// for the error to the message center instead of failing silently (anti-pattern 18). Keeps
// try/catch boilerplate out of the views.

/// Runs core calls on behalf of a view and reports failures.
@MainActor
enum CoreCall {
    /// Runs `body`; returns its value, or nil after posting the error to `messages`.
    @discardableResult
    static func run<T: Sendable>(_ messages: MessageCenter, _ body: () async throws -> T) async -> T? {
        do {
            return try await body()
        } catch {
            messages.show(error: error)
            return nil
        }
    }

    /// Runs a call that returns nothing; true when it succeeded, false after posting the
    /// error to `messages`. Saves callers from testing a `Void?` against nil.
    @discardableResult
    static func succeeded(_ messages: MessageCenter, _ body: () async throws -> Void) async -> Bool {
        await run(messages, body) != nil
    }
}

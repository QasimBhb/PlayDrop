// The life of one remote input session (protocol.md 5.12, SR-25) as the phone sees it. The
// phone opens a session when the remote input screen appears and must close it when the
// screen goes away, when the app goes to the background (the core stops there, so the
// session would otherwise end only by timeout on the desktop) and when the peer goes
// offline. A refusal from the desktop (remote input off for this phone), or the desktop
// ending the session itself, ends the screen and never retries on its own: only the user
// opening the screen again starts a new attempt.
//
// The controller only decides when to send `session start` and `session stop`; sending is an
// injected closure, so the rules are tested here without the core. Every send goes through
// one serial chain, so a stop can never overtake the start it ends.
import Foundation

/// Where the session stands.
public enum InputSessionState: Equatable, Sendable {
    /// Not started yet.
    case idle
    /// A start was sent (or is on its way); input may be sent.
    case active
    /// Paused because the app went to the background or the peer went offline; `resume()`
    /// starts it again.
    case suspended
    /// The desktop refused remote input from this phone. Terminal until a new screen.
    case refused
    /// The desktop ended the session itself (it locked, saw no input for 10 minutes, or its
    /// user pressed Stop). Terminal until a new screen: never restarted on its own.
    case endedByPeer
    /// The screen closed. Terminal.
    case closed
}

/// What one start or stop attempt did.
public enum InputSessionOutcome: Equatable, Sendable {
    /// The frame was sent.
    case sent
    /// Nothing was sent: the state did not call for it.
    case skipped
    /// The core said the peer refuses remote input from us.
    case refused
    /// The send failed for another reason (described in words for the log or the user).
    case failed(String)
}

/// Decides when to start and stop the remote input session; see the file comment.
@MainActor
public final class InputSessionController {
    /// Sends `session start` (`true`) or `session stop` (`false`); throws on failure.
    public typealias Send = @MainActor @Sendable (Bool) async throws -> Void

    /// The current state.
    public private(set) var state: InputSessionState = .idle

    /// True while pointer, key and presenter frames may be sent.
    public var isActive: Bool { state == .active }

    private let send: Send
    private let isRefusal: @Sendable (Error) -> Bool
    private var chain: Task<Void, Never>?

    /// Creates a controller over `send`; `isRefusal` tells a refusal (the core's `Forbidden`)
    /// from any other failure.
    public init(send: @escaping Send, isRefusal: @escaping @Sendable (Error) -> Bool) {
        self.send = send
        self.isRefusal = isRefusal
    }

    /// The screen appeared: start once. A second call while active does nothing.
    @discardableResult
    public func open() async -> InputSessionOutcome {
        guard state == .idle else { return .skipped }
        return await start()
    }

    /// The app came back to the foreground or the peer came back online: start again if
    /// the session was suspended. Never restarts after a refusal or a close.
    @discardableResult
    public func resume() async -> InputSessionOutcome {
        guard state == .suspended else { return .skipped }
        return await start()
    }

    /// The app went to the background or the peer went offline: stop if active. Send errors
    /// are ignored (an offline peer cannot be told, and the desktop ends the session itself
    /// when the connection drops).
    @discardableResult
    public func suspend() async -> InputSessionOutcome {
        switch state {
        case .active:
            state = .suspended
            return await stop()
        case .idle:
            // Not started yet: start when resumed instead.
            state = .suspended
            return .skipped
        case .suspended, .refused, .endedByPeer, .closed:
            return .skipped
        }
    }

    /// The desktop refused remote input (a `peerRefused` event for an `input` frame). Nothing
    /// is sent: there is no session on the desktop to stop.
    public func refused() {
        guard state != .closed else { return }
        state = .refused
    }

    /// The desktop ended the session (an `inputSessionEnded` event). Nothing is sent: the
    /// desktop already closed it, and the phone does not start again by itself.
    public func endedByPeer() {
        guard state != .closed, state != .refused else { return }
        state = .endedByPeer
    }

    /// The screen went away: stop if active, then never send again.
    @discardableResult
    public func close() async -> InputSessionOutcome {
        let wasActive = state == .active
        guard state != .closed else { return .skipped }
        state = .closed
        return wasActive ? await stop() : .skipped
    }

    /// Waits until every queued start and stop has been sent.
    public func settle() async {
        await chain?.value
    }

    // MARK: - Sending

    private func start() async -> InputSessionOutcome {
        // Set before the await so a second call cannot start twice and a close that comes
        // in while the start is on its way still sends the stop.
        state = .active
        let outcome = await enqueue(true)
        switch outcome {
        case .refused:
            if state != .closed { state = .refused }
        case .failed:
            // Not refused, just not reachable now: try again on the next resume.
            if state == .active { state = .suspended }
        case .sent, .skipped:
            break
        }
        return outcome
    }

    private func stop() async -> InputSessionOutcome {
        await enqueue(false)
    }

    /// Runs one send after every earlier one.
    private func enqueue(_ start: Bool) async -> InputSessionOutcome {
        let previous = chain
        let send = self.send
        let isRefusal = self.isRefusal
        let task = Task { @MainActor () -> InputSessionOutcome in
            await previous?.value
            do {
                try await send(start)
                return .sent
            } catch {
                return isRefusal(error) ? .refused : .failed(String(describing: error))
            }
        }
        chain = Task { _ = await task.value }
        return await task.value
    }
}

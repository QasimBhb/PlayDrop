import Foundation
import PlayDropCore

// Sends one peer's remote input frames strictly in order. Every `CoreHost` call hops to a
// background task, and two hops started one after the other may finish in either order, so
// a button `up` could reach the core before its `down`. This type queues every frame and one
// task sends them one by one. Pointer moves need no extra coalescing here: the core already
// merges them to one frame per 8 ms.

/// The ordered frame queue of one remote input screen; see the file comment.
@MainActor
final class InputSender {
    /// Sends one frame to a peer.
    typealias Transport = @Sendable (String, InputEvent) async throws -> Void

    private struct Item: Sendable {
        let event: InputEvent
        /// Resumed once the frame was handed to the core, for callers that wait.
        let done: CheckedContinuation<Void, Error>?
    }

    private let continuation: AsyncStream<Item>.Continuation
    private var pump: Task<Void, Never>?

    /// A queue for `peer` over `transport`. `onError` hears failures of frames nobody waits
    /// for (posted with `post`).
    init(peer: String, transport: @escaping Transport, onError: @escaping @MainActor (Error) -> Void) {
        let (stream, continuation) = AsyncStream<Item>.makeStream(bufferingPolicy: .unbounded)
        self.continuation = continuation
        pump = Task { @MainActor in
            for await item in stream {
                do {
                    try await transport(peer, item.event)
                    item.done?.resume()
                } catch {
                    if let done = item.done {
                        done.resume(throwing: error)
                    } else {
                        onError(error)
                    }
                }
            }
        }
    }

    /// Queues `event` without waiting.
    func post(_ event: InputEvent) {
        continuation.yield(Item(event: event, done: nil))
    }

    /// Queues `event` and waits until it was sent; throws what the core threw.
    func send(_ event: InputEvent) async throws {
        try await withCheckedThrowingContinuation { (done: CheckedContinuation<Void, Error>) in
            if case .terminated = continuation.yield(Item(event: event, done: done)) {
                done.resume(throwing: CancellationError())
            }
        }
    }

    /// Sends what is queued, then stops. Later frames are dropped.
    func finish() {
        continuation.finish()
    }
}

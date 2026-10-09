import Foundation
import PlayDropCore

// The `HostListener` handed to `PlayDropHost.start`. The core calls `onEvent` on its own
// event thread, in order, and a slow listener delays every later event (the core drops
// events once its 4,096-entry queue fills). So this listener does nothing but yield the event
// into an unbounded `AsyncStream`, which never blocks; CoreHostImpl's router consumes that
// stream on the main actor in the same order.

/// Forwards each host event into an `AsyncStream` continuation; see the file comment.
final class HostEventForwarder: HostListener {
    private let continuation: AsyncStream<HostEvent>.Continuation

    /// A listener that yields into `continuation`.
    init(continuation: AsyncStream<HostEvent>.Continuation) {
        self.continuation = continuation
    }

    /// Queues `event`. Never throws and never blocks.
    func onEvent(event: HostEvent) throws {
        continuation.yield(event)
    }

    /// Ends the stream once the buffered events are consumed. Later events are dropped.
    func finish() {
        continuation.finish()
    }
}

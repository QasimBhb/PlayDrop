import Foundation
import PlayDropCore

// Fans every host event out to the services and views that called `CoreHost.events()`.
// Each subscriber gets its own `AsyncStream` that starts at the next event and keeps the
// newest 256 events when it falls behind (a slow view can lose old progress events but
// never blocks the router or other subscribers). Streams live for the whole process, across
// core restarts; a subscriber ends its stream by cancelling the task iterating it.
//
// The registry is guarded by a lock because a stream's termination handler runs on
// whatever thread cancels it.

/// Process-wide fan-out of `HostEvent`s; see the file comment.
final class EventBroadcaster: @unchecked Sendable {
    /// Events a lagging subscriber keeps before the oldest are dropped.
    static let bufferLimit = 256

    private let lock = NSLock()
    private var subscribers: [UUID: AsyncStream<HostEvent>.Continuation] = [:]

    /// An empty broadcaster.
    init() {}

    /// A new stream that receives every event broadcast from now on.
    func subscribe() -> AsyncStream<HostEvent> {
        let id = UUID()
        let (stream, continuation) = AsyncStream<HostEvent>.makeStream(
            bufferingPolicy: .bufferingNewest(Self.bufferLimit)
        )
        continuation.onTermination = { [weak self] _ in
            self?.remove(id)
        }
        lock.lock()
        subscribers[id] = continuation
        lock.unlock()
        return stream
    }

    /// Delivers `event` to every live subscriber without waiting for any of them.
    func broadcast(_ event: HostEvent) {
        lock.lock()
        let targets = Array(subscribers.values)
        lock.unlock()
        for continuation in targets {
            continuation.yield(event)
        }
    }

    /// Number of live subscribers, for diagnostics and tests.
    var subscriberCount: Int {
        lock.lock()
        defer { lock.unlock() }
        return subscribers.count
    }

    private func remove(_ id: UUID) {
        lock.lock()
        subscribers[id] = nil
        lock.unlock()
    }
}

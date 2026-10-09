import Foundation

// A first-in, first-out async lock on the main actor. CoreHostImpl runs `start()` and
// `stop()` through it so a restart never races a shutdown that still holds the port: a
// scene that flips foreground, background, foreground quickly queues start, stop, start and
// they run one after another to completion.

/// A FIFO async mutex for main-actor work; see the file comment.
@MainActor
final class SerialLifecycle {
    private var busy = false
    private var waiters: [CheckedContinuation<Void, Never>] = []

    /// An unlocked lock.
    init() {}

    /// Runs `body` once every earlier caller has finished.
    func run<T: Sendable>(_ body: () async throws -> T) async rethrows -> T {
        await acquire()
        defer { release() }
        return try await body()
    }

    private func acquire() async {
        if !busy {
            busy = true
            return
        }
        await withCheckedContinuation { waiters.append($0) }
    }

    private func release() {
        if waiters.isEmpty {
            busy = false
        } else {
            // Ownership passes straight to the next waiter; `busy` stays true.
            waiters.removeFirst().resume()
        }
    }
}

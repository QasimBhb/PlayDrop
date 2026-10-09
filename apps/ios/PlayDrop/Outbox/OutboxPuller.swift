import Foundation
import PlayDropCore

// Pulls what paired desktops and phones hold for this iPhone. iOS cannot receive in the
// background (PLAN.md anti-pattern 11), so peers queue sends in their outbox and the iPhone
// fetches them with `pullOutbox` once it is open again. The pulled items arrive as ordinary
// incoming transfers.
//
// When it pulls, all only while `AppSettings.pullOutboxOnOpen` is on:
// - `pullAll()` when the scene becomes active (peers already online),
// - `.peerOnline`: a session just came up after the app opened,
// - `.outboxReady`: the peer announced items.
// With the setting off, nothing is pulled automatically; the UI can still call `pull(peer:)`.
//
// Never two pulls of one peer at once: a request that arrives while that peer's pull runs
// is remembered and run once more afterwards, so items announced mid-pull are not missed.
// `NotPaired` and `NotFound` (the peer went away, nothing queued) are expected and ignored;
// anything else is logged.

/// Outbox pulling; see the file comment.
@MainActor
enum OutboxPuller {
    private static var started = false
    private static var inFlight: Set<String> = []
    private static var again: Set<String> = []
    private static var eventTask: Task<Void, Never>?

    /// Subscribes to host events. Idempotent; called once by `AppServices.start()`.
    static func start() {
        guard !started else {
            return
        }
        started = true
        let events = CoreHost.shared.events()
        eventTask = Task { @MainActor in
            for await event in events {
                guard AppSettings.shared.pullOutboxOnOpen else {
                    continue
                }
                switch event {
                case let .peerOnline(peer, _, _, _):
                    Task { @MainActor in await pull(peer: peer) }
                case let .outboxReady(peer, _, _, _):
                    Task { @MainActor in await pull(peer: peer) }
                default:
                    break
                }
            }
        }
    }

    /// Pulls from every paired peer that is online now, concurrently per peer.
    static func pullAll() async {
        let online = CoreHost.shared.peers.filter(\.online).map(\.deviceId)
        await withTaskGroup(of: Void.self) { group in
            for peer in online {
                // `pull` is main-actor isolated, so each child task hops there to run it.
                group.addTask { await pull(peer: peer) }
            }
        }
    }

    /// Pulls from `peer` (hex Device ID), unless a pull of that peer is already running, in
    /// which case it runs once more when that one ends.
    static func pull(peer: String) async {
        guard !inFlight.contains(peer) else {
            again.insert(peer)
            return
        }
        inFlight.insert(peer)
        defer { inFlight.remove(peer) }
        repeat {
            again.remove(peer)
            await pullOnce(peer: peer)
        } while again.contains(peer)
    }

    private static func pullOnce(peer: String) async {
        do {
            let ids = try await CoreHost.shared.withHost { try await $0.pullOutbox(peer: peer) }
            if !ids.isEmpty {
                let label = CoreHost.shared.labelOf(peer: peer)
                Log.outbox.info("pulled \(ids.count, privacy: .public) items from \(label, privacy: .public)")
            }
        } catch let error as PlayDropError {
            switch error {
            case .NotPaired, .NotFound:
                break
            default:
                Log.outbox.error("pull failed: \(Log.describe(error), privacy: .public)")
            }
        } catch CoreHostError.notRunning {
            // The app went to the background mid-pull; the next open pulls again.
        } catch {
            Log.outbox.error("pull failed: \(Log.describe(error), privacy: .public)")
        }
    }
}

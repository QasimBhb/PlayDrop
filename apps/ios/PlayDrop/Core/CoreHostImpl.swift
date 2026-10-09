import Foundation
import PlayDropCore
import PlayDropShared
import UIKit

// Backing implementation of `CoreHost`: owns the one `PlayDropHost` of the process, turns its
// events into `CoreHost.shared` state and fans them out to `events()` subscribers. Port of
// Android's `core/CoreHostImpl.kt`. Nothing but CoreHost.swift calls into this file.
//
// Concurrency design (a main-actor enum rather than an actor):
// - Every piece of state the UI observes lives on `CoreHost.shared`, which is main-actor
//   isolated, so the code that mutates it is main-actor isolated too. One place, one order,
//   no hops between a private actor and the main actor for every property write.
// - The core never runs on the main actor. `PlayDropHost.start` (blocking), every host
//   method and the config's file work run in detached tasks (`offMain`), and the router
//   awaits them, so the main thread only ever assigns results.
// - The core calls the listener on its own thread. `HostEventForwarder` only yields into an
//   unbounded stream; one main-actor task (`processor`) consumes it in order, updates state,
//   then broadcasts the event, so subscribers see state that already reflects the event.
//   The processor starts only once `PlayDropHost.start` has returned (events emitted during
//   start wait in the stream), so no handler or subscriber ever sees an event while
//   `withHost` would still throw `notRunning`.
// - `start()` and `stop()` run through `SerialLifecycle`, so a restart waits for a shutdown
//   that still holds the port.
// - Transfer progress is coalesced: at most one transfers refresh every 250 ms (4 Hz);
//   offers, completions and failures refresh at once.
// - While the "Add device" window is open, `addDeviceList()` and `addDeviceWindowOpen()` are
//   polled every 2 s: the core raises no event when its window times out.

/// See the file comment.
@MainActor
enum CoreHostImpl {
    // MARK: - Tunables

    /// Minimum gap between two transfer refreshes caused by progress events (4 Hz).
    static let progressRefreshInterval: Duration = .milliseconds(250)
    /// Poll period of the "Add device" window.
    static let addDevicePollInterval: Duration = .seconds(2)

    // MARK: - State

    private static var state: CoreHost { CoreHost.shared }
    private static let broadcaster = EventBroadcaster()
    private static let lifecycle = SerialLifecycle()
    private static let held = HeldOpeners()

    private static var host: PlayDropHost?
    private static var forwarder: HostEventForwarder?
    private static var processor: Task<Void, Never>?
    private static var loggingInitialised = false

    private static var transferRefreshTask: Task<Void, Never>?
    private static var lastTransferRefresh: ContinuousClock.Instant?

    private static var windowReasons: Set<DiscoveryReason> = []
    private static var windowWatch: Task<Void, Never>?

    // MARK: - Events

    /// See `CoreHost.events()`.
    static func events() -> AsyncStream<HostEvent> {
        broadcaster.subscribe()
    }

    // MARK: - Lifecycle

    /// See `CoreHost.start()`.
    static func start() async throws {
        try await lifecycle.run {
            guard host == nil else {
                return
            }
            try await startLocked()
        }
    }

    /// See `CoreHost.stop()`.
    static func stop() async {
        await lifecycle.run {
            await stopLocked()
        }
    }

    private static func startLocked() async throws {
        initLoggingOnce()
        let settings = AppSettings.shared
        let inputs = HostConfigInputs(
            deviceName: settings.deviceName,
            port: settings.port,
            deviceType: UIDevice.current.userInterfaceIdiom == .pad ? .tablet : .phone
        )

        let (stream, continuation) = AsyncStream<HostEvent>.makeStream(bufferingPolicy: .unbounded)
        let listener = HostEventForwarder(continuation: continuation)

        let started: PlayDropHost
        do {
            started = try await offMain {
                let config = try HostConfigFactory.make(inputs)
                return try PlayDropHost.start(config: config, listener: listener)
            }
        } catch {
            listener.finish()
            let message = ErrorText.message(for: error)
            state.startError = message
            Log.core.error("start failed: \(Log.describe(error), privacy: .public)")
            throw CoreHostError.startFailed(message)
        }

        host = started
        forwarder = listener
        state.startError = nil
        let snapshot = await offMainValue {
            (OwnIdentity(deviceId: started.deviceId(), shortId: started.shortId(), addr: started.localAddr()),
             started.peers(), started.transfers())
        }
        if state.identity == nil {
            state.identity = snapshot.0
        }
        state.peers = snapshot.1
        state.transfers = snapshot.2
        lastTransferRefresh = .now
        // `listening` is already queued; handling it sets `running`.
        processor = Task { @MainActor in
            for await event in stream {
                await handle(event)
            }
        }
        Log.core.info("started on \(snapshot.0.addr, privacy: .public)")
    }

    private static func stopLocked() async {
        guard let h = host else {
            return
        }
        state.running = false
        host = nil
        await offMainValue { await h.shutdown() }
        forwarder?.finish()
        forwarder = nil
        // Let the router drain what the core emitted before it stopped, so a restart starts
        // from settled state.
        await processor?.value
        processor = nil
        teardownState()
        Log.core.info("stopped")
    }

    /// Clears what only makes sense while the core runs.
    private static func teardownState() {
        transferRefreshTask?.cancel()
        transferRefreshTask = nil
        windowWatch?.cancel()
        windowWatch = nil
        windowReasons.removeAll()
        held.releaseAll()
        state.identity = nil
        state.addDeviceCandidates = []
        state.addDeviceWindowOpen = false
        state.pairingPrompt = nil
        state.pendingOffers = []
        state.peerSeenOnlineThisRun = false
        state.peers = state.peers.map { peer in
            var copy = peer
            copy.online = false
            return copy
        }
    }

    private static func initLoggingOnce() {
        guard !loggingInitialised else {
            return
        }
        loggingInitialised = true
        #if DEBUG
        let debug = true
        #else
        let debug = AppSettings.shared.logLevelDebug
        #endif
        initLogging(level: debug ? .debug : .info)
    }

    // MARK: - Host access

    /// See `CoreHost.withHost(_:)`.
    static func withHost<T: Sendable>(_ body: @Sendable @escaping (PlayDropHost) async throws -> T) async throws -> T {
        guard let h = host else {
            throw CoreHostError.notRunning
        }
        return try await offMain { try await body(h) }
    }

    /// Runs `work` in a detached task (never on the main actor) and forwards cancellation.
    private static func offMain<T: Sendable>(_ work: @Sendable @escaping () async throws -> T) async throws -> T {
        let task = Task.detached(priority: .userInitiated) { try await work() }
        return try await withTaskCancellationHandler {
            try await task.value
        } onCancel: {
            task.cancel()
        }
    }

    /// Non-throwing form of `offMain`, for host calls that cannot fail.
    private static func offMainValue<T: Sendable>(_ work: @Sendable @escaping () async -> T) async -> T {
        await Task.detached(priority: .userInitiated) { await work() }.value
    }

    // MARK: - Refreshes

    /// See `CoreHost.refreshPeers()`.
    static func refreshPeers() async {
        guard let h = host else {
            return
        }
        state.peers = await offMainValue { h.peers() }
    }

    /// See `CoreHost.refreshTransfers()`.
    static func refreshTransfers() async {
        guard let h = host else {
            return
        }
        lastTransferRefresh = .now
        state.transfers = await offMainValue { h.transfers() }
    }

    /// See `CoreHost.refreshDiagnostics()`.
    static func refreshDiagnostics() async {
        guard let h = host else {
            return
        }
        state.diagnostics = await offMainValue { h.diagnose() }
    }

    /// Schedules one transfers refresh no sooner than `progressRefreshInterval` after the
    /// previous one; further progress events until then are absorbed.
    private static func requestCoalescedTransferRefresh() {
        guard transferRefreshTask == nil else {
            return
        }
        let wait: Duration
        if let last = lastTransferRefresh {
            let elapsed = ContinuousClock.now - last
            wait = elapsed >= progressRefreshInterval ? .zero : progressRefreshInterval - elapsed
        } else {
            wait = .zero
        }
        transferRefreshTask = Task { @MainActor in
            if wait > .zero {
                try? await Task.sleep(for: wait)
            }
            transferRefreshTask = nil
            guard !Task.isCancelled else {
                return
            }
            await refreshTransfers()
        }
    }

    // MARK: - Add device window

    /// See `CoreHost.openAddDeviceWindow(reason:)`.
    static func openAddDeviceWindow(reason: DiscoveryReason) async throws {
        try await withHost { try $0.openAddDeviceWindow() }
        windowOpened(for: reason)
    }

    /// See `CoreHost.showQr()`.
    static func showQr() async throws -> String {
        let uri = try await withHost { try $0.showQr() }
        windowOpened(for: .showQr)
        return uri
    }

    /// See `CoreHost.closeAddDeviceWindow(reason:)`.
    static func closeAddDeviceWindow(reason: DiscoveryReason) async {
        windowReasons.remove(reason)
        guard windowReasons.isEmpty else {
            return
        }
        do {
            try await withHost { $0.closeAddDeviceWindow() }
        } catch {
            Log.events.debug("closing the add-device window: \(Log.describe(error), privacy: .public)")
        }
        windowClosed()
    }

    private static func windowOpened(for reason: DiscoveryReason) {
        windowReasons.insert(reason)
        state.addDeviceWindowOpen = true
        guard windowWatch == nil else {
            return
        }
        windowWatch = Task { @MainActor in
            while !Task.isCancelled {
                try? await Task.sleep(for: addDevicePollInterval)
                if Task.isCancelled {
                    break
                }
                guard await pollWindow() else {
                    windowClosed()
                    break
                }
            }
        }
    }

    /// Re-reads the window state and candidates; false once the core closed the window.
    private static func pollWindow() async -> Bool {
        guard let h = host else {
            return false
        }
        let (open, candidates) = await offMainValue { (h.addDeviceWindowOpen(), h.addDeviceList()) }
        if open {
            state.addDeviceCandidates = candidates
        }
        return open
    }

    private static func windowClosed() {
        windowWatch?.cancel()
        windowWatch = nil
        windowReasons.removeAll()
        state.addDeviceWindowOpen = false
        state.addDeviceCandidates = []
    }

    // MARK: - Answers

    /// See `CoreHost.answerPairing(accept:)`.
    static func answerPairing(accept: Bool) async {
        defer { state.pairingPrompt = nil }
        do {
            try await withHost { $0.pairingRespond(accept: accept) }
        } catch {
            Log.events.error("answering pairing: \(Log.describe(error), privacy: .public)")
        }
    }

    /// See `CoreHost.respondOffer(transferId:accept:)`.
    static func respondOffer(transferId: String, accept: Bool) async throws {
        defer { state.pendingOffers.removeAll { $0.transferId == transferId } }
        try await withHost { try await $0.respondOffer(id: transferId, accept: accept) }
    }

    // MARK: - Sending

    /// See `CoreHost.sendFiles(peer:items:note:)`.
    static func sendFiles(peer: String, items: [SendItem], note: String?) async throws -> String {
        guard host != nil else {
            throw CoreHostError.notRunning
        }
        let prepared = try await offMain { try PreparedSend.prepare(items) }
        let sources = prepared.sources
        let id: String
        do {
            id = try await withHost { try await $0.send(peer: peer, sources: sources, note: note) }
        } catch {
            prepared.release()
            throw error
        }
        held.hold(prepared.openers, for: id)
        Log.send.info("transfer \(String(id.prefix(8)), privacy: .public): \(items.count, privacy: .public) items")
        return id
    }

    // MARK: - Labels and foreground

    /// See `CoreHost.labelOf(peer:)`.
    static func labelOf(peer: String) -> String {
        let summary = state.peers.first { $0.deviceId == peer }
        if let label = summary?.label.trimmingCharacters(in: .whitespacesAndNewlines), !label.isEmpty {
            return label
        }
        return PeerLabel.fallback(deviceId: peer, shortId: summary?.shortId)
    }

    /// See `CoreHost.setForeground(_:)`.
    static func setForeground(_ foreground: Bool) async {
        guard host != nil else {
            return
        }
        await DeviceSignals.reportPresence(foreground: foreground)
        if foreground {
            do {
                try await withHost { $0.networkChanged() }
            } catch {
                Log.core.debug("network nudge: \(Log.describe(error), privacy: .public)")
            }
        }
    }

    // MARK: - Event routing

    /// Handles one event in order: state first, then subscribers. Never throws, so one bad
    /// event cannot stop the stream.
    private static func handle(_ event: HostEvent) async {
        await route(event)
        broadcaster.broadcast(event)
    }

    private static func route(_ event: HostEvent) async {
        switch event {
        case let .listening(addr, deviceId, shortId):
            state.identity = OwnIdentity(deviceId: deviceId, shortId: shortId, addr: addr)
            if host != nil {
                state.running = true
            }

        case .peerOnline:
            state.peerSeenOnlineThisRun = true
            await refreshPeers()

        case .peerOffline, .unpaired, .removedByPeer:
            await refreshPeers()

        case .paired:
            state.pairingPrompt = nil
            await refreshPeers()
            // A successful pairing closes the core's window; notice it now, not in 2 s.
            if state.addDeviceWindowOpen, !(await pollWindow()) {
                windowClosed()
            }

        case .pairingFailed:
            state.pairingPrompt = nil
            await refreshPeers()

        case let .pairingPrompt(peer, shortId, name, prompt):
            state.pairingPrompt = PendingPairing(peer: peer, shortId: shortId, name: name, prompt: prompt)

        case .addDeviceSeen:
            if let h = host {
                state.addDeviceCandidates = await offMainValue { h.addDeviceList() }
            }

        case let .transferOffer(id, peer, shortId, files, fileCount, totalBytes, note):
            if !state.pendingOffers.contains(where: { $0.transferId == id }) {
                state.pendingOffers.append(PendingOffer(
                    transferId: id, peer: peer, shortId: shortId, files: files,
                    fileCount: fileCount, totalBytes: totalBytes, note: note
                ))
            }
            await refreshTransfers()

        case .transferProgress:
            requestCoalescedTransferRefresh()

        case let .transferComplete(id, _, _, _, _, _, _, _):
            transferEnded(id)
            await refreshTransfers()

        case let .transferFailed(id, _, _, _):
            transferEnded(id)
            await refreshTransfers()

        case let .battery(peer, level, charging, low):
            var status = state.peerStatus[peer] ?? PeerStatus()
            status.batteryLevel = level
            status.charging = charging
            status.batteryLow = low
            state.peerStatus[peer] = status

        case let .presence(peer, presence):
            var status = state.peerStatus[peer] ?? PeerStatus()
            status.presence = presence
            state.peerStatus[peer] = status

        case let .discoveryDiagnostic(_, shortId, path, failure):
            Log.events.info("peer \(shortId, privacy: .public) not reached via \(String(describing: path), privacy: .public): \(failure, privacy: .public)")

        default:
            break
        }
    }

    private static func transferEnded(_ id: String) {
        held.transferEnded(id)
        state.pendingOffers.removeAll { $0.transferId == id }
    }
}

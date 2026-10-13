import Foundation
import Observation
import PlayDropCore
import PlayDropShared

// The one process-wide handle on the core. The app starts the core through `start()` when it
// comes to the foreground and stops it through `stop()` when it goes to the background
// (iOS suspends the process and marks its sockets defunct, so there is nothing to keep
// alive: PLAN.md anti-pattern 11, no always-on claims). Every view, the outgoing-queue
// drain, the outbox puller and the Shortcut action read state from the observable
// properties and call the core through `withHost`. Nothing else touches `PlayDropHost`.
//
// Contract between the core agent (implements `CoreHostImpl.swift`) and the UI agent
// (consumes this file). Keep the signatures; the core agent may add members, each with a
// doc comment. Mirrors `apps/android/.../core/CoreHost.kt`.

/// Why the "Add device" discovery window is wanted. iOS has no multicast lock; the window
/// is the core's own 120 s window, and this enum only records who asked for it.
public enum DiscoveryReason: Sendable {
    /// The "Add device" screen is open.
    case addDevice
    /// The QR screen is open.
    case showQr
}

/// A pairing prompt the user must answer (SR-8: show the Short ID next to the name).
public struct PendingPairing: Equatable, Sendable {
    public let peer: String
    public let shortId: String
    public let name: String
    public let prompt: PairingPrompt

    public init(peer: String, shortId: String, name: String, prompt: PairingPrompt) {
        self.peer = peer
        self.shortId = shortId
        self.name = name
        self.prompt = prompt
    }
}

/// An offer waiting for the user (the sender has no `files` grant). Answer with
/// `CoreHost.respondOffer`.
public struct PendingOffer: Equatable, Identifiable, Sendable {
    public var id: String { transferId }
    public let transferId: String
    public let peer: String
    public let shortId: String
    public let files: [OfferedFile]
    public let fileCount: UInt64
    public let totalBytes: UInt64
    public let note: String?

    public init(
        transferId: String, peer: String, shortId: String, files: [OfferedFile],
        fileCount: UInt64, totalBytes: UInt64, note: String?
    ) {
        self.transferId = transferId
        self.peer = peer
        self.shortId = shortId
        self.files = files
        self.fileCount = fileCount
        self.totalBytes = totalBytes
        self.note = note
    }
}

/// The last battery and presence a peer reported.
public struct PeerStatus: Equatable, Sendable {
    public var batteryLevel: UInt8?
    public var charging: Bool = false
    public var batteryLow: Bool = false
    /// `active`, `idle`, `locked` or `asleep`.
    public var presence: String?

    public init(batteryLevel: UInt8? = nil, charging: Bool = false, batteryLow: Bool = false, presence: String? = nil) {
        self.batteryLevel = batteryLevel
        self.charging = charging
        self.batteryLow = batteryLow
        self.presence = presence
    }
}

/// This device as the core reports it.
public struct OwnIdentity: Equatable, Sendable {
    public let deviceId: String
    public let shortId: String
    public let addr: String

    public init(deviceId: String, shortId: String, addr: String) {
        self.deviceId = deviceId
        self.shortId = shortId
        self.addr = addr
    }
}

/// Something the user picked to send: a file the app may read. Security-scoped URLs (from
/// the document picker) are opened through `FileOpener`; App Group files (from the share
/// extension) and exported photos are app-private and may go as plain paths.
public enum SendItem: Equatable, Sendable {
    /// A file inside the app's own sandbox or the App Group container.
    case privateFile(URL)
    /// A security-scoped URL from the document picker; stays readable only through its scope.
    case scopedFile(URL)
}

/// What a `CoreHost` call reports when the core is not running.
public enum CoreHostError: Error, Equatable, Sendable {
    /// `start()` has not completed, or `stop()` ran.
    case notRunning
    /// `start()` failed with this message (shown on the fix-it screen).
    case startFailed(String)
}

/// The one process-wide handle on the core; see the file comment.
@MainActor
@Observable
public final class CoreHost {
    /// The shared instance. Views read it from the environment; app services use it directly.
    public static let shared = CoreHost()

    /// True once `start()` returned and the listener saw `listening`.
    public internal(set) var running = false

    /// Why the last `start()` failed, until the next successful start. `PortHeld` and
    /// `Io` messages are shown on the Local Network fix-it screen.
    public internal(set) var startError: String?

    /// Our own identity once listening.
    public internal(set) var identity: OwnIdentity?

    /// Paired peers, refreshed on every peer event and by `refreshPeers()`.
    public internal(set) var peers: [PeerSummary] = []

    /// Live and recent transfers, refreshed on every transfer event (at most 4 times a second).
    public internal(set) var transfers: [TransferSnapshot] = []

    /// Devices seen in their "Add device" window, one row per address.
    public internal(set) var addDeviceCandidates: [AddDeviceCandidate] = []

    /// True while the core's "Add device" window is open.
    public internal(set) var addDeviceWindowOpen = false

    /// The pairing prompt waiting for an answer, if any.
    public internal(set) var pairingPrompt: PendingPairing?

    /// Offers waiting for an answer, oldest first.
    public internal(set) var pendingOffers: [PendingOffer] = []

    /// Battery and presence last reported by each peer, keyed by hex Device ID.
    public internal(set) var peerStatus: [String: PeerStatus] = [:]

    /// True once any peer came online since the core last started; cleared by `stop()`.
    /// iOS gives no API to read the Local Network permission, so this is the evidence
    /// `LocalNetworkInference.infer(from:peerOnlineThisRun:)` uses for "granted": a peer
    /// that went offline again still proves the network was reachable.
    public internal(set) var peerSeenOnlineThisRun = false

    /// The latest `diagnose()` snapshot; refreshed by `refreshDiagnostics()`.
    public internal(set) var diagnostics: Diagnostics?

    /// Media players each peer reported (protocol.md 5.9), keyed by hex Device ID, in the
    /// order they first appeared. A player that stops playing and allows no action is gone
    /// and removed (`MediaPlayback.isGone`); a peer's players are dropped when it goes
    /// offline and when the core stops.
    public internal(set) var mediaPlayers: [String: [MediaState]] = [:]

    /// Every event, in order, for services that need more than the properties above
    /// (notifications, the outbox puller, the clipboard applier). Each call returns an
    /// independent stream that starts at the next event.
    public func events() -> AsyncStream<HostEvent> { CoreHostImpl.events() }

    /// Starts the core if it is not running: builds the `HostConfig` (data dir under
    /// Application Support, `ReceiveTarget.directory` under Documents/PlayDrop, Keychain
    /// secret store, `getifaddrs` interfaces, `KeepAliveProfile.mobile`), then reconnects
    /// to paired peers. Runs the blocking start off the main actor. Sets `startError` and
    /// throws `CoreHostError.startFailed` on failure.
    public func start() async throws { try await CoreHostImpl.start() }

    /// Stops the core. Idempotent. Called when the scene goes to the background.
    public func stop() async { await CoreHostImpl.stop() }

    /// Runs `body` against the live host on a background executor. Throws
    /// `CoreHostError.notRunning` when the core is down.
    public func withHost<T: Sendable>(
        _ body: @Sendable @escaping (PlayDropHost) async throws -> T
    ) async throws -> T {
        try await CoreHostImpl.withHost(body)
    }

    /// Re-reads `peers` from the core. Grant and label changes raise no event, so call this
    /// after `setGrants`, `setLabel` or `unpair` through `withHost`.
    public func refreshPeers() async { await CoreHostImpl.refreshPeers() }

    /// Re-reads `transfers` from the core.
    public func refreshTransfers() async { await CoreHostImpl.refreshTransfers() }

    /// Re-reads `diagnostics` from the core.
    public func refreshDiagnostics() async { await CoreHostImpl.refreshDiagnostics() }

    /// Opens the core's 120 s "Add device" window. `addDeviceCandidates` fills as devices
    /// are seen and is cleared when the window closes (by `closeAddDeviceWindow`, a
    /// successful pairing, or the core's own timeout). Use this instead of
    /// `withHost { try $0.openAddDeviceWindow() }`.
    public func openAddDeviceWindow(reason: DiscoveryReason) async throws {
        try await CoreHostImpl.openAddDeviceWindow(reason: reason)
    }

    /// Closes the "Add device" window for `reason`; the core window closes when no reason
    /// remains, and the candidates are cleared.
    public func closeAddDeviceWindow(reason: DiscoveryReason) async {
        await CoreHostImpl.closeAddDeviceWindow(reason: reason)
    }

    /// Opens the window and returns the one-time QR pairing URI (SR-9: render it on a view
    /// that is hidden from screen capture, never log it). Same window bookkeeping as
    /// `openAddDeviceWindow(reason: .showQr)`.
    public func showQr() async throws -> String { try await CoreHostImpl.showQr() }

    /// Answers `pairingPrompt` and clears it. Use instead of `withHost { $0.pairingRespond(..) }`.
    public func answerPairing(accept: Bool) async { await CoreHostImpl.answerPairing(accept: accept) }

    /// Answers a pending offer and removes it from `pendingOffers`.
    public func respondOffer(transferId: String, accept: Bool) async throws {
        try await CoreHostImpl.respondOffer(transferId: transferId, accept: accept)
    }

    /// Sends `items` to `peer` (hex Device ID) and returns the transfer ID. Each item is
    /// turned into a `SendSource` (`path` for private files, `stream` with a `FileOpener`
    /// for scoped files) and kept readable until the transfer ends. Throws `PlayDropError`
    /// (for example `Io` naming an unreadable item) or `CoreHostError.notRunning`.
    public func sendFiles(peer: String, items: [SendItem], note: String? = nil) async throws -> String {
        try await CoreHostImpl.sendFiles(peer: peer, items: items, note: note)
    }

    /// Sends one remote input frame to `peer` (protocol.md 5.12). Throws `PlayDropError`:
    /// `NotFound` without a session, `Forbidden` when the peer does not accept that kind of
    /// input. A peer with remote input off for this phone answers later with a
    /// `peerRefused` event instead. Callers that need frames in order serialise their calls
    /// (`InputSender`).
    public func sendInput(peer: String, _ input: InputEvent) async throws {
        try await withHost { host in try host.sendInput(peer: peer, input: input) }
    }

    /// Sends a media command to one of `peer`'s players (protocol.md 5.9). `value` is the
    /// seek position in ms or the volume from 0 to 100. Throws `Forbidden` when the peer
    /// does not accept media control.
    public func sendMediaCommand(peer: String, player: String, action: MediaAction, value: UInt64? = nil) async throws {
        try await withHost { host in
            try host.sendMediaCommand(peer: peer, player: player, action: action, value: value)
        }
    }

    /// The local label of a paired peer, else its Short ID, else a truncated Device ID.
    public func labelOf(peer: String) -> String { CoreHostImpl.labelOf(peer: peer) }

    /// Tells the core the app is in the foreground (presence `active`, a `networkChanged()`
    /// nudge so dead sessions are redialled) or going to the background (presence
    /// `asleep`, then `stop()` is expected to follow).
    public func setForeground(_ foreground: Bool) async { await CoreHostImpl.setForeground(foreground) }

    nonisolated init() {}
}

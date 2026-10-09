import Foundation
import Observation
import PlayDropCore
import UIKit

// The actions a device row and the Device screen share: ping, find (ring), connect, set as
// default, unpair, send clipboard. One instance lives in the environment (created by
// `RootView`) so a ringing device shows "Stop ringing" wherever it appears. Every call goes
// through `CoreHost.withHost` and reports failures as words. Port of the action half of
// Android `DeviceDetailViewModel.kt`.

/// Shared device actions; see the file comment.
@MainActor
@Observable
final class DeviceActions {
    /// Devices this iPhone is currently ringing (a `find` start was sent and not stopped).
    private(set) var ringing: Set<String> = []

    @ObservationIgnored private let core: CoreHost
    @ObservationIgnored private let settings: AppSettings

    /// Creates the actions over `core` and `settings`, by default the shared ones.
    // Defaults are nil rather than `.shared`: a default argument is evaluated outside the
    // main actor, where the main-actor `shared` instances cannot be read.
    init(core: CoreHost? = nil, settings: AppSettings? = nil) {
        self.core = core ?? .shared
        self.settings = settings ?? .shared
    }

    /// Sends a ping with an optional message; the other device shows a notification.
    func ping(_ peer: String, message: String?, messages: MessageCenter) async {
        let text = message?.trimmingCharacters(in: .whitespacesAndNewlines)
        let note = (text?.isEmpty ?? true) ? nil : text
        let ok = await CoreCall.succeeded(messages) {
            try await self.core.withHost { host in try host.ping(peer: peer, message: note) }
        }
        if ok { messages.info(L10n.Detail.pingSent) }
    }

    /// Starts ringing `peer`, or stops when it is ringing already.
    func toggleFind(_ peer: String, messages: MessageCenter) async {
        let start = !ringing.contains(peer)
        let ok = await CoreCall.succeeded(messages) {
            try await self.core.withHost { host in try host.find(peer: peer, start: start) }
        }
        guard ok else { return }
        if start { ringing.insert(peer) } else { ringing.remove(peer) }
    }

    /// Stops ringing `peer` if it rings; errors are ignored (the device may be gone).
    func stopFindIfRinging(_ peer: String) async {
        guard ringing.contains(peer) else { return }
        ringing.remove(peer)
        _ = try? await core.withHost { host in try host.find(peer: peer, start: false) }
    }

    /// Dials `peer` now from its last-known addresses.
    func connect(_ peer: String, messages: MessageCenter) async {
        await CoreCall.run(messages) {
            try await self.core.withHost { host in try await host.connect(peer: peer) }
        }
    }

    /// Makes `peer` the one-tap send target.
    func setDefault(_ peer: String, messages: MessageCenter) {
        settings.defaultPeer = peer
        messages.info(L10n.Detail.defaultSet)
    }

    /// Unpairs `peer`, refreshes the list and clears it as default. True on success.
    @discardableResult
    func unpair(_ peer: String, messages: MessageCenter) async -> Bool {
        let ok = await CoreCall.succeeded(messages) {
            try await self.core.withHost { host in try await host.unpair(peer: peer) }
        }
        await core.refreshPeers()
        guard ok else { return false }
        ringing.remove(peer)
        if settings.defaultPeer == peer { settings.defaultPeer = nil }
        return true
    }

    /// Sends the current pasteboard to `peer` and says what happened in one line. An empty
    /// pasteboard is reported before the core is asked (checking it shows no paste notice).
    func sendClipboard(to peer: String, label: String, messages: MessageCenter) async {
        let board = UIPasteboard.general
        guard board.hasStrings || board.hasImages else {
            messages.info(L10n.Detail.nothingToSend)
            return
        }
        guard let push = await CoreCall.run(messages, { try await ClipboardSender.pushCurrent(to: peer) }) else {
            return
        }
        messages.show(Self.clipboardMessage(push, label: label, pairedCount: core.peers.count))
    }

    /// The one message for a clipboard push. Port of Android `ClipboardOutcome.of`.
    static func clipboardMessage(_ push: ClipboardPush, label: String, pairedCount: Int) -> UiMessage {
        if !push.sentTo.isEmpty { return .info(L10n.Detail.copiedTo(label)) }
        if push.skippedEcho { return .info(L10n.Detail.clipboardUnchanged) }
        if pairedCount == 0 { return .error(L10n.Detail.noPairedDevices) }
        if push.skippedNoGrant > 0 && push.skippedOffline == 0 { return .error(L10n.Detail.noClipboardGrant) }
        return .error(L10n.Detail.peerOffline(label))
    }
}

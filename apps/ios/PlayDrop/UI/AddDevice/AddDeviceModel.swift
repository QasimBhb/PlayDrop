import Foundation
import Observation
import PlayDropCore
import PlayDropShared

// State and actions of the Add device screen: the core's 120 s discovery window (SR-6), the
// one-time QR code (SR-9), the typed address, the scanner and the pairing outcome. The window
// opens when the screen appears and closes when it disappears. The QR URI and anything the
// scanner reads are secrets: they are kept only in memory and never logged (SR-32). Port of
// Android `ui/adddevice/AddDeviceViewModel.kt`.

/// State of one Add device screen.
@MainActor
@Observable
final class AddDeviceModel {
    /// Which way of pairing the screen shows.
    enum Mode: Hashable, CaseIterable, Identifiable {
        /// Devices found on the network plus a typed address.
        case nearby
        /// This iPhone shows a QR code for the other device to scan.
        case showQr
        /// This iPhone scans the other device's QR code.
        case scanQr

        /// Stable identity for the picker.
        var id: Self { self }

        /// The tab title.
        var title: String {
            switch self {
            case .nearby: return L10n.Add.tabNearby
            case .showQr: return L10n.Add.tabShowQr
            case .scanQr: return L10n.Add.tabScanQr
            }
        }
    }

    /// The window and QR lifetime (SR-6, SR-9).
    static let windowSeconds: TimeInterval = 120
    /// Pause before the scanner accepts another code after a failed scan.
    static let rescanDelay: Duration = .seconds(2)

    /// Current tab; use `select(_:messages:)` to change it.
    private(set) var mode: Mode = .nearby
    /// When the discovery window closes.
    private(set) var windowDeadline: Date?
    /// The one-time QR URI while it is shown. Never logged (SR-9, SR-32).
    private(set) var qrUri: String?
    /// When the QR code stops working.
    private(set) var qrDeadline: Date?
    /// Name or address being paired with; "" for a scanned code; nil when idle.
    private(set) var pairingWith: String?
    /// Set once pairing succeeded; the screen goes back.
    private(set) var finished = false
    /// The typed address.
    var typedAddress = ""

    @ObservationIgnored private let core: CoreHost
    @ObservationIgnored private var scanArmed = true
    @ObservationIgnored private var lastRejectedCode: String?

    /// Creates the model over `core`, by default the shared host.
    // Defaults are nil rather than `.shared`: a default argument is evaluated outside the
    // main actor, where the main-actor `shared` instances cannot be read.
    init(core: CoreHost? = nil) {
        self.core = core ?? .shared
    }

    /// Seconds left in the discovery window at `now`.
    func windowSecondsLeft(at now: Date) -> Int { Self.secondsLeft(windowDeadline, at: now) }

    /// Seconds left before the QR code expires at `now`.
    func qrSecondsLeft(at now: Date) -> Int { Self.secondsLeft(qrDeadline, at: now) }

    /// The URI to draw at `now`, or nil once it expired.
    func liveQrUri(at now: Date) -> String? { qrSecondsLeft(at: now) > 0 ? qrUri : nil }

    /// Opens (or reopens) the 120 s window in which other devices may see this one (SR-6).
    func openWindow(messages: MessageCenter) async {
        let ok = await CoreCall.succeeded(messages) { try await self.core.openAddDeviceWindow(reason: .addDevice) }
        if ok { windowDeadline = Date().addingTimeInterval(Self.windowSeconds) }
    }

    /// Closes everything this screen opened. Called when the screen disappears.
    func close() async {
        if qrUri != nil { hideQr() }
        await core.closeAddDeviceWindow(reason: .showQr)
        await core.closeAddDeviceWindow(reason: .addDevice)
        windowDeadline = nil
    }

    /// Follows pairing events until the screen goes away (the task is cancelled).
    func followPairingEvents(messages: MessageCenter) async {
        for await event in core.events() {
            switch event {
            case let .paired(_, shortId, name):
                pairingWith = nil
                hideQr()
                messages.info(L10n.Add.paired("\(name) (\(shortId))"))
                finished = true
            case let .pairingFailed(reason):
                pairingWith = nil
                messages.show(.error(L10n.Add.pairFailed(reason)))
                try? await Task.sleep(for: Self.rescanDelay)
                scanArmed = true
            default:
                break
            }
        }
    }

    /// Switches tabs. Leaving "Show QR" hides the code; entering it asks for a fresh one.
    func select(_ newMode: Mode, messages: MessageCenter) async {
        guard newMode != mode else { return }
        if mode == .showQr {
            hideQr()
            await core.closeAddDeviceWindow(reason: .showQr)
        }
        mode = newMode
        switch newMode {
        case .showQr: await showQr(messages: messages)
        case .scanQr: scanArmed = true
        case .nearby: break
        }
    }

    /// Asks the core for a fresh one-time QR URI, valid for 120 s.
    func showQr(messages: MessageCenter) async {
        guard let uri = await CoreCall.run(messages, { try await self.core.showQr() }) else { return }
        let deadline = Date().addingTimeInterval(Self.windowSeconds)
        qrUri = uri
        qrDeadline = deadline
        windowDeadline = deadline
    }

    /// Starts SAS pairing with a found device, using its best address.
    func pair(_ candidate: CandidateRow, messages: MessageCenter) async {
        await pair(addr: candidate.bestAddr, shown: candidate.name, messages: messages)
    }

    /// Starts SAS pairing with the typed address; the default port is added when missing.
    func pairTyped(messages: MessageCenter) async {
        guard let addr = Self.normalizedAddress(typedAddress) else {
            messages.show(.error(L10n.Add.badAddress))
            return
        }
        await pair(addr: addr, shown: addr, messages: messages)
    }

    /// A QR code was decoded. Only `playdrop://pair?...` codes are used, once per match
    /// (the camera keeps delivering frames). The code itself is never logged (SR-32).
    func scanned(_ code: String, messages: MessageCenter) async {
        guard scanArmed, pairingWith == nil else { return }
        guard PairingUri.isPairingUri(code) else {
            if lastRejectedCode != code {
                lastRejectedCode = code
                messages.show(.error(L10n.Qr.notPairingCode))
            }
            return
        }
        scanArmed = false
        pairingWith = ""
        let ok = await CoreCall.succeeded(messages) {
            try await self.core.withHost { host in try await host.scanQr(uri: code) }
        }
        if !ok {
            pairingWith = nil
            try? await Task.sleep(for: Self.rescanDelay)
            scanArmed = true
        }
    }

    /// `host:port` for a typed address, bracketing IPv6, or nil when it is not an address.
    static func normalizedAddress(_ text: String) -> String? {
        let t = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard Addresses.isValidHostPort(t), let hp = Addresses.parseHostPort(t) else { return nil }
        return hp.host.contains(":") ? "[\(hp.host)]:\(hp.port)" : "\(hp.host):\(hp.port)"
    }

    private func pair(addr: String, shown: String, messages: MessageCenter) async {
        guard pairingWith == nil else { return }
        pairingWith = shown
        let ok = await CoreCall.succeeded(messages) {
            try await self.core.withHost { host in try await host.pairWith(addr: addr) }
        }
        if !ok { pairingWith = nil }
    }

    private func hideQr() {
        qrUri = nil
        qrDeadline = nil
    }

    private static func secondsLeft(_ deadline: Date?, at now: Date) -> Int {
        guard let deadline else { return 0 }
        return max(0, Int(deadline.timeIntervalSince(now).rounded(.up)))
    }
}

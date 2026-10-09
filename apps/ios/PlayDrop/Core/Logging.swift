import Foundation
import os
import PlayDropCore
import PlayDropShared

// Unified-logging channels for the app's host layer, plus the one helper every file uses
// to log an error. Secrets never reach the log (SR-32): no SAS, QR URI, clipboard text or
// key material is ever interpolated, and errors are reduced to their kind and the core's
// own message, which the core already keeps free of secrets. Foreign errors are logged by
// type, domain and code only, because their `localizedDescription` can carry file paths or
// user content.

/// The app's `os.Logger` channels, one per area, all under the app's bundle identifier.
enum Log {
    /// Core lifecycle: start, stop, configuration.
    static let core = Logger(subsystem: PlayDropIdentifiers.appBundleId, category: "core")
    /// Host event routing and state refreshes.
    static let events = Logger(subsystem: PlayDropIdentifiers.appBundleId, category: "events")
    /// Sends and stream openers.
    static let send = Logger(subsystem: PlayDropIdentifiers.appBundleId, category: "send")
    /// The Keychain-backed secret store.
    static let secrets = Logger(subsystem: PlayDropIdentifiers.appBundleId, category: "secrets")
    /// Interface enumeration.
    static let net = Logger(subsystem: PlayDropIdentifiers.appBundleId, category: "net")
    /// Outbox pulls.
    static let outbox = Logger(subsystem: PlayDropIdentifiers.appBundleId, category: "outbox")
    /// Clipboard apply and push (never the content).
    static let clipboard = Logger(subsystem: PlayDropIdentifiers.appBundleId, category: "clipboard")
    /// Local notifications.
    static let notify = Logger(subsystem: PlayDropIdentifiers.appBundleId, category: "notify")
    /// Battery and presence reports.
    static let signals = Logger(subsystem: PlayDropIdentifiers.appBundleId, category: "signals")
    /// Scene lifecycle wiring.
    static let services = Logger(subsystem: PlayDropIdentifiers.appBundleId, category: "services")

    /// A one-line, secret-free description of `error` for the log. Use it as
    /// `Log.core.error("start failed: \(Log.describe(error), privacy: .public)")`.
    static func describe(_ error: Error) -> String {
        if let e = error as? PlayDropError {
            return "PlayDropError.\(ErrorText.kind(of: e)): \(ErrorText.detail(of: e))"
        }
        if let e = error as? CoreHostError {
            switch e {
            case .notRunning: return "CoreHostError.notRunning"
            case .startFailed(let message): return "CoreHostError.startFailed: \(message)"
            }
        }
        if error is CancellationError {
            return "CancellationError"
        }
        let ns = error as NSError
        return "\(type(of: error)) (\(ns.domain) \(ns.code))"
    }
}

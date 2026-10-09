import Foundation
import Observation
import PlayDropShared
import UIKit

// What the app remembers between launches, in UserDefaults of the App Group suite so the
// share extension can read the default peer and the device name. The iOS counterpart of
// Android's `data/Settings.kt`.
//
// Every property reads and writes UserDefaults directly (no copy in memory to drift from
// what the extension wrote) and reports access and mutation to the Observation registrar by
// hand, so SwiftUI views update when a setting changes. Values that were never set fall back
// to their defaults without being written, so a changed default reaches existing installs.

/// The user's settings; see the file comment.
@MainActor
@Observable
public final class AppSettings {
    /// The shared instance, backed by `UserDefaults(suiteName: PlayDropIdentifiers.appGroup)`.
    public static let shared = AppSettings()

    /// Longest device name in UTF-8 bytes; peers show it next to the Short ID (SR-8).
    public static let maxDeviceNameBytes = 63

    /// Name used when the system name sanitises to nothing.
    static let fallbackDeviceName = "iPhone"

    /// UserDefaults keys, shared with the share extension (`SharedSettingsKeys` in
    /// PlayDropShared). Changing one loses that setting on existing installs.
    private typealias Key = SharedSettingsKeys

    @ObservationIgnored private let defaults: UserDefaults

    /// Settings in `defaults`; the shared instance uses the App Group suite.
    init(defaults: UserDefaults? = nil) {
        if let defaults {
            self.defaults = defaults
        } else if let group = UserDefaults(suiteName: PlayDropIdentifiers.appGroup) {
            self.defaults = group
        } else {
            // Only when the App Group entitlement is missing: the extension then cannot see
            // the settings, but the app still works.
            Log.core.error("App Group defaults unavailable; using standard defaults")
            self.defaults = .standard
        }
    }

    /// Name shown to peers. Defaults to `UIDevice.current.name` (which is the generic model
    /// name on iOS 16 and later without the device-name entitlement). Always sanitised as
    /// one path component (SR-8, SR-17) and capped at 63 UTF-8 bytes; setting an empty name
    /// restores the default.
    public var deviceName: String {
        get {
            access(keyPath: \.deviceName)
            if let stored = defaults.string(forKey: Key.deviceName) {
                let clean = Self.cleanDeviceName(stored)
                if !clean.isEmpty {
                    return clean
                }
            }
            return Self.defaultDeviceName()
        }
        set {
            withMutation(keyPath: \.deviceName) {
                let clean = Self.cleanDeviceName(newValue)
                if clean.isEmpty {
                    defaults.removeObject(forKey: Key.deviceName)
                } else {
                    defaults.set(clean, forKey: Key.deviceName)
                }
            }
        }
    }

    /// TCP and UDP port; `PlayDropIdentifiers.defaultPort` unless changed. Values below 1024
    /// are refused (they need privileges) and read back as the default.
    public var port: UInt16 {
        get {
            access(keyPath: \.port)
            let stored = defaults.integer(forKey: Key.port)
            guard stored >= 1024, stored <= Int(UInt16.max) else {
                return PlayDropIdentifiers.defaultPort
            }
            return UInt16(stored)
        }
        set {
            withMutation(keyPath: \.port) {
                if newValue < 1024 || newValue == PlayDropIdentifiers.defaultPort {
                    defaults.removeObject(forKey: Key.port)
                } else {
                    defaults.set(Int(newValue), forKey: Key.port)
                }
            }
        }
    }

    /// Hex Device ID the share flow sends to without asking, or nil until chosen.
    public var defaultPeer: String? {
        get {
            access(keyPath: \.defaultPeer)
            return SharedSettingsKeys.normalizedPeer(defaults.string(forKey: Key.defaultPeer))
        }
        set {
            withMutation(keyPath: \.defaultPeer) {
                // Unset or blank removes the key; an empty string is never stored.
                if let peer = SharedSettingsKeys.normalizedPeer(newValue) {
                    defaults.set(peer, forKey: Key.defaultPeer)
                } else {
                    defaults.removeObject(forKey: Key.defaultPeer)
                }
            }
        }
    }

    /// The user finished the first-run screens.
    public var onboardingDone: Bool {
        get {
            access(keyPath: \.onboardingDone)
            return bool(Key.onboardingDone, default: false)
        }
        set {
            withMutation(keyPath: \.onboardingDone) { defaults.set(newValue, forKey: Key.onboardingDone) }
        }
    }

    /// Incoming clipboard text and images are put on `UIPasteboard` while the app is in the
    /// foreground. Default true.
    public var applyIncomingClipboard: Bool {
        get {
            access(keyPath: \.applyIncomingClipboard)
            return bool(Key.applyIncomingClipboard, default: true)
        }
        set {
            withMutation(keyPath: \.applyIncomingClipboard) { defaults.set(newValue, forKey: Key.applyIncomingClipboard) }
        }
    }

    /// Items peers hold for this iPhone are pulled automatically when the app opens and when
    /// a peer comes online or announces items. Default true.
    public var pullOutboxOnOpen: Bool {
        get {
            access(keyPath: \.pullOutboxOnOpen)
            return bool(Key.pullOutboxOnOpen, default: true)
        }
        set {
            withMutation(keyPath: \.pullOutboxOnOpen) { defaults.set(newValue, forKey: Key.pullOutboxOnOpen) }
        }
    }

    /// Core logging at debug level from the next app launch (the core sets its log level once
    /// per process). Debug builds always log at debug level. Default false.
    public var logLevelDebug: Bool {
        get {
            access(keyPath: \.logLevelDebug)
            return bool(Key.logLevelDebug, default: false)
        }
        set {
            withMutation(keyPath: \.logLevelDebug) { defaults.set(newValue, forKey: Key.logLevelDebug) }
        }
    }

    /// A stored Bool, or `fallback` when the key was never written.
    private func bool(_ key: String, default fallback: Bool) -> Bool {
        defaults.object(forKey: key) == nil ? fallback : defaults.bool(forKey: key)
    }

    /// The system device name, cleaned like a user-entered one.
    private static func defaultDeviceName() -> String {
        let clean = cleanDeviceName(UIDevice.current.name)
        return clean.isEmpty ? fallbackDeviceName : clean
    }

    /// `name` trimmed, sanitised as one path component and cut to `maxDeviceNameBytes`
    /// UTF-8 bytes on a character boundary. Empty when nothing usable is left.
    static func cleanDeviceName(_ name: String) -> String {
        let trimmed = name.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else {
            return ""
        }
        var clean = FileNames.sanitized(trimmed)
        while clean.utf8.count > maxDeviceNameBytes {
            clean.removeLast()
        }
        return clean.trimmingCharacters(in: .whitespacesAndNewlines)
    }
}

import SwiftUI

// Maps the device type token a peer announces (`desktop`, `laptop`, `phone`, `tablet`, `tv`;
// protocol.md) to an SF Symbol and a spoken label. Port of Android `CandidateKind`. Unknown
// tokens fall back to a generic device, never to an error.

/// The kind of device behind a type token.
enum DeviceKind: Equatable, Sendable {
    /// A desktop computer.
    case computer
    /// A laptop.
    case laptop
    /// A phone.
    case phone
    /// A tablet.
    case tablet
    /// A TV.
    case tv
    /// Anything else.
    case other

    /// Maps the wire token, case-insensitively.
    init(token: String) {
        switch token.lowercased() {
        case "desktop": self = .computer
        case "laptop": self = .laptop
        case "phone", "mobile": self = .phone
        case "tablet": self = .tablet
        case "tv": self = .tv
        default: self = .other
        }
    }

    /// The SF Symbol name.
    var systemImage: String {
        switch self {
        case .computer: return "desktopcomputer"
        case .laptop: return "laptopcomputer"
        case .phone: return "iphone"
        case .tablet: return "ipad"
        case .tv: return "tv"
        case .other: return "display"
        }
    }

    /// "Computer", "Laptop" and so on.
    var label: String {
        switch self {
        case .computer: return String(localized: "ui_kind_computer", defaultValue: "Computer")
        case .laptop: return String(localized: "ui_kind_laptop", defaultValue: "Laptop")
        case .phone: return String(localized: "ui_kind_phone", defaultValue: "Phone")
        case .tablet: return String(localized: "ui_kind_tablet", defaultValue: "Tablet")
        case .tv: return String(localized: "ui_kind_tv", defaultValue: "TV")
        case .other: return String(localized: "ui_kind_other", defaultValue: "Device")
        }
    }
}

/// The icon for a device type token, labelled for VoiceOver.
struct DeviceTypeIcon: View {
    /// The announced token.
    let token: String

    var body: some View {
        let kind = DeviceKind(token: token)
        Image(systemName: kind.systemImage)
            .symbolRenderingMode(.hierarchical)
            .foregroundStyle(.tint)
            .accessibilityLabel(Text(kind.label))
    }
}

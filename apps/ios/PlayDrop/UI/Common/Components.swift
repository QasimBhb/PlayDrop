import SwiftUI

// Small views shared by several screens: the online dot, the battery label and the grey
// hint line. Port of Android `ui/common/Components.kt`.

/// A small round status dot: green while the device is reachable, grey otherwise.
struct StatusDot: View {
    /// Draw it green.
    let online: Bool

    var body: some View {
        Circle()
            .fill(online ? Color.green : Color.secondary.opacity(0.5))
            .frame(width: 10, height: 10)
            .accessibilityHidden(true)
    }
}

/// Battery glyph plus percent, as a peer last reported it.
struct BatteryLabel: View {
    /// Percent 0...100.
    let level: Int
    /// Charging at the last report.
    let charging: Bool

    var body: some View {
        HStack(spacing: 4) {
            Image(systemName: Self.symbol(level: level, charging: charging))
                .foregroundStyle(level <= 15 && !charging ? Color.red : Color.secondary)
            Text(L10n.percent(level))
                .font(.caption)
                .foregroundStyle(.secondary)
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(Text(L10n.Common.battery(level, charging: charging)))
    }

    /// The SF Symbol for a level.
    static func symbol(level: Int, charging: Bool) -> String {
        if charging { return "battery.100.bolt" }
        switch level {
        case ...15: return "battery.0"
        case ...35: return "battery.25"
        case ...60: return "battery.50"
        case ...85: return "battery.75"
        default: return "battery.100"
        }
    }
}

/// A grey explanatory line under a control.
struct HintText: View {
    /// The text.
    let text: String

    /// Creates a hint.
    init(_ text: String) { self.text = text }

    var body: some View {
        Text(text)
            .font(.footnote)
            .foregroundStyle(.secondary)
            .fixedSize(horizontal: false, vertical: true)
    }
}

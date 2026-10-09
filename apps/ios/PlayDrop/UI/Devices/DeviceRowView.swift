import SwiftUI

// One row of the Devices list: online dot, label with Short ID (SR-8), status line (online,
// asleep, removed, or "last seen"), battery, the default star and a ringing hint. The status
// line re-renders every 30 s so "last seen" stays true. Port of Android `DeviceCard`.

/// A paired device in the list.
struct DeviceRowView: View {
    /// The row data.
    let row: DeviceRow
    /// A find request is ringing this device.
    let ringing: Bool

    var body: some View {
        HStack(alignment: .center, spacing: 12) {
            StatusDot(online: row.online && !row.asleep)
            VStack(alignment: .leading, spacing: 4) {
                PeerLabelText(label: row.label, shortId: row.shortId, font: .headline)
                HStack(spacing: 8) {
                    TimelineView(.periodic(from: .now, by: 30)) { context in
                        Text(DeviceRows.statusText(row, now: UIFormat.nowSecs(context.date)))
                            .font(.subheadline)
                            .foregroundStyle(row.removedByPeer ? Color.red : Color.secondary)
                    }
                    if let level = row.batteryLevel, row.online {
                        BatteryLabel(level: level, charging: row.charging)
                    }
                    if ringing {
                        Label(L10n.Detail.ringing, systemImage: "speaker.wave.3.fill")
                            .font(.caption)
                            .foregroundStyle(.orange)
                    }
                }
            }
            Spacer(minLength: 0)
            if row.isDefault {
                Image(systemName: "star.fill")
                    .foregroundStyle(.tint)
                    .accessibilityLabel(Text(L10n.Common.defaultDevice))
            }
        }
        .padding(.vertical, 4)
        .accessibilityElement(children: .combine)
    }
}

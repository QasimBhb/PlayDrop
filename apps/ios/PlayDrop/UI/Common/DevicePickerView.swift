import PlayDropCore
import SwiftUI

// "Send to which device?": the list a send shows when no default device is set. Used by the
// Devices tab's Send files button and by queued share batches that have no target. Each row
// shows the label with its Short ID (SR-8); "Always send here with one tap" makes the choice
// the default device, so the next send needs no question (the one-tap goal).

/// Lets the user pick one paired device.
struct DevicePickerView: View {
    /// Navigation title.
    let title: String
    /// Paired devices to choose from.
    let peers: [PeerSummary]
    /// Offer "Always send here with one tap".
    var offerRemember = true
    /// Called with the hex Device ID and whether to make it the default. The caller closes
    /// the sheet (usually by presenting the next one), so the picker never dismisses itself.
    let onPick: (_ peer: String, _ remember: Bool) -> Void

    @Environment(\.dismiss) private var dismiss
    @State private var remember = true

    var body: some View {
        NavigationStack {
            List {
                Section {
                    ForEach(peers, id: \.deviceId) { peer in
                        Button {
                            onPick(peer.deviceId, offerRemember && remember)
                        } label: {
                            HStack(spacing: 12) {
                                StatusDot(online: peer.online)
                                PeerLabelText(label: peer.label, shortId: peer.shortId)
                                Spacer(minLength: 0)
                                if !peer.online {
                                    Text(L10n.Common.offline)
                                        .font(.caption)
                                        .foregroundStyle(.secondary)
                                }
                            }
                        }
                        .foregroundStyle(.primary)
                    }
                }
                if offerRemember {
                    Section {
                        Toggle(L10n.Picker.remember, isOn: $remember)
                    } footer: {
                        Text(L10n.Picker.rememberHint)
                    }
                }
            }
            .navigationTitle(title)
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button(L10n.cancel) { dismiss() }
                }
            }
        }
    }
}

extension L10n {
    /// Strings of the device picker.
    enum Picker {
        /// "Send to".
        static var sendTo: String { String(localized: "ui_send_to_title", defaultValue: "Send to") }
        /// "Send to which device?".
        static var whichDevice: String {
            String(localized: "share_pick_device", defaultValue: "Send to which device?")
        }
        /// "Always send here with one tap".
        static var remember: String {
            String(localized: "ui_send_to_remember", defaultValue: "Always send here with one tap")
        }
        /// Footer under the toggle.
        static var rememberHint: String {
            String(localized: "share_pick_hint", defaultValue: "Your choice becomes the default for one-tap sends.")
        }
    }
}

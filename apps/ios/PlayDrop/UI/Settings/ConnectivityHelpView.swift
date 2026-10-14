import PlayDropShared
import SwiftUI

// "Can't find your computer?": the guest Wi-Fi path. Four short steps in the order to try
// them (address or QR, Personal Hotspot, USB with Personal Hotspot, VPN) and a link to the
// Local Network permission screen. Reached from the empty Devices list, Add device and
// Diagnostics, which also suggests it when a failed layer points here
// (`ConnectivityHelp.isSuggested`). iOS has no public link to the Personal Hotspot page, so
// the steps are text. Anti-pattern 18: a blocked network is explained, never silent.

/// The connection help screen.
struct ConnectivityHelpView: View {
    /// True when diagnostics pointed here, so the screen opens with the reason.
    var suggested = false

    /// This build carries the multicast entitlement (a free-team build does not).
    private let multicast = ConnectivityHelp.multicastAvailable(
        profile: EmbeddedProvisioningProfile.load(from: .main)
    )

    var body: some View {
        List {
            Section {
                Text(suggested ? L10n.Connect.suggested : L10n.Connect.intro)
            }
            Section(L10n.Connect.sameNetworkTitle) {
                Text(L10n.Connect.sameNetworkBody)
                if !multicast {
                    Text(L10n.Connect.noMulticast).foregroundStyle(.secondary)
                }
            }
            Section(L10n.Connect.hotspotTitle) {
                Text(L10n.Connect.hotspotBody)
            }
            Section(L10n.Connect.usbTitle) {
                Text(L10n.Connect.usbBody)
            }
            Section(L10n.Connect.vpnTitle) {
                Text(L10n.Connect.vpnBody)
            }
            Section {
                NavigationLink {
                    LocalNetworkFixItView(reason: nil)
                } label: {
                    Label(L10n.Connect.permission, systemImage: "wifi.exclamationmark")
                }
            }
        }
        .navigationTitle(L10n.Connect.title)
        .navigationBarTitleDisplayMode(.inline)
    }
}

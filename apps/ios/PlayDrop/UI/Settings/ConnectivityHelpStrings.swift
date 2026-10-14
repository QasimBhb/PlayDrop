import Foundation

// Strings of the "Can't find your computer?" help, keyed like Android `strings_ui.xml`.
// See `L10n.swift` for the scheme.

extension L10n {
    /// The connection help (guest Wi-Fi path).
    enum Connect {
        /// Screen title and entry label.
        static var title: String {
            String(localized: "ui_connect_title", defaultValue: "Can't find your computer?")
        }
        /// Hint under the entry in Diagnostics.
        static var entryHint: String {
            String(
                localized: "ui_connect_entry_hint",
                defaultValue: "Other ways to connect: address, QR code, hotspot or USB"
            )
        }
        /// Intro line.
        static var intro: String {
            String(
                localized: "ui_connect_intro",
                defaultValue: "If your computer does not show up, try these, in this order."
            )
        }
        /// Shown when diagnostics point to this help.
        static var suggested: String {
            String(
                localized: "ui_connect_suggested",
                defaultValue: "Your devices may not be able to see each other on this network. These steps can help."
            )
        }

        /// Section (a) title.
        static var sameNetworkTitle: String {
            String(localized: "ui_connect_same_title", defaultValue: "Same network, but nothing shows up")
        }
        /// Section (a) body.
        static var sameNetworkBody: String {
            String(
                localized: "ui_connect_same_body",
                defaultValue: "Mesh Wi-Fi and VPNs often block the way devices find each other. Pair with the computer's address instead: on the computer open Add device and note its address, then type it in Add device here. Or show the QR code on one device and scan it with the other."
            )
        }
        /// Extra line when this build has no multicast entitlement.
        static var noMulticast: String {
            String(
                localized: "ui_connect_no_multicast",
                defaultValue: "This copy of PlayDrop cannot look for devices by itself, so the address or the QR code is how you pair. Paired devices reconnect on their own."
            )
        }
        /// Section (b) title.
        static var hotspotTitle: String {
            String(localized: "ui_connect_hotspot_title", defaultValue: "Guest, hotel or campus Wi-Fi")
        }
        /// Section (b) body.
        static var hotspotBody: String {
            String(
                localized: "ui_connect_hotspot_body",
                defaultValue: "Some networks stop devices from reaching each other. Turn on Personal Hotspot on this iPhone (Settings, then Personal Hotspot), connect the computer to it, then pair by address or QR code."
            )
        }

        /// Section (c) title.
        static var usbTitle: String {
            String(localized: "ui_connect_usb_title", defaultValue: "By USB cable")
        }
        /// Section (c) body.
        static var usbBody: String {
            String(
                localized: "ui_connect_usb_body",
                defaultValue: "With Personal Hotspot on, plug this iPhone into the computer and tap Trust. The computer gets a wired network to the iPhone. This is the fastest fallback. Then pair by address or QR code."
            )
        }

        /// Section (d) title.
        static var vpnTitle: String {
            String(localized: "ui_connect_vpn_title", defaultValue: "A VPN is on")
        }
        /// Section (d) body.
        static var vpnBody: String {
            String(
                localized: "ui_connect_vpn_body",
                defaultValue: "Allow local network access in the VPN app, or pause the VPN on this iPhone and on the computer while you pair."
            )
        }

        /// Link to the permission screen.
        static var permission: String {
            String(
                localized: "ui_connect_permission",
                defaultValue: "Still nothing? Check that Local Network access is on for PlayDrop."
            )
        }
    }

    /// Phone-side refusal text for a command the computer did not allow.
    static func mediaRefused(_ label: String) -> String {
        String(
            localized: "ui_media_refused",
            defaultValue: "\(label) has not allowed this iPhone to control its media. Turn on Media for this iPhone in PlayDrop on \(label)."
        )
    }
}

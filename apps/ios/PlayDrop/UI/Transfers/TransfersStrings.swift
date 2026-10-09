import Foundation

// Strings of the Transfers tab, the offer cards and the queued share batches, keyed like
// Android `strings_ui.xml` and `strings.xml`. See `L10n.swift` for the scheme.

extension L10n {
    /// Transfers tab.
    enum Transfers {
        /// "Transfers".
        static var title: String { String(localized: "ui_transfers_title", defaultValue: "Transfers") }
        /// Empty state.
        static var empty: String {
            String(
                localized: "ui_transfers_empty",
                defaultValue: "Nothing sent or received yet. Files you send or receive show up here."
            )
        }
        /// "Incoming".
        static var incoming: String { String(localized: "ui_transfers_incoming", defaultValue: "Incoming") }
        /// "Outgoing".
        static var outgoing: String { String(localized: "ui_transfers_outgoing", defaultValue: "Outgoing") }
        /// "From Desk".
        static func from(_ label: String) -> String {
            String(localized: "ui_transfers_from", defaultValue: "From \(label)")
        }
        /// "To Desk".
        static func to(_ label: String) -> String {
            String(localized: "ui_transfers_to", defaultValue: "To \(label)")
        }
        /// "12.0 MiB of 40.0 MiB".
        static func of(_ done: String, _ total: String) -> String {
            String(localized: "ui_transfers_of", defaultValue: "\(done) of \(total)")
        }
        /// "13 s left".
        static func left(_ time: String) -> String {
            String(localized: "ui_transfers_left", defaultValue: "\(time) left")
        }
        /// "and 4 more".
        static func more(_ n: Int) -> String {
            String(localized: "ui_transfers_more", defaultValue: "and \(n) more")
        }
        /// "Accept".
        static var accept: String { String(localized: "ui_transfers_accept", defaultValue: "Accept") }
        /// "Decline".
        static var decline: String { String(localized: "ui_transfers_decline", defaultValue: "Decline") }
        /// "Open".
        static var open: String { String(localized: "ui_transfers_open", defaultValue: "Open") }
        /// Where received files are.
        static var savedIn: String {
            String(
                localized: "ui_transfers_saved_in",
                defaultValue: "Saved in Files > On My iPhone > PlayDrop"
            )
        }
        /// "1 file", "3 files".
        static func files(_ n: Int) -> String {
            L10n.plural(
                n,
                one: String(localized: "ui_files_one", defaultValue: "\(n) file"),
                other: String(localized: "ui_files_other", defaultValue: "\(n) files")
            )
        }
        /// "Waiting for you".
        static var waitingYou: String { String(localized: "ui_state_waiting_you", defaultValue: "Waiting for you") }
        /// "Waiting for the other device".
        static var waitingPeer: String {
            String(localized: "ui_state_waiting_peer", defaultValue: "Waiting for the other device")
        }
        /// "Done".
        static var stateDone: String { String(localized: "ui_state_done", defaultValue: "Done") }
        /// "Failed".
        static var stateFailed: String { String(localized: "ui_state_failed", defaultValue: "Failed") }
        /// "Cancelled".
        static var stateCancelled: String { String(localized: "ui_state_cancelled", defaultValue: "Cancelled") }
        /// "Desk wants to send files".
        static func offerTitle(_ label: String) -> String {
            String(localized: "offer_title", defaultValue: "\(label) wants to send files")
        }
        /// "Incoming files" sheet title.
        static var offersSheetTitle: String {
            String(localized: "ui_offers_sheet_title", defaultValue: "Incoming files")
        }
        /// Offer answer failed.
        static func offerAnswerFailed(_ reason: String) -> String {
            String(localized: "offer_answer_failed", defaultValue: "Could not answer the offer: \(reason)")
        }
    }

    /// Batches queued by the share extension.
    enum Outgoing {
        /// Section title.
        static var title: String {
            String(localized: "ui_outgoing_title", defaultValue: "Waiting to send")
        }
        /// Explains the section.
        static var hint: String {
            String(
                localized: "ui_outgoing_hint",
                defaultValue: "Shared from other apps. They go out when the device is online, or pick one now."
            )
        }
        /// Sheet title when batches have no device.
        static var needsDeviceTitle: String {
            String(localized: "ui_outgoing_needs_device", defaultValue: "Send shared items")
        }
        /// "1 item, 2.0 MiB", "3 items, 2.0 MiB".
        static func summary(_ count: Int, _ size: String) -> String {
            L10n.plural(
                count,
                one: String(localized: "ui_outgoing_summary_one", defaultValue: "\(count) item, \(size)"),
                other: String(localized: "ui_outgoing_summary_other", defaultValue: "\(count) items, \(size)")
            )
        }
        /// "Send to Desk".
        static func sendTo(_ label: String) -> String {
            String(localized: "action_send_to", defaultValue: "Send to \(label)")
        }
        /// "Choose device".
        static var chooseDevice: String {
            String(localized: "ui_outgoing_choose", defaultValue: "Choose device")
        }
        /// "Discard".
        static var discard: String { String(localized: "ui_outgoing_discard", defaultValue: "Discard") }
        /// Discard confirmation.
        static var discardTitle: String {
            String(localized: "ui_outgoing_discard_title", defaultValue: "Discard these items?")
        }
        /// Discard explanation.
        static var discardBody: String {
            String(
                localized: "ui_outgoing_discard_body",
                defaultValue: "They are removed from PlayDrop. The originals in other apps stay as they are."
            )
        }
        /// "Sending".
        static var sending: String { String(localized: "ui_outgoing_sending", defaultValue: "Sending") }
        /// "Shared 5 min ago".
        static func sharedAgo(_ ago: String) -> String {
            String(localized: "ui_outgoing_shared_ago", defaultValue: "Shared \(ago)")
        }
        /// "Sending 3 items to Desk".
        static func sendingItems(_ count: Int, _ label: String) -> String {
            L10n.plural(
                count,
                one: String(localized: "toast_sending_items_one", defaultValue: "Sending \(count) item to \(label)"),
                other: String(localized: "toast_sending_items_other", defaultValue: "Sending \(count) items to \(label)")
            )
        }
    }
}

import PlayDropCore
import PlayDropShared

// Which sheet the root shows over the tabs. SwiftUI presents one sheet at a time, so the
// three things that must reach the user wherever they are share one `sheet(item:)`, in
// priority order: a pairing prompt (it times out after 30 s), then incoming offers (shown
// inline instead while the Transfers tab is open), then shared items that have no device to
// go to. Dismissing offers or items snoozes exactly the ones on screen; new ones show again.
// Plain logic for unit tests.

/// A sheet the root presents.
enum RootSheet: Identifiable, Hashable {
    /// `CoreHost.pairingPrompt` is waiting.
    case pairing
    /// Offers wait for Accept or Decline.
    case offers
    /// Shared items need a device.
    case batches

    var id: Self { self }

    /// The sheet to show now, or nil.
    static func current(
        pairingWaiting: Bool,
        offerIds: [String],
        onTransfersTab: Bool,
        batchIdsNeedingDevice: [String],
        snoozedOffers: Set<String>,
        snoozedBatches: Set<String>
    ) -> RootSheet? {
        if pairingWaiting { return .pairing }
        if !onTransfersTab, offerIds.contains(where: { !snoozedOffers.contains($0) }) { return .offers }
        if batchIdsNeedingDevice.contains(where: { !snoozedBatches.contains($0) }) { return .batches }
        return nil
    }

    /// Batches that are not being sent and have no paired device to go to. Empty while no
    /// device is paired at all (there is nothing to pick; the Transfers tab still lists them).
    static func batchesNeedingDevice(
        _ batches: [OutgoingBatch],
        sending: [String: String],
        hasPeers: Bool,
        intendedPeer: (OutgoingBatch) -> String?
    ) -> [OutgoingBatch] {
        guard hasPeers else { return [] }
        return batches.filter { sending[$0.id] == nil && intendedPeer($0) == nil }
    }
}

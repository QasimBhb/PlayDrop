// Which sheet the root shows, and which shared batches still need a device.
import PlayDropShared
import XCTest
@testable import PlayDrop

final class RootSheetTests: XCTestCase {
    private func current(
        pairing: Bool = false, offers: [String] = [], onTransfers: Bool = false, batches: [String] = [],
        snoozedOffers: Set<String> = [], snoozedBatches: Set<String> = []
    ) -> RootSheet? {
        RootSheet.current(
            pairingWaiting: pairing, offerIds: offers, onTransfersTab: onTransfers,
            batchIdsNeedingDevice: batches, snoozedOffers: snoozedOffers, snoozedBatches: snoozedBatches
        )
    }

    func testPriority() {
        XCTAssertNil(current())
        XCTAssertEqual(current(pairing: true, offers: ["1"], batches: ["b"]), .pairing)
        XCTAssertEqual(current(offers: ["1"], batches: ["b"]), .offers)
        XCTAssertEqual(current(offers: ["1"], onTransfers: true, batches: ["b"]), .batches, "offers show inline on Transfers")
        XCTAssertEqual(current(onTransfers: true, batches: ["b"]), .batches)
    }

    func testSnoozeCoversOnlyWhatWasShown() {
        XCTAssertNil(current(offers: ["1"], batches: ["b"], snoozedOffers: ["1"], snoozedBatches: ["b"]))
        XCTAssertEqual(current(offers: ["1", "2"], snoozedOffers: ["1"]), .offers, "a new offer shows again")
    }

    func testBatchesNeedingDevice() {
        let batches = [Fixtures.batch("a"), Fixtures.batch("b"), Fixtures.batch("c")]
        let need = RootSheet.batchesNeedingDevice(
            batches, sending: ["b": "t1"], hasPeers: true, intendedPeer: { $0.id == "c" ? "peer" : nil }
        )
        XCTAssertEqual(need.map(\.id), ["a"])
        XCTAssertEqual(
            RootSheet.batchesNeedingDevice(batches, sending: [:], hasPeers: false, intendedPeer: { _ in nil }), [],
            "nothing to pick while no device is paired"
        )
    }
}

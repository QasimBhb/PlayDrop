// Transfer rows: offers left out, newest first, time left, cancel rules and delivered files.
import PlayDropCore
import XCTest
@testable import PlayDrop

final class TransferRowsTests: XCTestCase {
    func testOrderingAndOfferFilter() {
        let snapshots = [
            Fixtures.transfer("old", startedAt: 10),
            Fixtures.transfer("offer", state: .offered, startedAt: 30),
            Fixtures.transfer("new", startedAt: 20),
            Fixtures.transfer("new2", startedAt: 20),
        ]
        let rows = TransferRows.derive(snapshots: snapshots, pendingOfferIds: ["offer"], label: { "L-\($0)" })
        XCTAssertEqual(rows.map(\.id), ["new2", "new", "old"], "newest first, later entry first on a tie")
        XCTAssertEqual(rows[0].peerLabel, "L-p")
        XCTAssertEqual(rows[0].shortId, "S-p")
    }

    func testActiveRowFigures() {
        let row = TransferRows.derive(
            snapshots: [Fixtures.transfer("t", direction: .send, bytes: 40, total: 100, bytesPerSec: 30)],
            pendingOfferIds: [], label: { $0 }
        )[0]
        XCTAssertFalse(row.receive)
        XCTAssertEqual(row.etaSeconds, 2, "60 bytes at 30 B/s")
        XCTAssertEqual(row.fraction, 0.4, accuracy: 0.0001)
        XCTAssertTrue(row.canCancel)
        XCTAssertEqual(row.deliveredPaths, [])
    }

    func testFinishedAndOfferedRows() {
        let delivered = [DeliveredFile(path: "a.txt", location: "/var/mobile/a.txt")]
        let rows = TransferRows.derive(
            snapshots: [
                Fixtures.transfer("done", state: .complete, bytes: 100, startedAt: 3, delivered: delivered),
                Fixtures.transfer("failed", direction: .send, state: .failed, startedAt: 2, note: "refused"),
                Fixtures.transfer("waiting", state: .offered, startedAt: 1),
            ],
            pendingOfferIds: [], label: { $0 }
        )
        XCTAssertEqual(rows[0].deliveredPaths, ["/var/mobile/a.txt"])
        XCTAssertFalse(rows[0].canCancel)
        XCTAssertNil(rows[0].etaSeconds)
        XCTAssertEqual(rows[1].note, "refused")
        XCTAssertFalse(rows[1].canCancel)
        XCTAssertFalse(rows[2].canCancel, "an incoming offer is answered, not cancelled")
    }

    func testForgetIsOfferedOnlyForAFailedSendKeptForResume() {
        func row(_ direction: TransferDirection, _ state: TransferStatus, resumable: Bool) -> TransferRow {
            TransferRows.derive(
                snapshots: [Fixtures.transfer("x", direction: direction, state: state, resumable: resumable)],
                pendingOfferIds: [], label: { $0 }
            )[0]
        }
        let kept = row(.send, .failed, resumable: true)
        XCTAssertTrue(kept.canForget)
        XCTAssertFalse(kept.canCancel)
        XCTAssertFalse(row(.send, .failed, resumable: false).canForget)
        XCTAssertFalse(row(.send, .cancelled, resumable: false).canForget)
        XCTAssertFalse(row(.send, .active, resumable: false).canForget)
        XCTAssertFalse(row(.receive, .failed, resumable: true).canForget)
    }

    func testFileURLs() {
        let urls = TransferRows.fileURLs(["/tmp/a.txt", "file:///tmp/b.txt", "content://x/y", "relative"])
        XCTAssertEqual(urls.map(\.path), ["/tmp/a.txt", "/tmp/b.txt"])
    }
}

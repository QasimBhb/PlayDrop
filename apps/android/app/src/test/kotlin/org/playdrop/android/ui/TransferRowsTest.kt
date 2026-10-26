package org.playdrop.android.ui

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.playdrop.android.ui.common.Samples
import org.playdrop.android.ui.transfers.OfferInfo
import org.playdrop.android.ui.transfers.TransferRows
import org.playdrop.core.DeliveredFile
import org.playdrop.core.TransferDirection
import org.playdrop.core.TransferStatus

/** Transfer row derivation: order, buttons, offer names and delivered files. */
class TransferRowsTest {
    private val peers = listOf(Samples.laptop, Samples.desktop)

    @Test
    fun offersComeFirstThenNewestFirst() {
        val rows = TransferRows.derive(Samples.transfers, peers, emptyMap())
        assertEquals(listOf("t4", "t5", "t3", "t2", "t1"), rows.map { it.id })
    }

    @Test
    fun buttonsFollowTheState() {
        val rows = TransferRows.derive(Samples.transfers, peers, emptyMap()).associateBy { it.id }
        val offer = rows.getValue("t4")
        assertTrue(offer.canRespond)
        assertFalse(offer.canCancel)
        val active = rows.getValue("t3")
        assertTrue(active.canCancel)
        assertFalse(active.canRespond)
        assertEquals(4L, active.etaSecs)
        assertFalse(rows.getValue("t1").canCancel)
        assertFalse(rows.getValue("t5").canCancel)
        assertEquals("connection lost", rows.getValue("t5").note)
    }

    @Test
    fun forgetIsOfferedOnlyForAFailedSendKeptForResume() {
        fun row(direction: TransferDirection, state: TransferStatus, resumable: Boolean) = TransferRows.derive(
            listOf(Samples.snapshot("x", direction, state, 10, 20, 0, 1, resumable = resumable)),
            peers,
            emptyMap(),
        ).single()
        val kept = row(TransferDirection.SEND, TransferStatus.FAILED, resumable = true)
        assertTrue(kept.canForget)
        assertFalse(kept.canCancel)
        assertFalse(kept.canRespond)
        assertFalse(row(TransferDirection.SEND, TransferStatus.FAILED, resumable = false).canForget)
        assertFalse(row(TransferDirection.SEND, TransferStatus.CANCELLED, resumable = false).canForget)
        assertFalse(row(TransferDirection.SEND, TransferStatus.ACTIVE, resumable = false).canForget)
        assertFalse(row(TransferDirection.RECEIVE, TransferStatus.FAILED, resumable = true).canForget)
    }

    @Test
    fun labelsComeFromPeersOrFallBackToShortId() {
        val stranger = Samples.snapshot("x", TransferDirection.SEND, TransferStatus.ACTIVE, 1, 2, 0, 1, peer = "ff".repeat(32))
        val rows = TransferRows.derive(listOf(Samples.transfers[0], stranger), peers, emptyMap()).associateBy { it.id }
        assertEquals("Pop laptop", rows.getValue("t1").peerLabel)
        assertEquals(Samples.laptop.shortId, rows.getValue("x").peerLabel)
    }

    @Test
    fun offerNamesAreCappedWithACountOfTheRest() {
        val info = OfferInfo(listOf("a", "b", "c"), 5, 100, "hi")
        val row = TransferRows.derive(Samples.transfers, peers, mapOf("t4" to info)).first { it.id == "t4" }
        assertEquals(listOf("a", "b", "c"), row.offeredNames)
        assertEquals(2L, row.moreNames)
        assertEquals("hi", row.note)
    }

    @Test
    fun deliveredFilesDriveOpenAndFolder() {
        val one = TransferRows.derive(Samples.transfers, peers, emptyMap()).first { it.id == "t2" }
        assertEquals("content://media/external/downloads/42", one.openUri)
        assertEquals("IMG_0042.jpg", one.openName)
        assertFalse(one.showFolder)

        val many = Samples.snapshot(
            "m",
            TransferDirection.RECEIVE,
            TransferStatus.COMPLETE,
            10,
            10,
            0,
            2,
            delivered = listOf(DeliveredFile("a", "content://a"), DeliveredFile("b", "content://b")),
        )
        val row = TransferRows.derive(listOf(many), peers, emptyMap()).single()
        assertEquals("content://a", row.openUri)
        assertTrue(row.showFolder)

        val sent = TransferRows.derive(Samples.transfers, peers, emptyMap()).first { it.id == "t1" }
        assertNull(sent.openUri)
    }

    @Test
    fun pendingOffersCountsIncomingOffersOnly() {
        assertEquals(1, TransferRows.pendingOffers(Samples.transfers))
    }

    @Test
    fun aRelocatedFileOpensWhereItReallyIs() {
        val relocate = { id: String, loc: String ->
            if (id == "t2" && loc == "content://media/external/downloads/42") "content://media/external/downloads/43" else loc
        }
        val rows = TransferRows.derive(Samples.transfers, peers, emptyMap(), relocate).associateBy { it.id }
        assertEquals("content://media/external/downloads/43", rows.getValue("t2").openUri)
        // Unknown keys keep the stored location.
        val plain = TransferRows.derive(Samples.transfers, peers, emptyMap()).associateBy { it.id }
        assertEquals("content://media/external/downloads/42", plain.getValue("t2").openUri)
    }

    @Test
    fun filesDoneShowsOnlyOnActiveMultiFileRows() {
        fun row(state: TransferStatus, files: Int, done: Int) = TransferRows.derive(
            listOf(
                Samples.snapshot("f", TransferDirection.SEND, state, 10, 100, 5, files.toLong())
                    .copy(filesDone = done.toULong()),
            ),
            peers,
            emptyMap(),
        ).single()
        val mid = row(TransferStatus.ACTIVE, 5000, 1204)
        assertTrue(mid.showFilesDone)
        assertEquals(1204L, mid.filesDone)
        // The core reports no count: the row keeps showing only the file count.
        assertFalse(row(TransferStatus.ACTIVE, 5000, 0).showFilesDone)
        // One file, or a finished row, has no "N of M".
        assertFalse(row(TransferStatus.ACTIVE, 1, 1).showFilesDone)
        assertFalse(row(TransferStatus.COMPLETE, 5000, 5000).showFilesDone)
    }
}

package org.playdrop.android.ui.device

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.playdrop.core.TransferFailure

/** The device page's send card follows the real outcome of its transfer. */
class SendNoticesTest {
    private fun notice(id: String) = SendNotice(1, "calliope", offline = false, transferId = id)

    @Test
    fun aRunningSendSaysSendingThenFollowsItsEnd() {
        var b = SendNoticeBook().begin("p", notice("t1"))
        assertEquals(SendPhase.Sending, b.notices["p"]!!.phase)
        b = b.end("t1", SendPhase.Failed(TransferFailure.NO_SPACE, null))
        assertEquals(SendPhase.Failed(TransferFailure.NO_SPACE, null), b.notices["p"]!!.phase)
        // A failed send that resumes and completes turns to sent.
        b = b.end("t1", SendPhase.Sent)
        assertEquals(SendPhase.Sent, b.notices["p"]!!.phase)
    }

    @Test
    fun anEndForAnotherTransferChangesNothing() {
        val b = SendNoticeBook().begin("p", notice("t1")).end("t2", SendPhase.Sent)
        assertEquals(SendPhase.Sending, b.notices["p"]!!.phase)
    }

    @Test
    fun anOutcomeThatArrivedBeforeTheCardIsAppliedWhenItBegins() {
        val b = SendNoticeBook().end("t1", SendPhase.Cancelled).begin("p", notice("t1"))
        assertEquals(SendPhase.Cancelled, b.notices["p"]!!.phase)
    }

    @Test
    fun anOutcomeWhileThePageIsClosedIsThereWhenItOpens() {
        // The book is process-wide: the end lands on the card with no screen reading it.
        val b = SendNoticeBook().begin("p", notice("t1")).end("t1", SendPhase.Sent)
        assertEquals(SendPhase.Sent, b.notices["p"]!!.phase)
    }

    @Test
    fun dismissingHidesOnlyThatDevicesCard() {
        val b = SendNoticeBook().begin("p", notice("t1")).begin("q", notice("t2")).dismiss("p")
        assertNull(b.notices["p"])
        assertTrue("q" in b.notices)
    }

    @Test
    fun rememberedEndsAreBounded() {
        var b = SendNoticeBook()
        for (i in 1..(SendNoticeBook.MAX_ENDED + 10)) b = b.end("t$i", SendPhase.Sent)
        assertEquals(SendNoticeBook.MAX_ENDED, b.ended.size)
        assertTrue("t${SendNoticeBook.MAX_ENDED + 10}" in b.ended)
        assertNull(b.ended["t1"])
    }

    @Test
    fun aResumableFailureIsItsOwnPhaseAndTurnsPlainWhenTheCoreDropsIt() {
        val phase = SendNoticeBook.failedPhase(TransferFailure.CONNECTION_LOST, "", resumable = true) { it }
        assertEquals(SendPhase.FailedResumable(TransferFailure.CONNECTION_LOST, null), phase)
        var b = SendNoticeBook().begin("p", notice("t1")).end("t1", phase)
        assertEquals(phase, b.notices["p"]!!.phase)
        // Listed by the core: kept. Not listed: plain failure. Unknown: unchanged.
        assertEquals(phase, b.reconcile(setOf("t1")).notices["p"]!!.phase)
        assertEquals(phase, b.reconcile(null).notices["p"]!!.phase)
        b = b.reconcile(emptySet())
        assertEquals(SendPhase.Failed(TransferFailure.CONNECTION_LOST, null), b.notices["p"]!!.phase)
    }

    @Test
    fun cancelledAndPlainFailuresAreNeverResumable() {
        assertEquals(SendPhase.Cancelled, SendNoticeBook.failedPhase(TransferFailure.CANCELLED_BY_YOU, "", true) { it })
        assertEquals(SendPhase.Failed(TransferFailure.NO_SPACE, null), SendNoticeBook.failedPhase(TransferFailure.NO_SPACE, "", false) { it })
        assertEquals(SendPhase.Failed(TransferFailure.OTHER, "Because."), SendNoticeBook.failedPhase(TransferFailure.OTHER, "x", false) { "Because." })
    }
}

package org.playdrop.android.share

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test
import org.playdrop.core.HostEvent
import org.playdrop.core.TransferDirection
import org.playdrop.core.TransferFailure

/** The toast after a send of files to several devices says what really happened on each. */
class FileResultsTest {
    private val chosen = listOf("aa", "bb", "cc")
    private val labels = mapOf("aa" to "A", "bb" to "B", "cc" to "C")
    private val online = chosen.associateWith { true }
    private val files = EnglishPhrases.files(3)

    private fun sentence(done: ManySend.Report.Done, wasOnline: Map<String, Boolean> = online) =
        SendReport.sentence(fileResults(done, chosen, wasOnline, labels::getValue) { it.message ?: "?" }, files)

    @Test
    fun cancelAfterEverySendCompletedIsNotCalledCancelled() {
        val done = ManySend.Report.Done(listOf(ManySend.Device.Completed(0), ManySend.Device.Completed(1)), cancelled = true)
        assertEquals("Already sent to A and B.", SendReport.sentence(fileResults(done, chosen, online, labels::getValue), files))
    }

    @Test
    fun cancelNamesWhatWentWhatStoppedAndWhatNeverStarted() {
        val done = ManySend.Report.Done(
            listOf(ManySend.Device.Completed(0), ManySend.Device.Stopped(1), ManySend.Device.NotStarted(2)),
            cancelled = true,
        )
        assertEquals("Already sent to A. Stopped sending to B. Not sent to C.", sentence(done))
    }

    @Test
    fun withoutCancelFinishedSendsAreSentAndRunningOnesSending() {
        val done = ManySend.Report.Done(
            listOf(ManySend.Device.Completed(0), ManySend.Device.Running(1, "id-b"), ManySend.Device.Running(2, "id-c")),
            cancelled = false,
        )
        assertEquals(
            "Sent 3 items to A. Sending 3 items to B. C is offline. PlayDrop keeps trying for 10 minutes.",
            sentence(done, online + ("cc" to false)),
        )
    }

    @Test
    fun failuresKeepTheirReasons() {
        val done = ManySend.Report.Done(
            listOf(
                ManySend.Device.Completed(0),
                ManySend.Device.Failed(1, "the receiving device does not have enough free space"),
                ManySend.Device.FailedToStart(2, IllegalStateException("not paired")),
            ),
            cancelled = false,
        )
        assertEquals(
            "Sent 3 items to A. Could not send to B: the receiving device does not have enough free space Could not send to C: not paired",
            sentence(done),
        )
    }

    @Test
    fun onlyTheEndsOfSendsAreFollowed() {
        val complete = HostEvent.TransferComplete("t1", "aa", TransferDirection.SEND, 10uL, 5uL, emptyList(), null, emptyList())
        assertEquals(SendEnd("t1", completed = true, byCancel = false, reason = ""), sendEndOf(complete))
        assertNull(sendEndOf(complete.copy(direction = TransferDirection.RECEIVE)))
        val cancelled = HostEvent.TransferFailed("t2", "bb", TransferDirection.SEND, "cancelled", TransferFailure.CANCELLED_BY_YOU)
        assertEquals(SendEnd("t2", completed = false, byCancel = true, reason = "cancelled"), sendEndOf(cancelled))
        val noSpace = HostEvent.TransferFailed("t3", "bb", TransferDirection.SEND, "no space", TransferFailure.NO_SPACE)
        assertEquals(SendEnd("t3", completed = false, byCancel = false, reason = "no space"), sendEndOf(noSpace))
        assertNull(sendEndOf(noSpace.copy(direction = TransferDirection.RECEIVE)))
    }
}

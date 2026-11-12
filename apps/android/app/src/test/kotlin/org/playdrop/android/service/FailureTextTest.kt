package org.playdrop.android.service

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.playdrop.android.R
import org.playdrop.android.ui.common.UiMessage
import org.playdrop.core.TransferDirection
import org.playdrop.core.TransferFailure

/** A failed transfer is told in plain words; protocol talk never reaches the user. */
class FailureTextTest {
    private fun send(code: TransferFailure, text: String = "") = FailureText.failureSentence(code, text, "calliope", TransferDirection.SEND)
    private fun receive(code: TransferFailure, text: String = "") = FailureText.failureSentence(code, text, "calliope", TransferDirection.RECEIVE)

    @Test
    fun noSpaceNamesTheDeviceThatIsFull() {
        assertEquals(UiMessage(R.string.failure_no_space_peer, listOf("calliope")), send(TransferFailure.NO_SPACE, "offer refused: 507 Insufficient Storage no_space"))
        assertEquals(UiMessage(R.string.failure_no_space_here), receive(TransferFailure.NO_SPACE))
    }

    @Test
    fun everyNamedCodeHasItsOwnSentenceAndIgnoresTheCoreText() {
        val raw = "offer refused: 429 Too Many Requests rate_limited"
        TransferFailure.entries.filter { it != TransferFailure.OTHER }.forEach { code ->
            for (m in listOf(send(code, raw), receive(code, raw))) {
                assertTrue(code.name, m.res != R.string.failure_other && m.res != R.string.failure_other_detail)
                assertTrue(code.name, m.args.none { it.toString().contains("429") })
            }
        }
    }

    @Test
    fun otherShowsPlainCoreTextOnly() {
        assertEquals(UiMessage(R.string.failure_other_detail, listOf("The files could not be read.")), send(TransferFailure.OTHER, "the files could not be read"))
        assertEquals(UiMessage(R.string.failure_other), send(TransferFailure.OTHER, "offer refused: 507 Insufficient Storage no_space"))
        assertEquals(UiMessage(R.string.failure_other), send(TransferFailure.OTHER, "HTTP error"))
        assertEquals(UiMessage(R.string.failure_other), send(TransferFailure.OTHER, "  "))
    }

    @Test
    fun wireLikeTextIsRecognised() {
        assertNull(FailureText.plainOrNull("413 too_large"))
        assertNull(FailureText.plainOrNull("bad_request"))
        assertNull(FailureText.plainOrNull("Service Unavailable"))
        assertEquals("Disk is full.", FailureText.plainOrNull("disk is full"))
        assertEquals("Stopped!", FailureText.plainOrNull("Stopped!"))
    }
}

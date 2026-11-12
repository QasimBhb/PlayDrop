package org.playdrop.android.ui.input

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import org.playdrop.android.R
import org.playdrop.android.ui.common.UiMessage
import org.playdrop.core.InputEndReason

/** Each reason the core gives for an ended input session becomes its own plain sentence. */
class InputEndTest {
    private fun of(code: InputEndReason, detail: String = "ended by the device") = InputEnd.of(code, "calliope", detail)

    @Test
    fun everyCodeHasItsOwnSentence() {
        val expected = mapOf(
            InputEndReason.GRANT_OFF to R.string.ui_input_end_grant_off,
            InputEndReason.LOCKED to R.string.ui_input_end_locked,
            InputEndReason.NOT_ACCEPTED to R.string.ui_input_end_not_accepted,
            InputEndReason.STOPPED_BY_USER to R.string.ui_input_end_stopped,
            InputEndReason.IDLE_TIMEOUT to R.string.ui_input_end_idle,
            InputEndReason.CONNECTION_LOST to R.string.ui_input_end_connection,
            InputEndReason.OTHER to R.string.ui_input_end_other_detail,
        )
        assertEquals(InputEndReason.entries.toSet(), expected.keys)
        expected.forEach { (code, res) -> assertEquals(code.name, res, of(code).message.res) }
        assertEquals(expected.values.size, expected.values.toSet().size)
    }

    @Test
    fun sentencesNameTheComputer() {
        assertEquals(UiMessage(R.string.ui_input_end_grant_off, listOf("calliope")), of(InputEndReason.GRANT_OFF).message)
        assertEquals(UiMessage(R.string.ui_input_end_locked, listOf("calliope")), of(InputEndReason.LOCKED).message)
        assertEquals(UiMessage(R.string.ui_input_end_idle), of(InputEndReason.IDLE_TIMEOUT).message)
    }

    @Test
    fun otherUsesTheCoresTextOnlyWhenThereIsSome() {
        assertEquals(UiMessage(R.string.ui_input_end_other_detail, listOf("calliope", "ended by the device")), of(InputEndReason.OTHER).message)
        assertEquals(UiMessage(R.string.ui_input_end_other, listOf("calliope")), of(InputEndReason.OTHER, " ").message)
        // The core's text is used for no other code.
        assertFalse(of(InputEndReason.LOCKED, "the desktop is locked").message.args.contains("the desktop is locked"))
    }

    @Test
    fun tryAgainIsOfferedWhenSomethingCanChange() {
        assertTrue(of(InputEndReason.GRANT_OFF).canRetry)
        assertTrue(of(InputEndReason.LOCKED).canRetry)
        assertTrue(of(InputEndReason.IDLE_TIMEOUT).canRetry)
        assertFalse(of(InputEndReason.NOT_ACCEPTED).canRetry)
    }
}

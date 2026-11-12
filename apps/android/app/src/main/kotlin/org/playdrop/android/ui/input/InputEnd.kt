package org.playdrop.android.ui.input

import org.playdrop.android.R
import org.playdrop.android.ui.common.UiMessage
import org.playdrop.core.InputEndReason

/**
 * Why remote input stopped, as the screen shows it: a plain sentence that says what to do,
 * kept on screen until the user closes it or tries again (protocol.md 5.12). Pure, so the
 * wording per reason is unit-tested.
 */
data class InputEnd(
    /** Why, as the core reported it. */
    val code: InputEndReason,
    /** The sentence to show. */
    val message: UiMessage,
    /** Offer "Try again": something the user can change on the computer may make it work. */
    val canRetry: Boolean,
) {
    companion object {
        /**
         * The notice for [code]. [label] is the computer's local name; [detail] is the core's
         * own English reason, used only for [InputEndReason.OTHER] (it is plain text, never a
         * wire code).
         */
        fun of(code: InputEndReason, label: String, detail: String): InputEnd = when (code) {
            InputEndReason.GRANT_OFF -> InputEnd(code, UiMessage(R.string.ui_input_end_grant_off, listOf(label)), canRetry = true)
            InputEndReason.LOCKED -> InputEnd(code, UiMessage(R.string.ui_input_end_locked, listOf(label)), canRetry = true)
            InputEndReason.NOT_ACCEPTED -> InputEnd(code, UiMessage(R.string.ui_input_end_not_accepted, listOf(label)), canRetry = false)
            InputEndReason.STOPPED_BY_USER -> InputEnd(code, UiMessage(R.string.ui_input_end_stopped, listOf(label)), canRetry = true)
            InputEndReason.IDLE_TIMEOUT -> InputEnd(code, UiMessage(R.string.ui_input_end_idle), canRetry = true)
            InputEndReason.CONNECTION_LOST -> InputEnd(code, UiMessage(R.string.ui_input_end_connection, listOf(label)), canRetry = true)
            InputEndReason.OTHER ->
                if (detail.isBlank()) {
                    InputEnd(code, UiMessage(R.string.ui_input_end_other, listOf(label)), canRetry = true)
                } else {
                    InputEnd(code, UiMessage(R.string.ui_input_end_other_detail, listOf(label, detail)), canRetry = true)
                }
        }
    }
}

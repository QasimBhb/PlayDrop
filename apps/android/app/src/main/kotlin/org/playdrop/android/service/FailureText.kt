package org.playdrop.android.service

import org.playdrop.android.R
import org.playdrop.android.ui.common.UiMessage
import org.playdrop.core.TransferDirection
import org.playdrop.core.TransferFailure

/**
 * The one place a failed transfer becomes words for the user (anti-pattern 18). The core
 * gives a machine-readable [TransferFailure] next to its English text; every named failure
 * gets its own plain sentence here, and only [TransferFailure.OTHER] falls back to the core's
 * text, and only when that text carries no HTTP status or wire code. Pure, so the wording is
 * unit-tested.
 */
internal object FailureText {
    /**
     * The body of the "Sending to X failed" or "Receiving from X failed" notification.
     * [peerLabel] is the other device's local name, [direction] our side of the transfer.
     */
    fun failureSentence(code: TransferFailure, fallbackText: String, peerLabel: String, direction: TransferDirection): UiMessage {
        val sending = direction == TransferDirection.SEND
        fun msg(res: Int) = UiMessage(res, listOf(peerLabel))
        return when (code) {
            TransferFailure.NO_SPACE -> if (sending) msg(R.string.failure_no_space_peer) else UiMessage(R.string.failure_no_space_here)
            TransferFailure.DECLINED -> if (sending) msg(R.string.failure_declined_peer) else UiMessage(R.string.failure_declined_here)
            TransferFailure.TOO_LARGE -> if (sending) msg(R.string.failure_too_large_peer) else UiMessage(R.string.failure_too_large_here)
            TransferFailure.RATE_LIMITED -> if (sending) msg(R.string.failure_busy_peer) else msg(R.string.failure_busy_here)
            TransferFailure.CANCELLED_BY_PEER -> msg(R.string.failure_cancelled_by_peer)
            TransferFailure.CANCELLED_BY_YOU -> UiMessage(R.string.failure_cancelled_by_you)
            TransferFailure.INTEGRITY -> if (sending) UiMessage(R.string.failure_integrity_send) else msg(R.string.failure_integrity_receive)
            TransferFailure.CONNECTION_LOST -> msg(R.string.failure_connection_lost)
            TransferFailure.TIMED_OUT -> if (sending) msg(R.string.failure_timed_out_send) else msg(R.string.failure_timed_out_receive)
            TransferFailure.OTHER -> plainOrNull(fallbackText)?.let { UiMessage(R.string.failure_other_detail, listOf(it)) }
                ?: UiMessage(R.string.failure_other)
        }
    }

    /**
     * [text] with a capital first letter and a full stop, or null when it is empty or looks
     * like protocol talk: an HTTP status ("507"), a wire code ("no_space") or an HTTP reason
     * phrase. Such text must never reach the user.
     */
    fun plainOrNull(text: String): String? {
        val t = text.trim()
        if (t.isEmpty() || WIRE_LIKE.any { it.containsMatchIn(t) }) return null
        val first = t.replaceFirstChar { it.uppercaseChar() }
        return if (first.last() in ".!?") first else "$first."
    }

    private val WIRE_LIKE = listOf(
        // A three-digit HTTP status.
        Regex("""\b[1-5]\d\d\b"""),
        // A snake_case code such as no_space or rate_limited.
        Regex("""\b[a-z0-9]+_[a-z0-9_]+\b"""),
        // HTTP itself, or a reason phrase in Title Case ("Insufficient Storage").
        Regex("""\bHTTP\b|\b(Insufficient Storage|Too Many Requests|Payload Too Large|Forbidden|Not Found|Bad Request|Internal Server Error|Service Unavailable)\b"""),
    )
}

package org.playdrop.android.ui.common

import android.content.Context
import kotlinx.coroutines.CancellationException
import org.playdrop.android.R
import org.playdrop.core.PlayDropException

/**
 * A user-visible line of text kept as a resource ID plus arguments, so view models stay free
 * of [Context] and tests can compare messages without resources (anti-pattern 18: every
 * failure reaches the user as words).
 */
data class UiMessage(
    /** A string resource, or a plurals resource when [quantity] is set. */
    val res: Int,
    /** Format arguments. */
    val args: List<Any> = emptyList(),
    /** Selects the plural form of a plurals resource. */
    val quantity: Int? = null,
) {
    /** The text in the current locale. */
    fun resolve(context: Context): String = when {
        quantity != null -> context.resources.getQuantityString(res, quantity, *args.toTypedArray())
        args.isEmpty() -> context.getString(res)
        else -> context.getString(res, *args.toTypedArray())
    }

    companion object {
        /**
         * The message for a failed core call. Details from the core are plain reasons (no
         * SAS, QR secret or clipboard text ever reaches an error, SR-32).
         */
        fun from(error: Throwable): UiMessage = when (error) {
            is PlayDropException.NotPaired -> UiMessage(R.string.ui_err_not_paired)
            is PlayDropException.Forbidden -> UiMessage(R.string.ui_err_forbidden)
            is PlayDropException.Locked -> UiMessage(R.string.ui_err_locked)
            is PlayDropException.NotFound -> UiMessage(R.string.ui_err_not_found)
            is PlayDropException.TooLarge -> UiMessage(R.string.ui_err_too_large)
            is PlayDropException.PortHeld -> UiMessage(R.string.ui_err_port_held, listOf(error.port.toInt()))
            is PlayDropException.InsecurePermissions -> UiMessage(R.string.ui_err_insecure)
            is PlayDropException.NoSpace -> UiMessage(R.string.ui_err_no_space)
            is PlayDropException.PathInvalid -> UiMessage(R.string.ui_err_path_invalid)
            is PlayDropException.PairRejected -> UiMessage(R.string.ui_err_pair_rejected)
            is PlayDropException.Io -> UiMessage(R.string.ui_err_io, listOf(error.detail))
            is PlayDropException.Other -> UiMessage(R.string.ui_err_other, listOf(error.detail))
            is IllegalStateException -> UiMessage(R.string.ui_err_not_running)
            else -> UiMessage(R.string.ui_err_other, listOf(error.message ?: error.javaClass.simpleName))
        }

        /** Rethrows coroutine cancellation so callers never turn it into a message. */
        fun fromOrRethrow(error: Throwable): UiMessage {
            if (error is CancellationException) throw error
            return from(error)
        }
    }
}

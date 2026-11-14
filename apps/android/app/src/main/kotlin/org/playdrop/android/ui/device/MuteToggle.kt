package org.playdrop.android.ui.device

import org.playdrop.core.MediaAction
import org.playdrop.core.MediaState

/**
 * Pure rules for the computer's mute button, kept apart from Compose so they are unit-tested.
 * Times are monotonic ms.
 *
 * A tap shows its result at once and holds it until the computer echoes the same value or
 * [holdMs] passes, so a late echo does not make the button flicker back. A volume above 0 sent
 * from the slider unmutes the computer, so it is held as "not muted" the same way.
 */
data class MuteToggle(
    /** The value shown until it is echoed or [holdMs] passes, else null. */
    val held: Boolean? = null,
    val heldAtMs: Long = 0,
    val holdMs: Long = DEFAULT_HOLD_MS,
) {
    /** A new state plus the mute value to send now, if any. */
    data class Step(val state: MuteToggle, val send: Boolean?)

    /** The value to show, given the [incoming] value the computer last reported. */
    fun shown(incoming: Boolean): Boolean = held ?: incoming

    /** The button was tapped: flips what is shown, holds it and sends it. */
    fun tap(incoming: Boolean, nowMs: Long): Step {
        val target = !shown(incoming)
        return Step(copy(held = target, heldAtMs = nowMs), target)
    }

    /** A volume above 0 was sent: the computer unmutes itself, so show that without a command. */
    fun unmutedByVolume(incoming: Boolean, nowMs: Long): MuteToggle =
        if (shown(incoming)) copy(held = false, heldAtMs = nowMs) else this

    /** The computer reported [value]; an echo of the held value ends the hold. */
    fun incoming(value: Boolean): MuteToggle = if (held != null && value == held) copy(held = null) else this

    /** When the hold ends; null when nothing is held. */
    fun holdUntilMs(): Long? = if (held == null) null else heldAtMs + holdMs

    /** Drops the held value once [holdMs] has passed at [nowMs]. */
    fun expire(nowMs: Long): MuteToggle {
        val until = holdUntilMs() ?: return this
        return if (nowMs >= until) copy(held = null) else this
    }

    companion object {
        /** How long a tapped value waits for the computer's echo. */
        const val DEFAULT_HOLD_MS = 2_000L

        /**
         * True when [state] offers the mute command and says whether it is muted (a desktop from
         * 0.16.0). A host may leave `muted` out when it cannot tell; reading that as "not muted"
         * would make unmuting impossible, so no button is shown then.
         */
        fun canMute(state: MediaState): Boolean = MediaAction.MUTE in state.can && state.muted != null

        /** Whether [state] reports the output as muted; false when it cannot tell (an older desktop). */
        fun reportedMuted(state: MediaState): Boolean = state.muted == true
    }
}

package org.playdrop.android.ui.device

/**
 * Pure rules for a volume slider that sends commands while it is dragged, kept apart from
 * Compose so they are unit-tested. Values are 0 to 100; times are monotonic ms.
 *
 * - While the finger is down the slider shows the finger's value and ignores incoming states.
 * - Commands during a drag go out at most once per [intervalMs]; a value held back by the
 *   throttle is sent when the interval ends ([flushAtMs], [flush]) or on release.
 * - On release the final value is always sent (unless it was the last one sent), and the
 *   slider keeps showing it until the computer echoes that value back or [holdMs] passes, so
 *   it does not jump back to the state that was current before the echo arrived.
 *
 * Every transition returns a new value; [Step.send] is the volume to send now, if any.
 */
data class VolumeSlider(
    /** The finger's value while the slider is held, else null. */
    val dragging: Int? = null,
    /** A value the throttle held back during the drag. */
    val pending: Int? = null,
    /** The last value sent, and when. */
    val lastSent: Int? = null,
    val lastSentAtMs: Long? = null,
    /** The value released, shown until it is echoed or [holdMs] passes. */
    val released: Int? = null,
    val releasedAtMs: Long = 0,
    val intervalMs: Long = DEFAULT_INTERVAL_MS,
    val holdMs: Long = DEFAULT_HOLD_MS,
) {
    /** A new state plus the volume to send now, if any. */
    data class Step(val state: VolumeSlider, val send: Int?)

    /** The value to show, given the [incoming] value the computer last reported. */
    fun shown(incoming: Int): Int = dragging ?: released ?: incoming

    /** The finger moved to [value] at [nowMs]. */
    fun drag(value: Int, nowMs: Long): Step {
        val v = value.coerceIn(0, MAX)
        val held = copy(dragging = v, released = null)
        return when {
            v == lastSent -> Step(held.copy(pending = null), null)
            canSend(nowMs) -> Step(held.sent(v, nowMs), v)
            else -> Step(held.copy(pending = v), null)
        }
    }

    /** When a held-back value may go out; null when nothing waits. */
    fun flushAtMs(): Long? = if (pending == null) null else (lastSentAtMs ?: 0) + intervalMs

    /** Sends the held-back value once its interval has passed. */
    fun flush(nowMs: Long): Step {
        val v = pending ?: return Step(this, null)
        return if (dragging != null && canSend(nowMs)) Step(sent(v, nowMs), v) else Step(this, null)
    }

    /** The finger lifted at [nowMs]: sends the final value and holds it on screen. */
    fun release(nowMs: Long): Step {
        val v = dragging ?: return Step(this, null)
        val done = copy(dragging = null, pending = null, released = v, releasedAtMs = nowMs)
        return if (v == lastSent) Step(done, null) else Step(done.sent(v, nowMs), v)
    }

    /** The computer reported [value]; an echo of the released value ends the hold. */
    fun incoming(value: Int): VolumeSlider = if (released != null && value == released) copy(released = null) else this

    /** When the hold on the released value ends; null when nothing is held. */
    fun holdUntilMs(): Long? = if (released == null) null else releasedAtMs + holdMs

    /** Drops the held released value once [holdMs] has passed at [nowMs]. */
    fun expire(nowMs: Long): VolumeSlider {
        val until = holdUntilMs() ?: return this
        return if (nowMs >= until) copy(released = null) else this
    }

    private fun canSend(nowMs: Long): Boolean = lastSentAtMs == null || nowMs - lastSentAtMs >= intervalMs

    private fun sent(v: Int, nowMs: Long): VolumeSlider = copy(pending = null, lastSent = v, lastSentAtMs = nowMs)

    companion object {
        /** 5 commands per second at most while dragging. */
        const val DEFAULT_INTERVAL_MS = 200L

        /** How long a released value waits for the computer's echo. */
        const val DEFAULT_HOLD_MS = 2_000L

        private const val MAX = 100
    }
}

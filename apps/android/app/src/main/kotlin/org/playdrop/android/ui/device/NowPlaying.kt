package org.playdrop.android.ui.device

import org.playdrop.core.MediaAction
import org.playdrop.core.MediaState

/**
 * Pure rules for the "Now playing" card: where the track is now and which button does
 * what. The desktop sends a position with the time it was read and never streams it
 * (protocol.md 5.9), so the phone extrapolates while the player is playing.
 */
object NowPlaying {
    /**
     * The position in ms at [nowMs] (Unix ms): the reported position plus the time since it
     * was read while playing, clamped to the track. A clock behind the desktop's never moves
     * the position backwards. Null when the player reports no position.
     */
    fun positionMs(state: MediaState, nowMs: Long): Long? {
        val base = state.positionMs?.toLong() ?: return null
        val at = state.positionAtMs?.toLong()
        val elapsed = if (state.playing && at != null) (nowMs - at).coerceAtLeast(0) else 0
        val pos = base + elapsed
        val duration = state.durationMs?.toLong()
        return if (duration != null && duration > 0) pos.coerceIn(0, duration) else pos.coerceAtLeast(0)
    }

    /** Progress 0 to 1, or null without both a position and a duration. */
    fun fraction(state: MediaState, nowMs: Long): Float? {
        val duration = state.durationMs?.toLong()?.takeIf { it > 0 } ?: return null
        val pos = positionMs(state, nowMs) ?: return null
        return pos.toFloat() / duration
    }

    /**
     * The action behind the play/pause button: the toggle when offered, else the one that
     * changes the current state; null hides the button.
     */
    fun toggleAction(state: MediaState): MediaAction? = when {
        MediaAction.PLAY_PAUSE in state.can -> MediaAction.PLAY_PAUSE
        state.playing && MediaAction.PAUSE in state.can -> MediaAction.PAUSE
        !state.playing && MediaAction.PLAY in state.can -> MediaAction.PLAY
        else -> null
    }

    /** "m:ss" or "h:mm:ss". */
    fun clock(ms: Long): String {
        val total = ms / 1000
        val h = total / 3600
        val m = (total % 3600) / 60
        val s = total % 60
        return if (h > 0) "%d:%02d:%02d".format(h, m, s) else "%d:%02d".format(m, s)
    }
}

package org.playdrop.android.core

import org.playdrop.core.MediaState

/**
 * Pure bookkeeping for the media players each peer reports (protocol.md 5.9), kept apart from
 * [CoreHostImpl] so the rules are unit-tested without the native core.
 */
object MediaPlayers {
    /**
     * A state that is not playing and offers no action means the player went away (the
     * desktop sends this when an MPRIS or SMTC player closes).
     */
    fun isGone(state: MediaState): Boolean = !state.playing && state.can.isEmpty()

    /**
     * [all] with [state] applied for [peer]: replaces the player with the same ID in place,
     * appends a new one, or removes it when [isGone]. A peer left with no players drops out
     * of the map, so "no key" always means "nothing to show".
     */
    fun apply(all: Map<String, List<MediaState>>, peer: String, state: MediaState): Map<String, List<MediaState>> {
        val current = all[peer].orEmpty()
        val next = if (isGone(state)) {
            current.filterNot { it.player == state.player }
        } else {
            val at = current.indexOfFirst { it.player == state.player }
            if (at < 0) current + state else current.toMutableList().also { it[at] = state }
        }
        return if (next.isEmpty()) all - peer else all + (peer to next)
    }

    /** [all] without [peer]'s players; used when the peer goes offline or is unpaired. */
    fun forget(all: Map<String, List<MediaState>>, peer: String): Map<String, List<MediaState>> =
        if (peer in all) all - peer else all
}

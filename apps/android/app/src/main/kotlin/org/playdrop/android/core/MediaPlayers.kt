package org.playdrop.android.core

import org.playdrop.core.MediaState

/**
 * Pure bookkeeping for the media players each peer reports (protocol.md 5.9), kept apart from
 * [CoreHostImpl] so the rules are unit-tested without the native core.
 */
object MediaPlayers {
    /**
     * Player ID of the reserved entry that carries the computer's master volume (0 to 100, 0
     * when muted). No real player can have this ID. It only offers a volume command, and a
     * `volume` command sent to it sets the master volume. It is a control, never a
     * "Now playing" card.
     */
    const val SYSTEM_VOLUME_ID = "@system"

    /** True when [state] is the computer volume entry rather than a real player. */
    fun isSystemVolume(state: MediaState): Boolean = state.player == SYSTEM_VOLUME_ID

    /** The computer volume entry in [players], or null when the peer does not offer one. */
    fun systemVolume(players: List<MediaState>): MediaState? = players.firstOrNull(::isSystemVolume)

    /** [players] without the computer volume entry: the ones shown as "Now playing" cards. */
    fun realPlayers(players: List<MediaState>): List<MediaState> = players.filterNot(::isSystemVolume)

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

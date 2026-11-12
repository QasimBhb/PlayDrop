package org.playdrop.android.service

/**
 * When the app starts [CoreService]. A running service is started again only to post its
 * notification anew: on Android 14 and newer the user can swipe a foreground service's
 * notification away, and the service goes on without it until something posts it again.
 * Its own updates post only when the numbers change, so opening the app is what brings it
 * back ([needed] with `repost`).
 */
internal object ServiceStart {
    /** What the persistent notification shows. */
    data class Shown(val running: Boolean, val online: Int, val defaultLabel: String?)

    /** True when the service must be started: it is not running, or the caller wants the notification posted again. */
    fun needed(running: Boolean, repost: Boolean): Boolean = !running || repost
}

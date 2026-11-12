package org.playdrop.android.service

/**
 * What the phone may mirror (docs/design/notification-mirroring.md, SR-41), as a pure
 * function so JUnit can check every rule without Android. [MirrorListener] reads a posted
 * notification into a [MirrorFilter.Candidate] and sends only what [decide] returns.
 */
internal object MirrorFilter {
    /** `Notification.VISIBILITY_PUBLIC`. */
    const val VISIBILITY_PUBLIC = 1

    /** `Notification.VISIBILITY_PRIVATE` (the platform default). */
    const val VISIBILITY_PRIVATE = 0

    /** `Notification.VISIBILITY_SECRET`. */
    const val VISIBILITY_SECRET = -1

    /** One posted notification, reduced to what the rules look at. */
    data class Candidate(
        /** The posting app's package. */
        val packageName: String,
        /** True when the notification belongs to another user or a work profile. */
        val otherUser: Boolean,
        /** `FLAG_ONGOING_EVENT` (or `isOngoing`). */
        val ongoing: Boolean,
        /** `FLAG_FOREGROUND_SERVICE`. */
        val foregroundService: Boolean,
        /** `FLAG_GROUP_SUMMARY`. */
        val groupSummary: Boolean,
        /** `Notification.visibility`. */
        val visibility: Int,
        /** `EXTRA_TITLE`, or null. */
        val title: String?,
        /** `EXTRA_TEXT` (or `EXTRA_BIG_TEXT` when the app gave no short text), or null. */
        val text: String?,
    )

    /** What goes on the wire: app name, title and text (the core cuts them to size). */
    data class Outgoing(val app: String, val title: String, val text: String) {
        /**
         * 64-bit FNV-1a over app, title and text, so an identical repost can be skipped
         * without keeping the text itself.
         */
        fun contentHash(): Long {
            var h = FNV_OFFSET
            for (part in arrayOf(app, title, text)) {
                for (ch in part) {
                    h = (h xor ch.code.toLong()) * FNV_PRIME
                }
                h = (h xor 0xFFFFL) * FNV_PRIME // separator, not a UTF-16 unit any field holds alone
            }
            return h
        }
    }

    private const val FNV_OFFSET = -0x340d631b7bdddcdbL
    private const val FNV_PRIME = 0x100000001b3L

    /**
     * The content to mirror, or null when [candidate] must stay on the phone. [ownPackage] is
     * PlayDrop itself, [allowed] the user's allow list and [appLabel] the posting app's name.
     * Public and private notifications go with their real title and text: the allow list is
     * the consent, and private only governs the phone's lock screen. Secret never leaves.
     */
    fun decide(
        candidate: Candidate,
        ownPackage: String,
        allowed: Set<String>,
        appLabel: String,
    ): Outgoing? {
        val c = candidate
        if (c.packageName == ownPackage) return null
        if (c.packageName !in allowed) return null
        if (c.otherUser) return null
        if (c.ongoing || c.foregroundService || c.groupSummary) return null
        if (c.visibility == VISIBILITY_SECRET) return null
        val title = c.title.clean()
        val text = c.text.clean()
        if (title.isEmpty() && text.isEmpty()) return null
        val app = appLabel.clean().ifEmpty { c.packageName }
        return Outgoing(app, title, text)
    }

    /** At most this many notifications are sent again when a peer comes back (SR-44 headroom). */
    const val RESYNC_LIMIT = 20

    /**
     * The notifications to send again when a peer comes online: the newest [limit] by
     * [postTime], returned oldest first so the newest ends up on top of the desktop's stack
     * and is the last its 20-copy limit would close.
     */
    fun <T> forResync(items: List<T>, postTime: (T) -> Long, limit: Int = RESYNC_LIMIT): List<T> =
        items.sortedByDescending(postTime).take(limit).reversed()

    private fun String?.clean(): String = this?.trim().orEmpty()
}

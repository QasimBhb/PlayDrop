package org.playdrop.android.service

import org.playdrop.android.core.FileNames

/**
 * The decisions behind each notification, kept free of Android types so they are
 * unit-tested on the JVM: notification IDs, the actions a notification carries (as plain
 * data that [Notifications] turns into explicit, immutable PendingIntents, SR-29), and what
 * tapping a "received" notification opens (SR-23).
 */
internal object NotificationPlan {
    /** The persistent foreground-service notification. */
    const val SERVICE_ID = 1

    /** The one "find my phone" notification. */
    const val FIND_ID = 2

    /** Start-up failure notice. */
    const val START_FAILED_ID = 3

    /** Action: accept a transfer offer. Extra [EXTRA_TRANSFER_ID]. */
    const val ACTION_ACCEPT_OFFER = "org.playdrop.android.action.ACCEPT_OFFER"

    /** Action: decline a transfer offer. Extra [EXTRA_TRANSFER_ID]. */
    const val ACTION_DECLINE_OFFER = "org.playdrop.android.action.DECLINE_OFFER"

    /** Action: stop the find ringer. */
    const val ACTION_STOP_FIND = "org.playdrop.android.action.STOP_FIND"

    /** Action: retry moving landed files into Downloads now. */
    const val ACTION_RETRY_MOVE = "org.playdrop.android.action.RETRY_MOVE"

    /** Tag of the ongoing "Moving N of M files" notifications, so a start can find leftovers. */
    const val MOVING_TAG = "moving"

    /** Transfer ID extra on offer actions. */
    const val EXTRA_TRANSFER_ID = "org.playdrop.android.extra.TRANSFER_ID"

    /** Notification ID extra, so the receiver can dismiss what it answered. */
    const val EXTRA_NOTIFICATION_ID = "org.playdrop.android.extra.NOTIFICATION_ID"

    /** Notification IDs at or above this are per-event; below it are the fixed ones. */
    private const val DYNAMIC_BASE = 1_000

    /** Kinds of per-event notification; each gets its own ID space. */
    enum class Kind(internal val slot: Int) {
        /** A transfer offer waiting for an answer. */
        OFFER(0),

        /** A finished receive. */
        RECEIVED(1),

        /** A failed transfer. */
        FAILED(2),

        /** A ping. */
        PING(3),

        /** A new pairing. */
        PAIRED(4),

        /** A sensitive clipboard notice. */
        CLIPBOARD(5),

        /** Landed files that could not be moved into Downloads. */
        MOVE_INCOMPLETE(6),
    }

    /**
     * Stable notification ID for one event about [key] (a transfer ID or a peer ID). Different
     * kinds never collide for the same key, so a "received" notice replaces nothing but an
     * earlier "received" notice for the same transfer.
     */
    fun notificationId(kind: Kind, key: String): Int {
        val bucket = (key.hashCode() and 0x00FF_FFFF) // 24 bits of the key
        return DYNAMIC_BASE + bucket * Kind.entries.size + kind.slot
    }

    /** One notification action as plain data. */
    data class ActionSpec(
        /** Intent action, handled by `ActionReceiver`. */
        val action: String,
        /** String extras. */
        val extras: Map<String, String>,
        /** PendingIntent request code; unique per action and notification. */
        val requestCode: Int,
    )

    /** Accept and Decline for an offer shown under [notificationId]. */
    fun offerActions(transferId: String, notificationId: Int): List<ActionSpec> = listOf(
        ActionSpec(ACTION_ACCEPT_OFFER, offerExtras(transferId, notificationId), notificationId * 4 + 1),
        ActionSpec(ACTION_DECLINE_OFFER, offerExtras(transferId, notificationId), notificationId * 4 + 2),
    )

    /** The "Try again" action of the notice about files that could not be moved. */
    fun retryMoveAction(notificationId: Int): ActionSpec = ActionSpec(ACTION_RETRY_MOVE, emptyMap(), notificationId * 4 + 3)

    /** The Stop action of the find notification. */
    fun stopFindAction(): ActionSpec = ActionSpec(ACTION_STOP_FIND, emptyMap(), FIND_ID * 4 + 3)

    private fun offerExtras(transferId: String, notificationId: Int) = mapOf(
        EXTRA_TRANSFER_ID to transferId,
        EXTRA_NOTIFICATION_ID to notificationId.toString(),
    )

    /** One delivered file: manifest path and the `content://` location. */
    data class Delivered(val path: String, val location: String)

    /** What a "received" notification shows and opens. */
    sealed class Received {
        /** One file: show its name, open it with ACTION_VIEW. */
        data class One(val name: String, val uri: String, val mime: String?) : Received()

        /** One file of a risky type (SR-23): show its name, open the Downloads list instead. */
        data class OneRisky(val name: String) : Received()

        /** Several files: show the count, open the Downloads list. */
        data class Many(val count: Int) : Received()
    }

    /**
     * Decides the "received" notification for a completed receive. With [forceFolder] it
     * opens the Downloads list even for one file: the location the core holds may name the
     * wrong file (a landed file that got another name when it was moved).
     */
    fun received(delivered: List<Delivered>, forceFolder: Boolean = false, mimeOf: (String) -> String?): Received {
        if (delivered.size != 1 || forceFolder) return Received.Many(delivered.size)
        val d = delivered.single()
        val name = FileNames.sanitize(d.path.substringAfterLast('/')) ?: FileNames.FALLBACK_NAME
        val mime = mimeOf(name)
        return if (FileNames.isRiskyToOpen(name, mime)) Received.OneRisky(name) else Received.One(name, d.location, mime)
    }

    /**
     * True when a "received" notification must open the Downloads list instead of the file.
     * Several files always do; a single file does too when the mover renamed it ([renamed]
     * above 0) and the new place is unknown ([relocated] false, for example the relocation
     * table could not be written). When the new place is known, the file opens directly.
     */
    fun needsFolder(deliveredCount: Int, renamed: Int, relocated: Boolean): Boolean =
        deliveredCount != 1 || (renamed > 0 && !relocated)

    /** The second line of the notice that ends a move: where the files are, and whether names changed. */
    enum class MoveDetail {
        /** Nothing unusual: the usual "saved in Download/PlayDrop" text. */
        SAVED,

        /** Some files got another name because theirs was taken: say how many. */
        RENAMED,
    }

    /** Which detail line a finished move shows, given how many files got a new name. */
    fun moveDetail(renamed: Int): MoveDetail = if (renamed > 0) MoveDetail.RENAMED else MoveDetail.SAVED

    /** Which notification ends a move of landed files. */
    enum class MoveNotice {
        /** The transfer completed: its normal "received" notification. */
        RECEIVED,

        /** The transfer failed or was cancelled: "N files arrived before the transfer stopped". */
        ARRIVED_BEFORE_END,

        /** Leftovers of a killed process: "N received files moved into Downloads". */
        MOVED_AFTER_RESTART,

        /** Nothing reached Downloads in this pass and nothing completed: no notice (a stale one is removed). */
        NONE,
    }

    /**
     * Decides the notice after a move pass. Landed files are kept whatever way the transfer
     * ended, so a failed or cancelled one still says how many of its files arrived.
     */
    fun moveNotice(completed: Boolean, endedEarly: Boolean, moved: Int): MoveNotice = when {
        completed -> MoveNotice.RECEIVED
        moved <= 0 -> MoveNotice.NONE
        endedEarly -> MoveNotice.ARRIVED_BEFORE_END
        else -> MoveNotice.MOVED_AFTER_RESTART
    }

    /** Human size, binary units with one decimal ("1.5 MiB"). */
    fun humanBytes(bytes: Long): String {
        if (bytes < 1024) return "$bytes B"
        val units = listOf("KiB", "MiB", "GiB", "TiB")
        var value = bytes.toDouble() / 1024
        var i = 0
        while (value >= 1024 && i < units.lastIndex) {
            value /= 1024
            i++
        }
        return String.format(java.util.Locale.ROOT, "%.1f %s", value, units[i])
    }
}

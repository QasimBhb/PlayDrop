package org.playdrop.android.core

/** How the move of one landed file into Downloads ended. */
internal enum class MoveOutcome {
    /** Renamed in place: no copy, the landed file is gone. */
    RENAMED,

    /** Copied, then the landed file deleted. */
    COPIED,

    /** The landed file was no longer there (an earlier pass moved it). */
    GONE,

    /** Could not be moved; the landed file stays for the next start to retry. */
    FAILED,
}

/**
 * What [DownloadsSink.moveLanded] reports for one file. [renumbered] is true when the file
 * reached Downloads under another name than the one promised at landing. [stopPass] asks the
 * mover to stop this pass: the file was copied but its landed copy could not be deleted, so
 * the journal must keep that entry until a later pass settles it, and no other copy may
 * overwrite it.
 */
internal data class MoveResult(val outcome: MoveOutcome, val renumbered: Boolean = false, val stopPass: Boolean = false)

/**
 * Bookkeeping of one mover pass over a transfer's landing folder: what happened to each file
 * and what the notification says. Pure JVM, unit-tested. Not thread-safe: one pass, one
 * coroutine.
 */
internal class MoveTally(val total: Int) {
    var renamed = 0
        private set
    var copied = 0
        private set
    var gone = 0
        private set
    var failed = 0
        private set

    /** Files that reached Downloads under another name than the one promised at landing. */
    var renumbered = 0
        private set

    /** Files handled so far, whatever the outcome. */
    val done: Int get() = renamed + copied + gone + failed

    /** Files that are in Downloads because of this pass. */
    val moved: Int get() = renamed + copied

    /** Records one [MoveResult]. */
    fun record(result: MoveResult) = record(result.outcome, result.renumbered)

    /**
     * Counts every file not handled yet as failed, for a pass that stopped early (an error, or
     * [MoveResult.stopPass]): they stay landed for the retry.
     */
    fun skipRest() {
        failed += (total - done).coerceAtLeast(0)
    }

    /** Records one file. [renumbered] is true when its promised name had to change. */
    fun record(outcome: MoveOutcome, renumbered: Boolean = false) {
        when (outcome) {
            MoveOutcome.RENAMED -> renamed++
            MoveOutcome.COPIED -> copied++
            MoveOutcome.GONE -> gone++
            MoveOutcome.FAILED -> failed++
        }
        if (renumbered && (outcome == MoveOutcome.RENAMED || outcome == MoveOutcome.COPIED)) this.renumbered++
    }

    /** One log line. */
    fun line(): String =
        "$done of $total files: $renamed renamed, $copied copied, $gone already gone, $failed failed, $renumbered renumbered"
}

/**
 * When the mover tries again within the process after a pass left files behind: after 1, 5
 * and 30 minutes, then 2 and 6 hours, then not until storage is mounted again, the user taps
 * "Try again" or the process restarts. Pure JVM.
 *
 * Why in process: the service is always on, so "next start" can be days away. Why it stops:
 * a file that failed six times over hours has a cause no timer fixes (a full disk, a refused
 * folder), and the notice already tells the user.
 */
internal object MoveRetry {
    private val DELAYS_MS = longArrayOf(60_000, 5 * 60_000, 30 * 60_000, 2 * 3_600_000, 6 * 3_600_000)

    /** Delay before retry number [attempt] (0 for the first retry), or null when there is none. */
    fun delayMs(attempt: Int): Long? = DELAYS_MS.getOrNull(attempt)
}

/**
 * Says when a progress notification is due: the first and last file always, otherwise at most
 * once per [intervalNanos], so a 5,000-file move posts a few dozen updates, not 5,000 (the
 * system drops updates faster than about 5 per second anyway). Pure JVM.
 */
internal class ProgressThrottle(private val intervalNanos: Long = 500_000_000L) {
    private var last = Long.MIN_VALUE

    /** True when the update for [done] of [total] at time [now] (nanoTime) should be posted. */
    fun due(done: Int, total: Int, now: Long): Boolean {
        val edge = done <= 1 || done >= total
        if (edge || last == Long.MIN_VALUE || now - last >= intervalNanos) {
            last = now
            return true
        }
        return false
    }
}

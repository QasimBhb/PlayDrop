package org.playdrop.android.core

import org.playdrop.core.TransferStatus

/**
 * When a received transfer lands in the app's own folder first and is moved into
 * `Download/PlayDrop` after it completes, instead of being written there file by file.
 * Pure JVM, unit-tested.
 *
 * Why: every file created under the public Downloads tree costs one MediaProvider database
 * insert behind the create, about 13 ms on the Galaxy A54, whatever its size. A flood of small
 * files is then bound by that insert (5,000 files of 4 to 64 KiB arrived at 1.9 MiB/s while
 * 100 medium files arrived at 39.8 MiB/s). A file created in the app's own external folder
 * costs about 0.2 ms, so the transfer runs at engine speed and the inserts are paid afterwards
 * by a background move the user does not wait on.
 *
 * Why not always: a landed file is not in Downloads until the move finishes, so a few files or
 * big files go straight there, where they show up the moment each one is verified.
 */
internal object LandingPolicy {
    /**
     * Fewest files for which landing pays off. Below it the whole insert cost is under about
     * 0.8 s (64 x 13 ms), which is not worth making the files wait for a move.
     */
    const val MIN_FILES: Long = 64

    /**
     * Largest mean file size that still lands. At about 40 MiB/s a 1 MiB file takes 25 ms on
     * the wire, so the 13 ms insert adds about half again; at larger means the insert is a
     * small share of the time and direct delivery is as fast as landing plus a move.
     */
    const val MAX_MEAN_BYTES: Long = 1L * 1024 * 1024

    /** True when a transfer of [fileCount] files and [totalBytes] bytes should land first. */
    fun shouldLand(fileCount: Long, totalBytes: Long): Boolean {
        if (fileCount < MIN_FILES || totalBytes < 0) return false
        // Mean below the threshold, compared without dividing so no rounding hides an edge.
        return totalBytes < MAX_MEAN_BYTES * fileCount
    }
}

/**
 * Whether a `TransferFailed` event ended its transfer. The core also emits one for a single
 * file's failed integrity check while the transfer keeps going (protocol.md 7.7), so the state
 * in the core's transfer list decides. Pure JVM, unit-tested.
 */
internal object TransferEnds {
    /**
     * True when [status] (the transfer's state in the core's list, null when it is not listed)
     * is final. An unlisted transfer counts as ended: moving its landed files is safe even if it
     * resumes later, since the mover is idempotent and its completion moves the rest.
     */
    fun isTerminal(status: TransferStatus?): Boolean = when (status) {
        null, TransferStatus.COMPLETE, TransferStatus.FAILED, TransferStatus.CANCELLED -> true
        TransferStatus.OFFERED, TransferStatus.ACTIVE -> false
    }
}

/**
 * How a failed `rename(2)` is handled. Pure JVM, unit-tested; [errnoKind] in the sink maps the
 * platform errno onto [ErrnoKind].
 */
internal enum class ErrnoKind {
    /** `EXDEV`: the two paths are on different mounts as far as the kernel or FUSE is concerned. */
    CROSS_DEVICE,

    /** `EACCES`. */
    ACCESS,

    /** `EPERM`. */
    PERMISSION,

    /** `ENOENT`: the source or the target folder went away. */
    NOT_FOUND,

    /** Anything else (an odd name, a full disk, an I/O error). */
    OTHER,
}

/** What to do after one failed rename. */
internal enum class RenameDecision {
    /** Copy this file instead and keep trying rename for the next ones. */
    COPY_THIS_FILE,

    /** Copy this file and every later one on this route: the platform refuses renames here. */
    COPY_FROM_NOW_ON,
}

/**
 * One rename route (staging to the landing folder). Starts open; the first refusal that
 * says the route can never work ([ErrnoKind.CROSS_DEVICE], [ErrnoKind.ACCESS],
 * [ErrnoKind.PERMISSION]) closes it for the rest of the process, so each later file goes
 * straight to the copy instead of paying a failed rename first. Thread-safe.
 */
internal class RenameGate(val route: String) {
    @Volatile
    var open: Boolean = true
        private set

    /** Renames that worked, for the log. */
    @Volatile
    var renamed: Long = 0
        private set

    /** Records a rename that worked; true for the first one, so the caller can log that it works. */
    @Synchronized
    fun succeeded(): Boolean {
        renamed++
        return renamed == 1L
    }

    /**
     * Records a failed rename of kind [kind]. Returns the decision, and whether this call is
     * the one that closed the gate (so the caller logs the reason exactly once).
     */
    @Synchronized
    fun failed(kind: ErrnoKind): Pair<RenameDecision, Boolean> {
        val decision = decide(kind)
        val closedNow = decision == RenameDecision.COPY_FROM_NOW_ON && open
        if (decision == RenameDecision.COPY_FROM_NOW_ON) open = false
        return decision to closedNow
    }

    companion object {
        /** The rule behind [failed]. */
        fun decide(kind: ErrnoKind): RenameDecision = when (kind) {
            ErrnoKind.CROSS_DEVICE, ErrnoKind.ACCESS, ErrnoKind.PERMISSION -> RenameDecision.COPY_FROM_NOW_ON
            ErrnoKind.NOT_FOUND, ErrnoKind.OTHER -> RenameDecision.COPY_THIS_FILE
        }
    }
}

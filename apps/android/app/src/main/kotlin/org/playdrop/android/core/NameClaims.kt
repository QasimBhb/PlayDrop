package org.playdrop.android.core

import java.util.concurrent.ConcurrentHashMap

/**
 * Name selection in `Download/PlayDrop` for everything this process writes there: one lock per
 * folder, and the names already promised to landed files that have not been moved yet. Pure
 * JVM, unit-tested.
 *
 * Why a lock: `rename(2)` replaces an existing target silently, so "check the name is free,
 * then rename" is only safe if no other thread of this process can create that name in
 * between. Every in-process path that picks a name in a folder (the direct create with
 * `O_EXCL`, the rename-first delivery, landing and the mover) does it inside [inFolder] for
 * that folder. Other apps are not covered; the window is the gap between one `stat` and one
 * `rename`, and the platform offers no `RENAME_NOREPLACE` to close it.
 *
 * Why reservations: a landed file's `content://` location is handed to the core when it lands,
 * long before the file reaches Downloads, so its name must stay free until the mover takes
 * it. A reservation maps the Downloads path to its owner (the landed file's path); only the
 * owner may take it.
 *
 * Why folded keys: shared storage is case-insensitive (`report.pdf` and `Report.pdf` are one
 * file), so folders and promises are keyed by [CaseFold.key], the core's rule. Owners are
 * compared as they are: they always come from the same code path.
 */
internal class NameClaims {
    private val locks = ConcurrentHashMap<String, Any>()
    private val reserved = ConcurrentHashMap<String, String>()

    /** Runs [block] holding the lock of [folder] (an absolute path). */
    fun <T> inFolder(folder: String, block: () -> T): T {
        val lock = locks.computeIfAbsent(CaseFold.key(folder)) { Any() }
        return synchronized(lock) { block() }
    }

    /**
     * Promises [target] to [owner]. Returns false when someone else holds it; true when it
     * was free or already promised to [owner] (so rebuilding the claims is idempotent).
     */
    fun reserve(target: String, owner: String): Boolean {
        val prev = reserved.putIfAbsent(CaseFold.key(target), owner)
        return prev == null || prev == owner
    }

    /** True when [target] is promised to anyone but [owner] (null owner: to anyone at all). */
    fun heldByOther(target: String, owner: String?): Boolean {
        val holder = reserved[CaseFold.key(target)] ?: return false
        return holder != owner
    }

    /** Drops the promise of [target] if [owner] holds it. */
    fun release(target: String, owner: String) {
        reserved.remove(CaseFold.key(target), owner)
    }

    /** Number of open promises. */
    val size: Int get() = reserved.size
}

/**
 * The case-fold key of a name or path, the core's rule (`fold` in
 * `crates/playdrop-core/src/transfer/manifest.rs`): upper then lower case, which also folds
 * `ß` and `SS` together. Stricter than shared storage needs, never looser. Pure JVM.
 */
internal object CaseFold {
    fun key(s: String): String = s.uppercase(java.util.Locale.ROOT).lowercase(java.util.Locale.ROOT)
}

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
 */
internal class NameClaims {
    private val locks = ConcurrentHashMap<String, Any>()
    private val reserved = ConcurrentHashMap<String, String>()

    /** Runs [block] holding the lock of [folder] (an absolute path). */
    fun <T> inFolder(folder: String, block: () -> T): T {
        val lock = locks.computeIfAbsent(folder) { Any() }
        return synchronized(lock) { block() }
    }

    /**
     * Promises [target] to [owner]. Returns false when someone else holds it; true when it
     * was free or already promised to [owner] (so rebuilding the claims is idempotent).
     */
    fun reserve(target: String, owner: String): Boolean {
        val prev = reserved.putIfAbsent(target, owner)
        return prev == null || prev == owner
    }

    /** True when [target] is promised to anyone but [owner] (null owner: to anyone at all). */
    fun heldByOther(target: String, owner: String?): Boolean {
        val holder = reserved[target] ?: return false
        return holder != owner
    }

    /** Drops the promise of [target] if [owner] holds it. */
    fun release(target: String, owner: String) {
        reserved.remove(target, owner)
    }

    /** Number of open promises. */
    val size: Int get() = reserved.size
}

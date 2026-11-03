package org.playdrop.android.core

import java.io.File
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.atomic.AtomicLongArray

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
 *
 * Why generations: a name check is a `stat` on shared storage, a call into MediaProvider
 * (about 0.5 ms on the test phone). Under the lock, thousands of landing deliveries on
 * several threads queued behind each other's `stat`s (Phase 11, `throughput.md` section
 * 13). Each folder counts the critical sections that may have put a file there and the
 * promises given back; a landing claim checks names outside the lock, then takes the lock
 * and keeps its pick only when that count did not move (see [generation]).
 */
internal class NameClaims {
    private val locks = ConcurrentHashMap<String, Any>()
    private val reserved = ConcurrentHashMap<String, String>()
    private val generations = AtomicLongArray(GENERATION_SLOTS)

    /**
     * Runs [block] holding the lock of [folder] (an absolute path). With [changesFiles] (any
     * section that may create a file there) the folder's [generation] moves on when the
     * block ends, after whatever it created; only a section that just promises a name may
     * pass false.
     */
    fun <T> inFolder(folder: String, changesFiles: Boolean = true, block: () -> T): T {
        val key = CaseFold.key(folder)
        val lock = locks.computeIfAbsent(key) { Any() }
        return synchronized(lock) {
            try {
                block()
            } finally {
                if (changesFiles) bump(key)
            }
        }
    }

    /**
     * A count that moves on after every section of [inFolder] that may have created a file
     * in [folder], and after every promise given back there ([release]; the file it kept
     * free is usually there by then). A name found free outside the lock, with the count
     * read before looking, is still free when the count is the same under the lock: every
     * in-process create in the folder happens before a move of the count. Other apps are
     * not covered, as before. Folders share a fixed number of counts, so this needs no
     * eviction and a count never goes back; two folders sharing one only cost a recheck.
     */
    fun generation(folder: String): Long = generations.get(slot(CaseFold.key(folder)))

    private fun bump(key: String) {
        generations.incrementAndGet(slot(key))
    }

    private fun slot(key: String): Int = key.hashCode() and (GENERATION_SLOTS - 1)

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
        val key = CaseFold.key(target)
        if (reserved[key] != owner) return
        // The count first: a checker that sees the name free must also see the count moved,
        // or it could keep a pick from a listing older than the file this promise covered.
        File(target).parent?.let { bump(CaseFold.key(it)) }
        reserved.remove(key, owner)
    }

    /** Number of open promises. */
    val size: Int get() = reserved.size

    private companion object {
        /** A power of two. */
        const val GENERATION_SLOTS = 256
    }
}

/**
 * Short-lived folder listings for name checks: one `readdir` instead of one `stat` per name.
 * At most one listing per folder per [maxAgeMs], one thread listing at a time, so a folder
 * that keeps changing (a mover running in it moves [NameClaims.generation] with every file)
 * costs no more than the `stat`s did: while a listing is younger than that but of another
 * generation, [names] says null and the caller checks the name on its own.
 *
 * Memory is bounded: a listing is only kept for a short time, at most [MAX_FOLDERS] of them,
 * and a folder of more than [maxNames] names is not kept at all and not listed again for
 * [OVERSIZE_FACTOR] times the age (a `readdir` of such a folder costs more than it saves, and
 * its names would hold megabytes). The caller drops everything when deliveries stop ([clear]).
 * Pure JVM.
 */
internal class FolderListings(
    private val maxAgeMs: Long,
    private val maxNames: Int,
    private val now: () -> Long,
) {
    /** [names] is null for a folder of more than [maxNames] names. */
    private class Listing(val generation: Long, val takenAt: Long, val names: Set<String>?)

    private val listings = ConcurrentHashMap<String, Listing>()

    /**
     * The case-folded names in [folder] as of a listing taken at [generation] (read by the
     * caller before this call), or null when there is none to rely on. [list] gives the
     * names, empty for a folder that does not exist, null when it cannot be read.
     */
    fun names(folder: String, generation: Long, list: () -> Collection<String>?): Set<String>? {
        val key = CaseFold.key(folder)
        fresh(key, generation)?.let { return it.value }
        return synchronized(this) {
            // Another thread may have listed while this one waited.
            fresh(key, generation)?.let { return it.value }
            val time = now()
            val names = list() ?: return null
            val kept = if (names.size > maxNames) null else names.mapTo(HashSet()) { CaseFold.key(it) }
            evictOld(time)
            listings[key] = Listing(generation, time, kept)
            kept
        }
    }

    /** Forgets every listing. */
    fun clear() {
        listings.clear()
    }

    /** Number of listings kept. */
    val size: Int get() = listings.size

    private class Answer(val value: Set<String>?)

    /** An answer that needs no new listing, or null when one is due. */
    private fun fresh(key: String, generation: Long): Answer? {
        val cached = listings[key] ?: return null
        val age = now() - cached.takenAt
        return when {
            age < 0 -> null
            cached.names == null -> if (age < maxAgeMs * OVERSIZE_FACTOR) Answer(null) else null
            age >= maxAgeMs -> null
            else -> Answer(cached.names.takeIf { cached.generation == generation })
        }
    }

    private fun evictOld(time: Long) {
        listings.entries.removeIf { (_, l) ->
            val age = time - l.takenAt
            l.names != null && (age < 0 || age >= maxAgeMs)
        }
        if (listings.size >= MAX_FOLDERS) listings.clear()
    }

    private companion object {
        const val MAX_FOLDERS = 16
        const val OVERSIZE_FACTOR = 30
    }
}

/**
 * The case-fold key of a name or path, the core's rule (`fold` in
 * `crates/playdrop-core/src/transfer/manifest.rs`): upper then lower case, which also folds
 * `ß` and `SS` together. Stricter than shared storage needs, never looser. Pure JVM.
 */
internal object CaseFold {
    fun key(s: String): String = s.uppercase(java.util.Locale.ROOT).lowercase(java.util.Locale.ROOT)
}

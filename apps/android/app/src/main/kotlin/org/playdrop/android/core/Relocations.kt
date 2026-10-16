package org.playdrop.android.core

import java.io.File
import java.io.FileOutputStream
import java.io.RandomAccessFile
import java.util.concurrent.ConcurrentHashMap

/**
 * Where a landed file really ended up when it did not get the name it was promised.
 *
 * Why this exists: when a landed file lands, the core is handed a `content://` location for
 * the place in `Download/PlayDrop` the file is promised. The core stores that string in the
 * transfer's `delivered` list and never learns of a later change. If the mover then finds the
 * promised name taken (another app, or the user, made a file of that name meanwhile) it has
 * to give the file another name, and the stored location points at nothing or at somebody
 * else's file. Pointing the core at the new place would need a core API or an app-owned
 * `ContentProvider` that resolves a stable ID; this table is the small alternative: the
 * mover writes one line per renamed file, and every place that shows or opens a delivered
 * location asks [resolve] first. Unknown locations fall through unchanged.
 *
 * Key: the transfer ID plus the promised location string exactly as the core stores it in
 * `DeliveredFile.location`. The manifest path (`DeliveredFile.path`) is not usable as the key
 * because the promised name is a cleaned and possibly numbered form of it that the mover
 * cannot map back; the location string is unique per file and is what the readers hold.
 *
 * File format (internal storage, UTF-8, one entry per line, each line ends with a newline):
 * `<transfer id> TAB <promised location> TAB <actual location>`. Entries are appended, the
 * latest line for a key wins, and a last line without its newline (a kill mid-write) is
 * ignored. [prune] rewrites the whole file through a temporary file and an atomic rename.
 * Appending keeps a 5,000-file renamed drop at one small write per file instead of
 * rewriting the table 5,000 times; a power cut can lose the last few lines, in which case
 * the old behaviour (a stale location) is all that comes back.
 *
 * Locations never hold a tab or a newline (they are encoded URIs); a line that would is not
 * recorded. Nothing here is logged: locations name files.
 *
 * Thread-safe. Pure `java.io`, unit-tested on the JVM.
 */
internal class Relocations(private val file: File) {
    /** Serialises writers (file I/O and map updates). Readers never take it. */
    private val writeLock = Any()
    private val byTransfer = ConcurrentHashMap<String, ConcurrentHashMap<String, String>>()

    init {
        load()
    }

    /** Remembers that [promised] of [transferId] is now at [actual]. False when it could not be stored. */
    fun record(transferId: String, promised: String, actual: String): Boolean {
        if (!storable(transferId) || !storable(promised) || !storable(actual)) return false
        synchronized(writeLock) {
            if (!append("$transferId\t$promised\t$actual\n")) return false
            byTransfer.getOrPut(transferId) { ConcurrentHashMap() }[promised] = actual
            return true
        }
    }

    /**
     * Forgets the entry of [promised], for a file that after all reached its promised name (a
     * redone move). Without it an old line would keep pointing at a copy that was removed.
     * Returns false when the entry could not be forgotten durably. Costs nothing when there is no entry.
     */
    fun forget(transferId: String, promised: String): Boolean {
        if (lookup(transferId, promised) == null) return true
        synchronized(writeLock) {
            if (!append("$transferId\t$promised\t\n")) return false
            byTransfer[transferId]?.remove(promised)
            return true
        }
    }

    /**
     * What the mover does about one successfully copied file, before it deletes the landed
     * file: a renamed file ([renumbered]) needs its [actual] place recorded, a file with its
     * promised name needs any old entry forgotten. Returns false when that failed; the caller
     * must then keep the landed file and fail the move, since deleting it would leave a stale
     * location that "Open" might follow to somebody else's file.
     */
    fun noteMove(transferId: String, promised: String, actual: String?, renumbered: Boolean): Boolean =
        if (renumbered) actual != null && record(transferId, promised, actual) else forget(transferId, promised)

    /** The recorded place of [promised], or null when the file kept its promised name. */
    fun lookup(transferId: String, promised: String): String? = byTransfer[transferId]?.get(promised)

    /** [lookup], falling back to [promised] itself. */
    fun resolve(transferId: String, promised: String): String = lookup(transferId, promised) ?: promised

    /**
     * Drops the entries of every transfer not in [live] (the IDs `host.transfers()` still
     * lists) and rewrites the file when anything went. Returns how many entries went.
     */
    fun prune(live: Set<String>): Int = synchronized(writeLock) {
        val gone = byTransfer.keys.filter { it !in live }
        if (gone.isEmpty()) return 0
        val dropped = gone.sumOf { byTransfer[it]?.size ?: 0 }
        gone.forEach { byTransfer.remove(it) }
        rewrite()
        dropped
    }

    /** Flushes appended lines to disk; the mover calls it once per pass. */
    fun sync() {
        synchronized(writeLock) {
            try {
                if (file.isFile) FileOutputStream(file, true).use { it.fd.sync() }
            } catch (_: java.io.IOException) {
                // Best effort: the lines are written, only a power cut could lose them.
            }
        }
    }

    /** Number of entries held. */
    val size: Int get() = byTransfer.values.sumOf { it.size }

    /** True when there are no entries. */
    fun isEmpty(): Boolean = byTransfer.values.all { it.isEmpty() }

    /** IDs of the transfers that have entries. */
    fun transferIds(): Set<String> = byTransfer.keys.toSet()

    private fun storable(s: String) = s.isNotEmpty() && '\t' !in s && '\n' !in s

    private fun append(line: String): Boolean = try {
        file.parentFile?.mkdirs()
        FileOutputStream(file, true).use { it.write(line.toByteArray(Charsets.UTF_8)) }
        true
    } catch (_: java.io.IOException) {
        false
    }

    private fun rewrite() {
        val tmp = File(file.path + ".tmp")
        try {
            file.parentFile?.mkdirs()
            FileOutputStream(tmp).use { out ->
                for ((tid, entries) in byTransfer) {
                    for ((promised, actual) in entries) out.write("$tid\t$promised\t$actual\n".toByteArray(Charsets.UTF_8))
                }
                out.fd.sync()
            }
            if (!tmp.renameTo(file)) tmp.delete()
        } catch (_: java.io.IOException) {
            // The old file stays; the same entries are dropped again by the next prune.
            tmp.delete()
        }
    }

    private fun load() {
        if (!file.isFile) return
        val text = runCatching { file.readText(Charsets.UTF_8) }.getOrNull() ?: return
        // Everything after the last newline is a torn write: ignore it and cut it off the file,
        // or the next append would glue a good line onto the fragment.
        val whole = text.substring(0, text.lastIndexOf('\n') + 1)
        if (whole.length != text.length) {
            runCatching { RandomAccessFile(file, "rw").use { it.setLength(whole.toByteArray(Charsets.UTF_8).size.toLong()) } }
        }
        for (line in whole.split('\n')) {
            val f = line.split('\t')
            if (f.size != 3 || f[0].isEmpty() || f[1].isEmpty()) continue
            if (f[2].isEmpty()) {
                byTransfer[f[0]]?.remove(f[1])
            } else {
                byTransfer.getOrPut(f[0]) { ConcurrentHashMap() }[f[1]] = f[2]
            }
        }
    }
}

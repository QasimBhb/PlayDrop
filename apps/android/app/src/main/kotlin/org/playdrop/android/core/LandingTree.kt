package org.playdrop.android.core

import java.io.File
import java.io.FileOutputStream

/**
 * Layout of the landing area: `<landing>/<transfer id>/<dirs>/<name>` mirrors
 * `Download/PlayDrop/<dirs>/<name>`, where `<name>` is already the final name promised in
 * Downloads. Because the tree itself says where every file goes, no plan file is needed and a
 * process that was killed half way leaves nothing a fresh start cannot finish. Pure JVM.
 */
internal object LandingPaths {
    /** One landed file, split into its transfer and its place under `Download/PlayDrop`. */
    data class Landed(val transferId: String, val dirs: List<String>, val name: String) {
        /** Path components under `Download/PlayDrop`. */
        val downloadsRel: List<String> get() = dirs + name
    }

    /** True for a transfer ID as the core makes them: 32 lowercase hex characters. */
    fun isTransferId(s: String): Boolean = s.length == 32 && s.all { it in '0'..'9' || it in 'a'..'f' }

    /** Components of a landed file below the landing root. */
    fun landingRel(transferId: String, dirs: List<String>, name: String): List<String> = listOf(transferId) + dirs + name

    /**
     * Reads the components of a file below the landing root back into a [Landed], or null when
     * they are not something this app wrote: a bad transfer ID, no name, or any component that
     * breaks the receive rules ([FileNames.isCleanName]: separators, `.`, `..`, control or bidi
     * characters, over 200 characters). Such a file is never moved into Downloads (SR-17). The
     * rules are checked directly rather than by cleaning the name again, so every name the sink
     * can produce (including a cut, numbered one) is accepted.
     */
    fun parse(rel: List<String>): Landed? {
        if (rel.size < 2 || !isTransferId(rel[0])) return null
        val rest = rel.drop(1)
        if (!rest.all(FileNames::isCleanName)) return null
        return Landed(rel[0], rest.dropLast(1), rest.last())
    }
}

/** File-system helpers for the landing tree (plain `java.io`, so they run in JVM tests). */
internal object LandingTree {
    /** Every regular file below [dir], in a stable order (by path); empty if [dir] is missing. */
    fun files(dir: File): List<File> {
        if (!dir.isDirectory) return emptyList()
        return dir.walkTopDown().filter { it.isFile }.sortedBy { it.path }.toList()
    }

    /** Components of [file] below [root], or null when it is not below it. */
    fun relative(root: File, file: File): List<String>? {
        val r = root.path.trimEnd('/') + "/"
        if (!file.path.startsWith(r)) return null
        val parts = file.path.substring(r.length).split('/')
        return parts.takeUnless { p -> p.any { it.isEmpty() } }
    }

    /**
     * Removes the empty folders below [dir], deepest first, and [dir] itself when it ends up
     * empty and [keepRoot] is false. A folder that holds anything is kept: `File.delete` on a
     * folder only succeeds when it is empty, so a file landing at the same moment is never
     * removed (the lander recreates a folder that vanished under it). Returns how many went.
     */
    fun pruneEmpty(dir: File, keepRoot: Boolean = false): Int {
        if (!dir.isDirectory) return 0
        var n = 0
        dir.walkBottomUp().filter { it.isDirectory && (it != dir || !keepRoot) }.forEach {
            if (it.list()?.isEmpty() == true && it.delete()) n++
        }
        return n
    }
}

/**
 * One-entry journal of the copy the mover is doing, so a copy cut short by a killed process is
 * redone instead of being left as a truncated file in Downloads beside a later full copy.
 *
 * Order of a copied move: create the target with `O_EXCL` (or insert the MediaStore row), [write]
 * the entry, copy (and publish the row), delete the landed file, [clear]. If the landed file
 * cannot be deleted the entry is kept and the pass stops, so the next pass removes the copy
 * and moves the file again instead of leaving two. On start, [recovery] decides from what is left. A kill between the
 * create and the [write] can leave one empty file in Downloads; writing the entry before the
 * create would instead risk deleting another app's file of that name if the create had failed
 * with `EEXIST`, which is worse. Renamed moves need no journal: `rename(2)` is atomic.
 *
 * The mover is serial, so one entry is enough. Written with `fsync` (internal storage, not the
 * FUSE layer, about a millisecond) so a power cut cannot reorder it after the copy.
 */
internal class MoveJournal(private val file: File) {
    /**
     * One copy in progress: the landed file (absolute path) and its target in Downloads, an
     * absolute path or, on the MediaStore fallback, the `content://` URI of the inserted row.
     */
    data class Entry(val landing: String, val target: String) {
        /** True when [target] is a MediaStore row rather than a path. */
        val targetIsUri: Boolean get() = target.startsWith("content://")
    }

    /** What a fresh start does with an entry it finds. */
    enum class Recovery {
        /** The landed file is still there, so the target is our unfinished copy: delete it, the move redoes it. */
        DELETE_TARGET,

        /** The move finished (or never created its target): just forget the entry. */
        FORGET,
    }

    /** Records [entry], durably. */
    fun write(entry: Entry) {
        file.parentFile?.mkdirs()
        FileOutputStream(file).use { out ->
            out.write("${entry.landing}\n${entry.target}\n".toByteArray(Charsets.UTF_8))
            out.fd.sync()
        }
    }

    /** The recorded entry, or null when there is none or it is unreadable. */
    fun read(): Entry? {
        if (!file.isFile) return null
        val lines = runCatching { file.readText(Charsets.UTF_8) }.getOrNull()?.split('\n') ?: return null
        if (lines.size < 2 || lines[0].isEmpty() || lines[1].isEmpty()) return null
        return Entry(lines[0], lines[1])
    }

    /** Forgets the entry. */
    fun clear() {
        file.delete()
    }

    /**
     * Settles the entry [e] a stopped pass left: when [recovery] says the target is an
     * unfinished copy, [removeTarget] removes it (and logs why when it cannot). Returns false,
     * keeping the entry, when the copy could not be removed; the pass must then wait, since its
     * next copy would overwrite the entry. Otherwise the entry is forgotten.
     */
    fun settle(e: Entry, landingExists: Boolean, targetExists: Boolean, removeTarget: () -> Boolean): Boolean {
        if (recovery(landingExists, targetExists) == Recovery.DELETE_TARGET && !removeTarget()) return false
        clear()
        return true
    }

    companion object {
        /**
         * The rule behind the cleanup before each pass. For a MediaStore row the caller passes
         * [targetExists] true: deleting a row that is gone is harmless.
         */
        fun recovery(landingExists: Boolean, targetExists: Boolean): Recovery =
            if (landingExists && targetExists) Recovery.DELETE_TARGET else Recovery.FORGET
    }
}

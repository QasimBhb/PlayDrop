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
 * Journal of the copies the mover has in flight, so a copy cut short by a killed process is
 * redone instead of being left as a truncated file in Downloads beside a later full copy.
 *
 * One file move ([write]): create the target with `O_EXCL` (or insert the MediaStore row),
 * [write] the entry, copy (and publish the row), delete the landed file, [clear]. A kill
 * between the create and the [write] can leave one empty file in Downloads; writing the entry
 * before the create would instead risk deleting another app's file of that name if the create
 * had failed with `EEXIST`, which is worse.
 *
 * A batch of moves ([append], see [LandedMove.moveBatch]): first one [Entry.forTag] entry per
 * file with a tag made for this move alone, then the MediaStore rows are inserted carrying
 * those tags, then each entry again with its row's `content://` URI, then the copies, the
 * landed deletes, [clear]. A tag entry is safe to write before the insert because recovery
 * only removes rows this app owns that carry that very tag ([CopyRemover.removeTagged]):
 * nothing else, not even an earlier file of the same name, can match. The URI is journaled as
 * well because the media scan that follows a written file replaces its tag.
 *
 * If a landed file cannot be deleted the entries are kept and the pass stops, so the next pass
 * removes the copy and moves the file again instead of leaving two. On start, [recovery]
 * decides per entry from what is left ([settleAll]).
 *
 * File format (internal storage, UTF-8): one `<landing> TAB <target>` line per entry, each
 * ending with a newline; the latest line for a landed file wins, a last line without its
 * newline (a kill mid-write) is ignored. Paths and URIs never hold a tab or a newline (the
 * names pass [FileNames.isCleanName]). The one-entry format of earlier versions
 * (`<landing>\n<target>\n`) is still read, so an update never strands an unfinished copy.
 * Every write ends with `fsync` (internal storage, not the FUSE layer, about a millisecond) so
 * a power cut cannot reorder it after the copies it covers; a batch pays two per 100 files.
 */
internal class MoveJournal(private val file: File) {
    /**
     * One copy in progress: the landed file (absolute path) and its target in Downloads: an
     * absolute path, the `content://` URI of a MediaStore row, ([forTag]) the tag of a batch
     * row with the row's URI once it is known and the size the copy will have, or ([kept]) a
     * row that must be left alone.
     */
    data class Entry(val landing: String, val target: String) {
        /** True when [target] is a MediaStore row rather than a path. */
        val targetIsUri: Boolean get() = target.startsWith("content://")

        /** True when [target] is a batch row's tag ([forTag]); the row may or may not exist. */
        val targetIsTag: Boolean get() = target.startsWith(TAG_PREFIX)

        /** True for a [kept] entry: recovery forgets it and touches nothing. */
        val targetIsKept: Boolean get() = target == KEPT

        private val tagFields: List<String>? get() = if (targetIsTag) target.removePrefix(TAG_PREFIX).split('|') else null

        /** For a [targetIsTag] entry: the tag its row carries. */
        val tag: String? get() = tagFields?.get(0)

        /** For a [targetIsTag] entry: its row's URI, or null when it was not known yet. */
        val rowUri: String? get() = tagFields?.getOrNull(1)?.ifEmpty { null }

        /**
         * For a [targetIsTag] entry: the size of the landed file, so the most the row's file can
         * hold if only this app wrote it. A bigger file is someone else's: recovery leaves it.
         */
        val size: Long? get() = tagFields?.getOrNull(2)?.toLongOrNull()

        companion object {
            /** A batch row's entry: [tag] and [size] before the insert, then with [rowUri] after it. */
            fun forTag(landing: String, tag: String, rowUri: String? = null, size: Long? = null) =
                Entry(landing, TAG_PREFIX + tag + "|" + rowUri.orEmpty() + "|" + (size?.toString() ?: ""))

            /**
             * Replaces the entry of [landing] when its row must not be removed: another writer
             * took the row's place before the copy, so the file there is not this app's.
             */
            fun kept(landing: String) = Entry(landing, KEPT)
        }
    }

    /** What a fresh start does with an entry it finds. */
    enum class Recovery {
        /** The landed file is still there, so the target is our unfinished copy: delete it, the move redoes it. */
        DELETE_TARGET,

        /** The move finished (or never created its target): just forget the entry. */
        FORGET,
    }

    /** Records [entry] as the only one, durably (the one-file move). */
    fun write(entry: Entry) {
        writeLines(listOf(entry), append = false)
    }

    /** Adds [entries], durably, after what is recorded (a batch). A later entry for the same landed file replaces the earlier one. */
    fun append(entries: List<Entry>) {
        if (entries.isNotEmpty()) writeLines(entries, append = true)
    }

    private fun writeLines(entries: List<Entry>, append: Boolean) {
        file.parentFile?.mkdirs()
        val text = entries.joinToString("") { e ->
            require(storable(e.landing) && storable(e.target)) { "a journal entry holds a tab or a newline" }
            "${e.landing}\t${e.target}\n"
        }
        FileOutputStream(file, append).use { out ->
            out.write(text.toByteArray(Charsets.UTF_8))
            out.fd.sync()
        }
    }

    /** Every recorded entry, the latest per landed file, in the order first recorded; empty when there is none. */
    fun readAll(): List<Entry> {
        if (!file.isFile) return emptyList()
        val text = runCatching { file.readText(Charsets.UTF_8) }.getOrNull() ?: return emptyList()
        val whole = text.substring(0, text.lastIndexOf('\n') + 1)
        val lines = whole.split('\n').dropLast(1)
        if (lines.isNotEmpty() && lines.none { '\t' in it }) {
            // The one-entry format of earlier versions: landing, then target, on their own lines.
            if (lines.size < 2 || lines[0].isEmpty() || lines[1].isEmpty()) return emptyList()
            return listOf(Entry(lines[0], lines[1]))
        }
        val latest = LinkedHashMap<String, Entry>()
        for (line in lines) {
            val f = line.split('\t')
            if (f.size != 2 || f[0].isEmpty() || f[1].isEmpty()) continue
            latest[f[0]] = Entry(f[0], f[1])
        }
        return latest.values.toList()
    }

    /** The last recorded entry, or null when there is none or the journal is unreadable. */
    fun read(): Entry? = readAll().lastOrNull()

    /** Forgets every entry. */
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

    /**
     * Settles every recorded entry the same way: [landingExists] and [targetExists] describe
     * each, [removeTarget] removes an unfinished copy. The journal is cleared only when all of
     * them settled; otherwise it is kept whole and false returned, and settling again later is
     * harmless (a removed copy then reads as gone, so its entry is just forgotten).
     */
    fun settleAll(
        landingExists: (Entry) -> Boolean,
        targetExists: (Entry) -> Boolean,
        removeTarget: (Entry) -> Boolean,
    ): Boolean {
        var ok = true
        for (e in readAll()) {
            if (recovery(landingExists(e), targetExists(e)) == Recovery.DELETE_TARGET && !removeTarget(e)) ok = false
        }
        if (ok) clear()
        return ok
    }

    companion object {
        private const val TAG_PREFIX = "tag:"
        private const val KEPT = "kept:"
        private const val TAG_HEAD = "playdrop-move:"
        private val TAG_SHAPE = Regex("playdrop-move:[0-9a-f-]{36}:[0-9]{1,6}")

        /** The tag of the [k]th row of batch [batchId] (a random UUID): unique to that one move. */
        fun tag(batchId: String, k: Int): String = "$TAG_HEAD$batchId:$k"

        /** True for a tag [tag] makes; recovery acts on nothing else. */
        fun isTag(s: String): Boolean = TAG_SHAPE.matches(s)

        private fun storable(s: String) = s.isNotEmpty() && '\t' !in s && '\n' !in s

        /**
         * The rule behind the cleanup before each pass. For a MediaStore row, or a tag whose
         * row may exist, the caller passes [targetExists] true: deleting a row that is gone is harmless.
         */
        fun recovery(landingExists: Boolean, targetExists: Boolean): Recovery =
            if (landingExists && targetExists) Recovery.DELETE_TARGET else Recovery.FORGET
    }
}

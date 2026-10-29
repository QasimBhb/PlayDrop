package org.playdrop.android.core

import java.io.File
import java.util.UUID
import java.util.concurrent.Callable
import java.util.concurrent.ExecutionException
import java.util.concurrent.ExecutorService

/**
 * Where a copied file ended up: [promisedUri] is the location the core stored when the file
 * landed, [actualUri] the real one (null only if the platform gave none), [renumbered] true
 * when the file did not get its promised name.
 */
internal data class Placement(val promisedUri: String, val actualUri: String?, val renumbered: Boolean)

/**
 * The platform half of a move, split off so [LandedMove] runs on the JVM: it copies [file]
 * into `Download/PlayDrop` (direct write or MediaStore, journaled) and reports where it went.
 * It must remove any half-written copy before it throws, and it never deletes [file].
 * [avoidRows] is set for a file a batch gave up because another row holds its place: the
 * name chosen must then also be free of MediaStore rows that have no file yet.
 */
internal fun interface DownloadsPlacer {
    fun place(landed: LandingPaths.Landed, file: File, avoidRows: Boolean): Placement
}

/** One landed file of a batch: where it goes and the landed file itself. */
internal data class BatchItem(val landed: LandingPaths.Landed, val file: File)

/**
 * A row [BatchPlacer.reserve] inserted: [ref] is its `content://` URI, [relativePath], [name]
 * and [tag] are what MediaProvider made of the folder, name and tag asked for (read back after
 * the insert), [shared] is true when the platform half found another row holding the same
 * place (the sink checks places before the insert instead and leaves it false).
 */
internal data class Row(val ref: String, val relativePath: String, val name: String, val tag: String?, val shared: Boolean)

/**
 * When a MediaStore row is removed by unlinking its file through the file system before the
 * row is deleted (see `DownloadsSink`'s remover): only for a published row this app owns whose
 * file ([data]) is exactly its place ([place]). A pending row keeps its file at a hidden
 * `.pending-<expiry>-<name>` path, so its place may be another, finished file of the same name;
 * such a row, or one whose file is elsewhere, is deleted by URI only. Pure JVM.
 */
internal object RowUnlink {
    fun throughFileSystem(pending: Boolean, owner: String?, me: String, data: String?, place: String?): Boolean =
        !pending && owner == me && data != null && place != null && data == place
}

/**
 * Removes what an unfinished copy left in Downloads; each returns false when it is still there.
 * With [maxSize] (the size the copy would have had) a row whose file is bigger is left alone
 * and counts as settled: another writer took its place, and the file there is theirs. A file
 * at most that size is this app's partial copy (or, in the rare case that someone else wrote
 * less, theirs: that is the accepted limit of the check).
 */
internal interface CopyRemover {
    /** The MediaStore row [uri] (gone already counts as removed). */
    fun removeRow(uri: String, maxSize: Long? = null): Boolean

    /** Every row this app owns that carries [tag] ([BatchPlacer.reserve]). */
    fun removeTagged(tag: String, maxSize: Long? = null): Boolean

    /** The file at [path]. */
    fun removeFile(path: String): Boolean
}

/**
 * The platform half of a batch move (MediaStore on the phone), split off like [DownloadsPlacer]
 * so the order of [LandedMove.moveBatch] runs on the JVM.
 *
 * Why batches: on the Galaxy A54 a file created in Downloads costs about 10 ms, nearly all of
 * it MediaProvider adding its row, and MediaProvider serialises those creates (2, 4 or 8
 * threads were no faster). Inserting the rows 100 at a time costs about 2 ms a file, and
 * opening and writing each row's file then runs well on 4 threads: 5 ms a file in all against
 * 10 to 14 ms (`MoverProbe` in `androidTest`).
 */
internal interface BatchPlacer : CopyRemover {
    /** The folder [landed] goes to, as MediaStore writes it: `Download/PlayDrop/<dirs>/`. */
    fun relativePath(landed: LandingPaths.Landed): String

    /** The location promised to [landed] when it landed. */
    fun promisedUri(landed: LandingPaths.Landed): String

    /**
     * The indices of [items] whose place already has a row (any row, written or not, compared
     * without regard to case as emulated storage does). Their files go the one-file way, which
     * then avoids such rows: a row deleted later unlinks its file, so two rows must never share
     * a place.
     */
    fun placesWithRows(items: List<BatchItem>): Set<Int>

    /**
     * Inserts one published, still empty row per item, in order, at [relativePath] under the
     * landed name, carrying [tags] (one per item) so [CopyRemover.removeTagged] finds it, and
     * returns what each row became. Never replaces an existing file. Throws when the batch as
     * a whole failed; rows made before that carry their tags.
     *
     * Visible meanwhile: up to [LandedMove.BATCH] published rows with no file yet show in the
     * Downloads collection for the half second the batch takes to copy, and after a kill until
     * the next pass removes them (the start-up sweep runs one at once).
     */
    fun reserve(items: List<BatchItem>, tags: List<String>): List<Row>

    /**
     * Copies the landed file of [item] into [row], without truncating: when the row's file is
     * not empty (another writer took the place since the insert) it throws
     * [PlaceTakenException] and writes nothing. May run on several threads at once. Throws on
     * any other failure.
     */
    fun fill(item: BatchItem, row: Row)

    /**
     * Removes [row]: a copy that failed ([written] true) or a row that is not where it was
     * asked to be (nothing written to it yet). False when it could not be removed.
     */
    fun discard(row: Row, written: Boolean): Boolean
}

/** [BatchPlacer.fill] found the row's place already holding someone else's file; nothing was written. */
internal class PlaceTakenException(message: String) : java.io.IOException(message)

/**
 * MediaProvider's cleaning of names and folders on insert, mirrored conservatively: a
 * component this says no to is moved the one-file way instead of in a batch, since its row
 * would not land where the file was promised. Confirmed on the A54 (Android 16, 2026-10-28,
 * `MoverProbe.cleaning`): `" * / : < > ? \ |` and control characters become `_`, one trailing
 * `.` of a name is dropped, a name over 255 UTF-8 bytes is cut. Older versions trim spaces,
 * so those go the one-file way too. AOSP's `FileUtils.sanitizeDisplayName` also puts `_`
 * before a leading `.`; the A54 does not, so that is left to the read-back. Whatever this
 * misses is caught by reading every row back after the insert. Pure JVM.
 */
internal object MediaStoreNames {
    private const val FAT_BAD = "\"*/:<>?\\|"

    /**
     * True when MediaProvider keeps [component] (a folder or a name) exactly as it is. A
     * leading `.` passes: the A54 keeps it, a repository's `.git/` would otherwise lose the
     * whole speed-up, and a device that renames it is caught by the read-back.
     */
    fun keeps(component: String): Boolean =
        component.isNotEmpty() &&
            !component.endsWith('.') &&
            !component.startsWith(' ') &&
            !component.endsWith(' ') &&
            component.none { it < ' ' || it == '\u007f' || it in FAT_BAD } &&
            component.toByteArray(Charsets.UTF_8).size <= 250

    /**
     * [s] as a `LIKE ... ESCAPE '\\'` pattern that matches it whatever its case: `\\`, `%`
     * and `_` escaped, every character outside ASCII (where SQLite's `LIKE` is case-sensitive)
     * made the one-character wildcard `_`, one per code point as SQLite counts them.
     */
    fun likePattern(s: String): String = buildString {
        s.codePoints().forEach { cp ->
            when {
                cp == '\\'.code || cp == '%'.code || cp == '_'.code -> append('\\').appendCodePoint(cp)
                cp > 0x7f -> append('_')
                else -> appendCodePoint(cp)
            }
        }
    }
}

/**
 * Settles what a stopped pass or a killed process left in the [MoveJournal], through
 * [remover]: each entry whose landed file is still there has its copy removed (a file, a row,
 * or the rows carrying its tag), so the move redoes it; an entry whose landed file is gone is
 * forgotten. An entry pointing outside [landingRoot] and [receiveRoot] is only forgotten.
 * Pure JVM: `DownloadsSink.recoverJournal` and the JVM tests run the same code.
 */
internal class JournalRecovery(
    private val journal: MoveJournal,
    private val remover: CopyRemover,
    private val landingRoot: String,
    private val receiveRoot: String,
    /** True for a `content://` URI of the MediaStore downloads this app writes to. */
    private val isRowUri: (String) -> Boolean,
    private val landingExists: (String) -> Boolean = { File(it).exists() },
    private val fileExists: (String) -> Boolean = { File(it).exists() },
) {
    /** [settled] false: the journal is kept and the pass must wait. */
    data class Outcome(val settled: Boolean, val removed: Int, val ignored: Int)

    fun settle(): Outcome {
        var removed = 0
        var ignored = 0
        val ok = journal.settleAll(
            landingExists = { e -> inside(e).also { if (!it) ignored++ } && landingExists(e.landing) },
            targetExists = { e -> e.targetIsUri || e.targetIsTag || (!e.targetIsKept && fileExists(e.target)) },
        ) { e -> remove(e).also { if (it) removed++ } }
        return Outcome(ok, removed, ignored)
    }

    private fun inside(e: MoveJournal.Entry): Boolean {
        val targetOk = when {
            e.targetIsKept -> true
            e.targetIsTag -> MoveJournal.isTag(e.tag!!) && e.rowUri.let { it == null || isRowUri(it) }
            e.targetIsUri -> isRowUri(e.target)
            else -> e.target.startsWith(receiveRoot.trimEnd('/') + "/")
        }
        return e.landing.startsWith(landingRoot.trimEnd('/') + "/") && targetOk
    }

    private fun remove(e: MoveJournal.Entry): Boolean = when {
        // Both: the URI finds a row whose tag a media scan already replaced (a row is scanned
        // once its file is written), the tag one whose URI was never journaled. The URI goes
        // first, so a written row is removed through the file system and the redone move does
        // not find its name still "taken".
        e.targetIsTag -> (e.rowUri?.let { remover.removeRow(it, e.size) } ?: true) and remover.removeTagged(e.tag!!, e.size)
        e.targetIsUri -> remover.removeRow(e.target)
        else -> remover.removeFile(e.target)
    }
}

/**
 * The order of one landed-file move, free of Android types so unit tests can drive it:
 * validate the path, copy ([DownloadsPlacer]), record where the file went, delete the landed
 * file, forget the journal. The reasons for each step are on [DownloadsSink.moveLanded],
 * which delegates here. [moveBatch] does the same for many files at once ([BatchPlacer]).
 */
internal class LandedMove(
    private val landingRoot: () -> File?,
    private val downloadsFile: (LandingPaths.Landed) -> File,
    private val placer: DownloadsPlacer,
    private val relocations: Relocations,
    private val journal: MoveJournal,
    private val claims: NameClaims,
    /** Called when [relocations] gained or lost an entry, so the pass flushes the table once. */
    private val onRelocationsChanged: () -> Unit,
    private val info: (String) -> Unit,
    private val warn: (String, Throwable?) -> Unit,
    /** The batch half, or null where batches are not possible (then [moveBatch] moves one file at a time). */
    private val batchPlacer: BatchPlacer? = null,
    /** Runs the copies of a batch side by side; null runs them one after another. */
    private val fillPool: ExecutorService? = null,
    /** Settles the journal ([DownloadsSink.recoverJournal]); used when a batch insert failed half way. */
    private val settleJournal: () -> Boolean = { true },
) {
    /** Set when a batch insert fails: the rest of that pass moves one file at a time ([beginPass] clears it). */
    @Volatile
    private var batchesOff = false

    /** Called by the mover before each pass: a batch insert that failed in an earlier pass is tried again. */
    fun beginPass() {
        batchesOff = false
    }

    fun move(transferId: String, file: File, avoidRows: Boolean = false): MoveResult {
        val root = landingRoot() ?: return MoveResult(MoveOutcome.FAILED)
        val landed = parse(root, transferId, file) ?: return MoveResult(MoveOutcome.FAILED)
        val target = downloadsFile(landed)
        if (!file.exists()) {
            claims.release(target.path, file.path)
            return MoveResult(MoveOutcome.GONE)
        }
        try {
            val placement = placer.place(landed, file, avoidRows)
            return finish(transferId, file, target, placement)
                ?: MoveResult(MoveOutcome.FAILED, stopPass = true)
        } catch (e: Exception) {
            // A journal entry left by a failed copy points at a copy that was already removed
            // (or a pending MediaStore row); settling it before the next pass is harmless.
            warn("moving a landed file into Downloads failed; it stays in the landing folder", e)
            return MoveResult(MoveOutcome.FAILED)
        }
    }

    /**
     * Moves [files] of [transferId] (the mover hands [BATCH] at a time) and returns one result
     * per file handled. When a result has [MoveResult.stopPass] the list stops short: the files
     * not in it stay landed and their journal entries are kept for the next pass.
     *
     * Order, so that a kill at any instant leaves each file landed or in Downloads, never both
     * for good and never lost:
     * 1. check every path ([LandingPaths.parse]); a file whose promised name already exists,
     *    is held by another landed file, or would be cleaned by MediaProvider
     *    ([MediaStoreNames]) goes the one-file way ([move]) after the batch;
     *    a place that already has a MediaStore row ([BatchPlacer.placesWithRows]) too;
     * 2. journal one tag entry per file ([MoveJournal.Entry.forTag]), durably;
     * 3. insert the rows carrying those tags ([BatchPlacer.reserve]), which never replace an
     *    existing file, and read them back;
     * 4. journal each row's URI next to its tag, durably (a written row loses its tag to the
     *    media scan);
     * 5. remove every row that is not exactly where it was asked to be (cleaned, numbered, or
     *    sharing its place with another row); those files go the one-file way;
     * 6. copy every file into its row, side by side ([fillPool]);
     * 7. per file, in order: record that a renamed file's old entry is gone, then delete the
     *    landed file; a failure here stops the pass with the journal kept;
     * 8. forget the journal.
     * A kill before 8 is settled by the next pass ([JournalRecovery]): an entry whose landed
     * file is still there has its rows removed (by tag and by URI) and the file is moved
     * again, an entry whose landed file is gone is just forgotten. A batch row always has its
     * promised name, so no relocation is ever recorded for one.
     */
    fun moveBatch(transferId: String, files: List<File>): List<MoveResult> {
        val batch = batchPlacer
        if (batch == null || batchesOff || files.size < 2) return moveEach(transferId, files)
        val root = landingRoot() ?: return files.map { MoveResult(MoveOutcome.FAILED) }
        val results = arrayOfNulls<MoveResult>(files.size)
        val items = ArrayList<Pair<Int, BatchItem>>()
        val singles = ArrayList<Int>()
        val avoidRows = HashSet<Int>()
        for ((i, f) in files.withIndex()) {
            val landed = parse(root, transferId, f)
            if (landed == null) {
                results[i] = MoveResult(MoveOutcome.FAILED)
                continue
            }
            val target = downloadsFile(landed)
            when {
                !f.exists() -> {
                    claims.release(target.path, f.path)
                    results[i] = MoveResult(MoveOutcome.GONE)
                }
                // MediaProvider would store it under another folder or name than promised.
                !landed.downloadsRel.all(MediaStoreNames::keeps) -> singles += i
                // Taken since it was promised: the one-file way numbers it.
                claims.heldByOther(target.path, f.path) || target.exists() -> singles += i
                else -> items += i to BatchItem(landed, f)
            }
        }
        if (items.isNotEmpty()) {
            // A place that already has a row (one with no file yet is invisible to the checks
            // above) goes the one-file way, which avoids it.
            val held = try {
                batch.placesWithRows(items.map { it.second })
            } catch (e: Exception) {
                warn("could not look up Downloads before a batch; moving one file at a time", e)
                items.indices.toSet()
            }
            if (held.isNotEmpty()) {
                val keep = items.filterIndexed { k, _ -> k !in held }
                items.filterIndexed { k, _ -> k in held }.forEach { (i, _) -> singles += i; avoidRows += i }
                items.clear()
                items.addAll(keep)
            }
        }
        if (items.isNotEmpty()) {
            val stopped = try {
                runBatch(transferId, batch, items, results, singles, avoidRows)
            } catch (e: Exception) {
                // Nothing expected throws here; keep the journal and let the next pass settle it.
                warn("moving a batch of landed files failed; the next pass settles it", e)
                items.firstOrNull { results[it.first] == null }?.let { results[it.first] = MoveResult(MoveOutcome.FAILED, stopPass = true) }
                true
            }
            if (stopped) return results.filterNotNull()
        }
        for (i in singles.sorted()) {
            val r = move(transferId, files[i], avoidRows = i in avoidRows)
            results[i] = r
            if (r.stopPass) break
        }
        return results.filterNotNull()
    }

    /** Steps 2 to 8 of [moveBatch] for [items]; returns true when the pass must stop. */
    private fun runBatch(
        transferId: String,
        batch: BatchPlacer,
        items: List<Pair<Int, BatchItem>>,
        results: Array<MoveResult?>,
        singles: MutableList<Int>,
        avoidRows: MutableSet<Int>,
    ): Boolean {
        val batchId = UUID.randomUUID().toString()
        val tags = items.indices.map { k -> MoveJournal.tag(batchId, k) }
        try {
            journal.append(items.mapIndexed { k, (_, it) -> MoveJournal.Entry.forTag(it.file.path, tags[k], size = it.file.length()) })
        } catch (e: Exception) {
            // Nothing was made in Downloads yet, but the write may have left a line without
            // its newline: stop the pass, so nothing is appended after it until the next pass's
            // recovery (which ignores that line) clears the journal.
            warn("could not write the move journal; the files stay in the landing folder", e)
            results[items.first().first] = MoveResult(MoveOutcome.FAILED, stopPass = true)
            return true
        }
        val rows = try {
            batch.reserve(items.map { it.second }, tags)
        } catch (e: Exception) {
            // Rows made before the failure carry their tags: settle them now, then move these
            // files one at a time for the rest of this pass.
            if (!settleJournal()) {
                warn("inserting a batch into Downloads failed and could not be undone; the next pass settles it", e)
                results[items.first().first] = MoveResult(MoveOutcome.FAILED, stopPass = true)
                return true
            }
            warn("inserting a batch into Downloads failed; moving one file at a time for the rest of this pass", e)
            batchesOff = true
            items.forEach { singles += it.first }
            return false
        }
        check(rows.size == items.size) { "reserve returned ${rows.size} rows for ${items.size} files" }
        try {
            journal.append(items.mapIndexed { k, (_, it) -> MoveJournal.Entry.forTag(it.file.path, tags[k], rows[k].ref, it.file.length()) })
        } catch (e: Exception) {
            // The tags still find every row (none is written yet); keep the entries and stop.
            warn("could not write the move journal; the next pass settles the batch", e)
            results[items.first().first] = MoveResult(MoveOutcome.FAILED, stopPass = true)
            return true
        }
        if (rows.indices.any { rows[it].tag != tags[it] }) {
            // MediaProvider did not keep a tag, so recovery could not find a row without its
            // URI: remove the batch now (by URI, all journaled) and go one file at a time.
            if (!settleJournal()) {
                warn("a batch's tags were not kept and its rows could not be removed; the next pass settles it", null)
                results[items.first().first] = MoveResult(MoveOutcome.FAILED, stopPass = true)
                return true
            }
            warn("Downloads did not keep a batch's tags; moving one file at a time for the rest of this pass", null)
            batchesOff = true
            items.forEach { singles += it.first }
            return false
        }
        val made = ArrayList<Triple<Int, BatchItem, Row>>()
        for ((k, row) in rows.withIndex()) {
            val (i, item) = items[k]
            if (row.relativePath == batch.relativePath(item.landed) && row.name == item.landed.name && !row.shared) {
                made += Triple(i, item, row)
                continue
            }
            // Not where the file was promised (MediaProvider cleaned or numbered it), or a
            // stray row shares its place: remove it at once, before it has a file, and move
            // the file the one-file way, which picks its name under the folder lock.
            if (!batch.discard(row, written = false)) {
                warn("a Downloads entry in the wrong place could not be removed; the next pass removes it", null)
                results[i] = MoveResult(MoveOutcome.FAILED, stopPass = true)
                return true
            }
            if (row.shared) avoidRows += i
            singles += i
        }
        val errors = fillAll(made.map { (_, item, r) -> { batch.fill(item, r) } })
        var stop = false
        for ((k, entry) in made.withIndex()) {
            val (i, item, r) = entry
            if (stop) break
            val err = errors[k]
            if (err is PlaceTakenException) {
                // Someone else's file now sits at the row's place: leave the row and that file
                // alone (a delete would unlink it), tell recovery so, and move this file the
                // one-file way, which numbers it.
                warn("a Downloads entry was taken by another writer before the copy; that file is left alone", null)
                try {
                    journal.append(listOf(MoveJournal.Entry.kept(item.file.path)))
                } catch (e: Exception) {
                    warn("could not write the move journal; the next pass settles the batch", e)
                    results[i] = MoveResult(MoveOutcome.FAILED, stopPass = true)
                    stop = true
                    continue
                }
                singles += i
                avoidRows += i
                continue
            }
            if (err != null) {
                warn("moving a landed file into Downloads failed; it stays in the landing folder", err)
                if (batch.discard(r, written = true)) {
                    results[i] = MoveResult(MoveOutcome.FAILED)
                } else {
                    // The half-written row stays in the journal: no later batch may clear it.
                    warn("a failed copy could not be removed from Downloads; the next pass removes it", null)
                    results[i] = MoveResult(MoveOutcome.FAILED, stopPass = true)
                    stop = true
                }
                continue
            }
            val promised = batch.promisedUri(item.landed)
            val result = finishCopied(transferId, item.file, downloadsFile(item.landed), Placement(promised, promised, renumbered = false))
            if (result == null) {
                results[i] = MoveResult(MoveOutcome.FAILED, stopPass = true)
                stop = true
            } else {
                results[i] = result
            }
        }
        if (!stop) journal.clear()
        return stop
    }

    /**
     * Runs [tasks] on [fillPool] (or one after another) and returns each one's failure, or
     * null. An [Error] (not an [Exception]) is rethrown once every task has ended.
     */
    private fun fillAll(tasks: List<() -> Unit>): List<Throwable?> {
        val pool = fillPool
        val errors: List<Throwable?> = if (pool == null || tasks.size < 2) {
            tasks.map { t -> runCatching { t() }.exceptionOrNull() }
        } else {
            pool.invokeAll(tasks.map { t -> Callable { t() } }).map { f ->
                try {
                    f.get()
                    null
                } catch (e: ExecutionException) {
                    e.cause ?: e
                }
            }
        }
        errors.firstOrNull { it is Error }?.let { throw it }
        return errors
    }

    /** [move] for each of [files], stopping after a [MoveResult.stopPass]. */
    private fun moveEach(transferId: String, files: List<File>): List<MoveResult> {
        val out = ArrayList<MoveResult>(files.size)
        for (f in files) {
            val r = move(transferId, f)
            out += r
            if (r.stopPass) break
        }
        return out
    }

    private fun parse(root: File, transferId: String, file: File): LandingPaths.Landed? {
        val landed = LandingTree.relative(root, file)?.let(LandingPaths::parse)
        if (landed == null || landed.transferId != transferId) {
            warn("a file in the landing folder has a path PlayDrop would not write; left in place", null)
            return null
        }
        return landed
    }

    /**
     * The steps after a file is copied to [placement]: record where it went, delete the
     * landed file. Returns null when the pass must stop with the landed file kept (the journal
     * entry is kept with it), else the result. Does not touch the journal.
     */
    private fun finishCopied(transferId: String, file: File, target: File, placement: Placement): MoveResult? {
        // Before the landed file is deleted: a kill after the delete must not lose where the
        // file went, and a file that got its promised name must drop any older entry (a
        // redone move). If the table cannot be updated the file stays landed and the pass
        // stops (the journal entry is kept, the next pass removes this copy and redoes it):
        // deleting it would leave a stale location that could open someone else's file.
        if (placement.renumbered || !relocations.isEmpty()) {
            if (!relocations.noteMove(transferId, placement.promisedUri, placement.actualUri, placement.renumbered)) {
                warn("could not update the relocation table; the move is redone later", null)
                return null
            }
            onRelocationsChanged()
        }
        // Delete before forgetting the journal. If the delete fails the entry stays and
        // the pass stops, so the next pass removes this copy and moves the file again
        // instead of leaving it twice.
        if (!file.delete() && file.exists()) {
            warn("a copied file could not be removed from the landing folder; the move is redone later", null)
            return null
        }
        claims.release(target.path, file.path)
        if (placement.renumbered) {
            info("transfer ${transferId.take(8)}: a landed file reached Downloads under another name than promised")
        }
        return MoveResult(MoveOutcome.COPIED, placement.renumbered)
    }

    /** [finishCopied] for the one-file way, which then forgets its journal entry. */
    private fun finish(transferId: String, file: File, target: File, placement: Placement): MoveResult? =
        finishCopied(transferId, file, target, placement)?.also { journal.clear() }

    companion object {
        /** Files the mover hands to [moveBatch] at a time: two journal writes and three MediaProvider calls per batch. */
        const val BATCH = 100
    }
}

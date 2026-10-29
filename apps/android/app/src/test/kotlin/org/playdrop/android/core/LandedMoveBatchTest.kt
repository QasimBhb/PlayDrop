package org.playdrop.android.core

import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder
import java.io.File
import java.io.IOException
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.ExecutorService
import java.util.concurrent.Executors
import java.util.concurrent.atomic.AtomicInteger

// The batch half used here is a fake. The real one (MediaStore inserts, the read-back, the
// clash query, URI building) is exercised on a device by LandedMoveDeviceTest.
/**
 * The order of a batch move ([LandedMove.moveBatch]): tag entries, row inserts, URI entries,
 * removal of misplaced rows, copies side by side, landed deletes, journal cleared; and its
 * recovery, which runs the real [JournalRecovery]. [FakeStore] stands in for MediaStore: a
 * row has an owner and a title (the tag), no file until it is filled, a name it cleans like
 * MediaProvider, a name numbered against files on disk, and the write resets the title like
 * the media scan does.
 */
class LandedMoveBatchTest {
    @get:Rule
    val tmp = TemporaryFolder()

    private val tid = "e".repeat(32)
    private lateinit var landingRoot: File
    private lateinit var downloadsRoot: File
    private lateinit var relocationsFile: File
    private lateinit var journal: MoveJournal
    private lateinit var claims: NameClaims
    private lateinit var store: FakeStore
    private lateinit var single: FakeSingle
    private lateinit var pool: ExecutorService
    private val warnings = mutableListOf<String>()

    private class ProcessKilled : Error()

    /**
     * One MediaStore row: its place, where its file really is ([file]: the place, or a hidden
     * `.pending-` file for a pending row), who owns it, its title.
     */
    private class FakeRow(val file: File, val owner: String, @Volatile var title: String?, val place: File = file, val pending: Boolean = false)

    /** Stands in for MediaStore: rows by URI; a row's file exists only once it is filled. */
    private inner class FakeStore : BatchPlacer {
        val rows = ConcurrentHashMap<String, FakeRow>()
        private val next = AtomicInteger(1)

        /** Called with the count of rows inserted so far in this reserve call; may throw. */
        var duringReserve: ((Int) -> Unit)? = null

        /** Called after every row of a reserve call is made, before it returns. */
        var afterReserve: (() -> Unit)? = null

        /** A cleaning rule [MediaStoreNames] does not know, to exercise the read-back. */
        var extraClean: (String) -> String = { it }

        /** Names whose fill throws an IOException after writing half the file. */
        val failFill = mutableSetOf<String>()

        /** Names whose fill "kills the process" after writing half the file (or before writing, with [killBeforeWrite]). */
        val killFill = mutableSetOf<String>()
        var killBeforeWrite = false
        var failDiscard = false

        /** Kills the process inside the first discard of a row nothing was written to. */
        var killDiscard = false

        /** Like a device that does not keep the title asked for. */
        var dropTitle = false

        /** Names whose place another writer fills (with this text) between the insert and the copy. */
        val takePlace = mutableMapOf<String, String>()
        val fills = AtomicInteger()

        fun dir(landed: LandingPaths.Landed) = File(downloadsRoot, landed.dirs.joinToString("/"))

        fun uri(f: File) = "content://test.files/received/" + f.relativeTo(downloadsRoot).path

        /** MediaProvider's cleaning as AOSP documents it: a leading dot gets a `_`, FAT characters become `_`, a trailing dot goes. */
        fun clean(component: String): String {
            var c = component.map { if (it in "\"*/:<>?\\|" || it < ' ') '_' else it }.joinToString("")
            if (c.startsWith('.')) c = "_$c"
            if (c.endsWith('.')) c = c.dropLast(1)
            return extraClean(c)
        }

        override fun relativePath(landed: LandingPaths.Landed) = "Download/PlayDrop/" + landed.dirs.joinToString("") { "$it/" }

        override fun promisedUri(landed: LandingPaths.Landed) = uri(File(dir(landed), landed.name))

        /**
         * Adds a row someone else made: [owner] "other" for another app, "us" for an untagged
         * row of ours; [pending] puts its file at a hidden `.pending-` path beside its place.
         */
        fun addRow(path: String, owner: String, written: String? = null, pending: Boolean = false): String {
            val place = File(downloadsRoot, path)
            val f = if (pending) File(place.parentFile, ".pending-1700000000-${place.name}") else place
            if (written != null) f.apply { parentFile!!.mkdirs() }.writeText(written)
            val ref = "content://media/external/downloads/${next.getAndIncrement()}"
            rows[ref] = FakeRow(f, owner, null, place, pending)
            return ref
        }

        private fun sameName(a: File, b: File) = a.path.equals(b.path, ignoreCase = true)

        override fun placesWithRows(items: List<BatchItem>): Set<Int> =
            items.indices.filter { k -> rows.values.any { sameName(it.place, File(dir(items[k].landed), items[k].landed.name)) } }.toSet()

        override fun reserve(items: List<BatchItem>, tags: List<String>): List<Row> {
            val out = ArrayList<Row>()
            for ((k, item) in items.withIndex()) {
                duringReserve?.invoke(k)
                val dirs = item.landed.dirs.map(::clean)
                val dir = File(downloadsRoot, dirs.joinToString("/")).apply { mkdirs() }
                val name = clean(item.landed.name)
                // Like MediaProvider: numbered against files on disk, not against rows.
                var n = 0
                while (File(dir, FileNames.numberedName(name, n)).exists()) n++
                val f = File(dir, FileNames.numberedName(name, n))
                val ref = "content://media/external/downloads/${next.getAndIncrement()}"
                rows[ref] = FakeRow(f, "us", if (dropTitle) null else tags[k])
                out += Row(ref, "Download/PlayDrop/" + dirs.joinToString("") { "$it/" }, f.name, rows.getValue(ref).title, shared = false)
            }
            afterReserve?.invoke()
            // The clash check: rows per place, whatever the case.
            return out.map { r -> r.copy(shared = rows.values.count { sameName(it.place, rows.getValue(r.ref).place) } > 1) }
        }

        override fun fill(item: BatchItem, row: Row) {
            val r = rows.getValue(row.ref)
            takePlace[item.landed.name]?.let { r.file.writeText(it) }
            // "rw" and a size check, like the sink: someone else's file is never truncated.
            if (r.file.exists() && r.file.length() > 0) throw PlaceTakenException("taken")
            val body = item.file.readBytes()
            // A write is followed by a media scan, which replaces the title.
            r.title = null
            if (killBeforeWrite && item.landed.name in killFill) throw ProcessKilled()
            if (item.landed.name in failFill || item.landed.name in killFill) {
                r.file.writeBytes(body.copyOf(body.size / 2))
                if (item.landed.name in killFill) throw ProcessKilled()
                throw IOException("write failed")
            }
            r.file.writeBytes(body)
            fills.incrementAndGet()
        }

        override fun discard(row: Row, written: Boolean): Boolean {
            if (failDiscard) return false
            if (killDiscard && !written) throw ProcessKilled()
            return removeRow(row.ref)
        }

        /** Like the sink's remover: an unlink at the place only when [RowUnlink] allows it, then the row (whose delete unlinks its own file). */
        override fun removeRow(uri: String, maxSize: Long?): Boolean {
            val r = rows[uri] ?: return true
            if (maxSize != null && r.file.length() > maxSize) return true
            if (RowUnlink.throughFileSystem(r.pending, r.owner, "us", r.file.path, r.place.path)) r.place.delete()
            rows.remove(uri)
            r.file.delete()
            return true
        }

        override fun removeTagged(tag: String, maxSize: Long?): Boolean {
            rows.entries.filter { it.value.owner == "us" && it.value.title == tag }.forEach { removeRow(it.key, maxSize) }
            return true
        }

        override fun removeFile(path: String): Boolean = File(path).let { it.delete() || !it.exists() }
    }

    /** The one-file way: O_EXCL-like numbering, journaled like `placeInDownloads`. */
    private inner class FakeSingle : DownloadsPlacer {
        var places = 0
        val avoided = mutableListOf<String>()

        override fun place(landed: LandingPaths.Landed, file: File, avoidRows: Boolean): Placement {
            if (avoidRows) avoided += landed.name
            val dir = store.dir(landed).apply { mkdirs() }
            fun taken(f: File) = f.exists() || (avoidRows && store.rows.values.any { it.place.path.equals(f.path, ignoreCase = true) })
            var n = 0
            while (taken(File(dir, FileNames.numberedName(landed.name, n)))) n++
            val dest = File(dir, FileNames.numberedName(landed.name, n))
            journal.write(MoveJournal.Entry(file.path, dest.path))
            file.copyTo(dest)
            places++
            return Placement(store.uri(File(dir, landed.name)), store.uri(dest), dest.name != landed.name)
        }
    }

    @Before
    fun setUp() {
        // A fresh base per call: the kill test sets up again for every kill point.
        val base = tmp.newFolder()
        landingRoot = File(base, "landing").apply { mkdirs() }
        downloadsRoot = File(base, "Download").apply { mkdirs() }
        relocationsFile = File(base, "landing-relocations.txt")
        journal = MoveJournal(File(base, "landing-move.journal"))
        claims = NameClaims()
        store = FakeStore()
        single = FakeSingle()
        pool = Executors.newFixedThreadPool(4)
    }

    @After
    fun tearDown() {
        pool.shutdownNow()
    }

    /** The real recovery, through the seam. */
    private fun recover(): Boolean = JournalRecovery(
        journal, store, landingRoot.path, downloadsRoot.path,
        isRowUri = { it.startsWith("content://media/") },
    ).settle().settled

    private fun mover(
        relocations: Relocations = Relocations(relocationsFile),
        onRelocationsChanged: () -> Unit = {},
        settle: () -> Boolean = ::recover,
        batch: BatchPlacer = store,
    ) = LandedMove(
        landingRoot = { landingRoot },
        downloadsFile = { File(store.dir(it), it.name) },
        placer = single,
        relocations = relocations,
        journal = journal,
        claims = claims,
        onRelocationsChanged = onRelocationsChanged,
        info = {},
        warn = { m, _ -> synchronized(warnings) { warnings.add(m) } },
        batchPlacer = batch,
        fillPool = pool,
        settleJournal = settle,
    )

    private fun land(name: String, dirs: String = "docs", body: String = "body of $name"): File {
        val f = File(landingRoot, "$tid/$dirs/$name")
        f.parentFile!!.mkdirs()
        f.writeText(body)
        claims.reserve(File(downloadsRoot, "$dirs/$name").path, f.path)
        return f
    }

    private fun landMany(n: Int): List<File> = (0 until n).map { land("f%03d.bin".format(it)) }

    private fun landedNow(): List<File> = LandingTree.files(File(landingRoot, tid))

    /** Every file under Downloads with its text, by path relative to it. */
    private fun downloadsNow(): Map<String, String> =
        downloadsRoot.walkTopDown().filter { it.isFile }.associate { it.relativeTo(downloadsRoot).path to it.readText() }

    /** Rows of ours with no file: what a stopped batch must never leave behind. */
    private fun strayRows() = store.rows.values.filter { it.owner == "us" && !it.pending && !it.file.exists() }.map { it.file.path }

    /** The end state every scenario must reach: each file once, whole, at [expected] (path to body). */
    private fun assertEnd(expected: Map<String, String>, why: String) {
        assertEquals(why, expected, downloadsNow())
        assertTrue("$why: landed files left ${landedNow()}", landedNow().isEmpty())
        assertEquals("$why: stray rows", emptyList<String>(), strayRows())
        assertNull("$why: journal left", journal.read())
    }

    private fun assertAllMovedOnce(files: List<File>, why: String) =
        assertEnd(files.associate { "docs/${it.name}" to "body of ${it.name}" }, why)

    @Test
    fun aBatchMovesEveryFileUnderItsPromisedNameAndClearsTheJournal() {
        val files = landMany(30)
        val results = mover().moveBatch(tid, files)
        assertEquals(List(30) { MoveResult(MoveOutcome.COPIED) }, results)
        assertAllMovedOnce(files, "plain batch")
        assertEquals(30, store.fills.get())
        assertEquals(0, single.places)
        assertEquals(0, claims.size)
        assertTrue(warnings.isEmpty())
    }

    @Test
    fun theJournalHoldsATagPerFileBeforeTheRowsAndTheRowUrisBeforeTheCopies() {
        val files = landMany(5)
        var beforeReserve: List<MoveJournal.Entry>? = null
        var beforeFill: List<MoveJournal.Entry>? = null
        store.duringReserve = { k -> if (k == 0) beforeReserve = journal.readAll() }
        // Snapshot the journal when the first copy starts.
        val wrapped = object : BatchPlacer by store {
            override fun fill(item: BatchItem, row: Row) {
                synchronized(this) { if (beforeFill == null) beforeFill = journal.readAll() }
                store.fill(item, row)
            }
        }
        assertEquals(5, mover(batch = wrapped).moveBatch(tid, files).count { it.outcome == MoveOutcome.COPIED })
        assertEquals(files.map { it.path }, beforeReserve!!.map { it.landing })
        assertTrue(beforeReserve!!.all { it.targetIsTag && it.rowUri == null && MoveJournal.isTag(it.tag!!) })
        assertEquals("tags are unique", 5, beforeReserve!!.map { it.tag }.toSet().size)
        assertEquals(files.map { it.path }, beforeFill!!.map { it.landing })
        assertTrue(beforeFill!!.all { it.targetIsTag && it.rowUri!!.startsWith("content://media/") })
        assertEquals(beforeReserve!!.map { it.tag }, beforeFill!!.map { it.tag })
        assertNull(journal.read())
    }

    /**
     * A kill at every point of a batch of several files, then the start-up recovery and a full
     * pass: each file ends up in Downloads exactly once, whole, under its promised name, and no
     * row without a file is left.
     */
    @Test
    fun aKillAtAnyStepOfABatchEndsWithEachFileOnceAndNoStrayRows() {
        /** One kill point: [arm] sets the store's hooks; [afterRecorded] kills after that many files reached step 7. */
        class Kill(val name: String, val afterRecorded: Int? = null, val arm: (FakeStore) -> Unit = {})
        val n = 8
        val kills = mutableListOf(
            Kill("before the first insert") { s -> s.duringReserve = { k -> if (k == 0) throw ProcessKilled() } },
            Kill("after every insert, before the URI entries") { s -> s.afterReserve = { throw ProcessKilled() } },
        )
        for (at in listOf(1, 4, 7)) kills += Kill("after $at inserts") { s -> s.duringReserve = { k -> if (k == at) throw ProcessKilled() } }
        for (k in listOf(0, 3, 7)) kills += Kill("during copy $k") { s -> s.killFill += "f%03d.bin".format(k) }
        for (k in listOf(0, 2, 7)) kills += Kill("after file $k is recorded, before its delete", afterRecorded = k)
        kills += Kill("between the URI entries and the first copy") { s -> s.killFill += "f000.bin"; s.killBeforeWrite = true }
        // A cleaning rule only the read-back catches makes f005 misplaced; the kill is inside its discard.
        kills += Kill("while removing a misplaced row") { s -> s.extraClean = { it.replace("f005", "g005") }; s.killDiscard = true }
        for (kill in kills) {
            setUp()
            val files = landMany(n)
            kill.arm(store)
            val relocations = Relocations(relocationsFile)
            // An unrelated entry makes every file of the batch update the table, so the change
            // callback runs between each file's relocation step and its delete.
            if (kill.afterRecorded != null) assertTrue(relocations.record("0".repeat(32), "content://x/promised", "content://x/actual"))
            var calls = 0
            try {
                mover(relocations, onRelocationsChanged = { if (calls++ == kill.afterRecorded) throw ProcessKilled() }).moveBatch(tid, files)
                throw AssertionError("the simulated kill (${kill.name}) should have escaped")
            } catch (_: ProcessKilled) {
            }
            restartAndFinish()
            assertAllMovedOnce(files, "kill ${kill.name}")
            pool.shutdownNow()
        }
    }

    /** The next start: fresh process state, recovery, then the mover's pass, then a second pass that changes nothing. */
    private fun restartAndFinish() {
        store.duringReserve = null
        store.afterReserve = null
        store.killFill.clear()
        store.killDiscard = false
        claims = NameClaims()
        landedNow().forEach { claims.reserve(File(store.dir(LandingPaths.parse(it.relativeTo(landingRoot).path.split('/'))!!), it.name).path, it.path) }
        assertTrue("recovery", recover())
        mover().moveBatch(tid, landedNow())
        mover().moveBatch(tid, landedNow())
    }

    /**
     * The reviewer's case: a file appears at a promised name between the check and the insert,
     * so MediaProvider numbers that row, and the process dies before the URIs are journaled.
     * The tag still finds the numbered row, so nothing stray is left.
     */
    @Test
    fun aNumberedRowAndAKillBeforeTheUriEntriesLeaveNoStrayRow() {
        val files = landMany(5)
        store.duringReserve = { k -> if (k == 3) File(downloadsRoot, "docs/f003.bin").writeText("someone else's") }
        store.afterReserve = { throw ProcessKilled() }
        try {
            mover().moveBatch(tid, files)
            throw AssertionError("the simulated kill should have escaped")
        } catch (_: ProcessKilled) {
        }
        assertTrue(store.rows.values.any { it.file.name == "f003 (1).bin" })
        restartAndFinish()
        val expected = files.associate { "docs/${it.name}" to "body of ${it.name}" }.toMutableMap()
        expected["docs/f003.bin"] = "someone else's"
        expected["docs/f003 (1).bin"] = "body of f003.bin"
        assertEnd(expected, "numbered row and kill")
        val promised = store.uri(File(downloadsRoot, "docs/f003.bin"))
        assertEquals(store.uri(File(downloadsRoot, "docs/f003 (1).bin")), Relocations(relocationsFile).lookup(tid, promised))
    }

    @Test
    fun aRowMediaProviderNumberedIsRemovedAtOnceAndTheFileGoesTheOneFileWay() {
        val files = landMany(5)
        store.duringReserve = { k -> if (k == 3) File(downloadsRoot, "docs/f003.bin").writeText("someone else's") }
        val results = mover().moveBatch(tid, files)
        assertEquals(4, results.count { it == MoveResult(MoveOutcome.COPIED) })
        assertEquals(1, results.count { it == MoveResult(MoveOutcome.COPIED, renumbered = true) })
        assertEquals(1, single.places)
        assertEquals(4, store.fills.get())
        val expected = files.associate { "docs/${it.name}" to "body of ${it.name}" }.toMutableMap()
        expected["docs/f003.bin"] = "someone else's"
        expected["docs/f003 (1).bin"] = "body of f003.bin"
        assertEnd(expected, "numbered row")
    }

    @Test
    fun aRowMediaProviderCleanedIsRemovedAtOnceAndTheFileGoesTheOneFileWay() {
        // A rule MediaStoreNames does not know: only the read-back can catch it.
        store.extraClean = { it.replace('~', '_') }
        val files = listOf(land("a~b.bin"), land("plain.bin"), land("c.bin"))
        val results = mover().moveBatch(tid, files)
        assertEquals(List(3) { MoveResult(MoveOutcome.COPIED) }, results)
        assertEquals(1, single.places)
        assertEnd(files.associate { "docs/${it.name}" to "body of ${it.name}" }, "cleaned row")
    }

    /**
     * `.gitignore` and `.git/HEAD` go in the batch (the A54 keeps leading dots); this fake
     * cleans them the AOSP way, so the read-back sends them the one-file way. `a:b.txt` and
     * `trail.` never enter the batch.
     */
    @Test
    fun dotFilesDotFoldersAndOtherCleanedNamesGoTheOneFileWay() {
        val files = listOf(
            land(".gitignore", dirs = "proj"), land("HEAD", dirs = "proj/.git"), land("a:b.txt", dirs = "proj"),
            land("trail.", dirs = "proj"), land("ok.txt", dirs = "proj"), land("ok2.txt", dirs = "proj"),
        )
        val results = mover().moveBatch(tid, files)
        assertEquals(List(6) { MoveResult(MoveOutcome.COPIED) }, results)
        assertEquals(4, single.places)
        assertEquals(2, store.fills.get())
        assertEnd(files.associate { it.relativeTo(File(landingRoot, tid)).path to "body of ${it.name}" }, "cleaned names")
    }

    @Test
    fun mediaStoreNamesMirrorsTheCleaningSeenOnThePhone() {
        for (ok in listOf("a.txt", "noext", "sp ace.txt", "emoji😀.txt", "a#b.txt", "a[b].txt", "CON", "~tilde", "x".repeat(200), ".gitignore", ".git")) {
            assertTrue(ok, MediaStoreNames.keeps(ok))
        }
        for (bad in listOf("", "a:b", "q?", "s*", "lt<gt>", "pipe|", "quote\"", "back\\slash", "tab\tx", "trail.", "a.txt.", " lead", "trail ", "é".repeat(130))) {
            assertFalse(bad, MediaStoreNames.keeps(bad))
        }
    }

    @Test
    fun aRowThatSharesItsPlaceIsRemovedAndTheOneFileWayAvoidsTheOtherRow() {
        val files = landMany(3)
        // A row of ours with no file at f002's promised place (left by something else).
        store.addRow("docs/f002.bin", owner = "us")
        val results = mover().moveBatch(tid, files)
        assertEquals(2, results.count { it == MoveResult(MoveOutcome.COPIED) })
        assertEquals(1, results.count { it == MoveResult(MoveOutcome.COPIED, renumbered = true) })
        assertEquals(listOf("f002.bin"), single.avoided)
        assertEquals("body of f002.bin", File(downloadsRoot, "docs/f002 (1).bin").readText())
        assertEquals(listOf(File(downloadsRoot, "docs/f002.bin").path), strayRows())
        assertTrue(landedNow().isEmpty())
        assertNull(journal.read())
    }

    @Test
    fun recoveryRemovesOnlyRowsCarryingTheTagNeverAFinishedFileOfTheSameName() {
        val f = land("same.bin")
        // A finished file of ours under the same name (an earlier MediaStore delivery) and
        // another app's row: neither carries the tag.
        val ours = store.addRow("docs/same.bin", owner = "us", written = "earlier file")
        val theirs = store.addRow("docs/same.bin", owner = "other")
        journal.append(listOf(MoveJournal.Entry.forTag(f.path, MoveJournal.tag("0".repeat(8) + "-0000-0000-0000-" + "0".repeat(12), 0))))
        assertTrue(recover())
        assertTrue(store.rows.containsKey(ours))
        assertTrue(store.rows.containsKey(theirs))
        assertEquals("earlier file", File(downloadsRoot, "docs/same.bin").readText())
        assertNull(journal.read())
    }

    @Test
    fun recoveryIgnoresEntriesThatPointOutsideItsFolders() {
        val f = land("x.bin")
        val outside = File(tmp.root, "elsewhere.txt").apply { writeText("keep") }
        journal.append(
            listOf(
                MoveJournal.Entry(f.path, outside.path),
                MoveJournal.Entry(f.path + "2", "content://evil/1"),
                MoveJournal.Entry.forTag(f.path + "3", "not-a-tag"),
            ),
        )
        val outcome = JournalRecovery(journal, store, landingRoot.path, downloadsRoot.path, isRowUri = { it.startsWith("content://media/") }).settle()
        assertTrue(outcome.settled)
        assertEquals(3, outcome.ignored)
        assertEquals(0, outcome.removed)
        assertTrue(outside.exists())
    }

    @Test
    fun aStopAfterEveryDeleteBeforeTheClearIsForgottenWithoutTouchingDownloads() {
        // Nothing runs between the last delete and journal.clear(); build the state such a stop
        // leaves: every landed file gone, every row written, the entries still recorded.
        val files = landMany(4)
        assertEquals(4, mover().moveBatch(tid, files).size)
        val refs = store.rows.keys.toList()
        journal.append(files.mapIndexed { k, f -> MoveJournal.Entry.forTag(f.path, MoveJournal.tag("1".repeat(8) + "-0000-0000-0000-" + "0".repeat(12), k), refs[k]) })
        assertTrue(recover())
        assertAllMovedOnce(files, "stop before clear")
    }

    @Test
    fun oneFailedCopyAmongManyLeavesOnlyThatFileLandedAndTheOthersMoved() {
        val files = landMany(10)
        store.failFill += "f004.bin"
        val results = mover().moveBatch(tid, files)
        assertEquals(10, results.size)
        assertEquals(9, results.count { it == MoveResult(MoveOutcome.COPIED) })
        assertEquals(1, results.count { it == MoveResult(MoveOutcome.FAILED) })
        assertEquals(listOf(files[4]), landedNow())
        assertFalse("the half-written row is removed", File(downloadsRoot, "docs/f004.bin").exists())
        assertEquals(9, store.rows.size)
        assertNull(journal.read())
        assertTrue(claims.heldByOther(File(downloadsRoot, "docs/f004.bin").path, null))
        // The retry moves it under its promised name.
        store.failFill.clear()
        assertEquals(listOf(MoveResult(MoveOutcome.COPIED)), mover().moveBatch(tid, landedNow()))
        assertAllMovedOnce(files, "retry after one failed copy")
    }

    @Test
    fun aFailedCopyThatCannotBeRemovedStopsThePassAndKeepsEveryEntry() {
        val files = landMany(6)
        store.failFill += "f002.bin"
        store.failDiscard = true
        val results = mover().moveBatch(tid, files)
        // f000 and f001 moved; f002 failed and stopped the pass; f003 to f005 not reported.
        assertEquals(
            listOf(MoveResult(MoveOutcome.COPIED), MoveResult(MoveOutcome.COPIED), MoveResult(MoveOutcome.FAILED, stopPass = true)),
            results,
        )
        assertEquals(files.drop(2), landedNow())
        assertEquals(6, journal.readAll().size)
        assertTrue(journal.readAll().all { it.targetIsTag && it.rowUri != null })
        // The tally counts the unreported files as failed, for the retry.
        val tally = MoveTally(files.size)
        results.forEach(tally::record)
        tally.skipRest()
        assertEquals(2, tally.copied)
        assertEquals(4, tally.failed)
        // Next pass: recovery removes the half-written row and the unfinished copies, then all move.
        store.failFill.clear()
        store.failDiscard = false
        assertTrue(recover())
        assertNull(journal.read())
        mover().moveBatch(tid, landedNow())
        assertAllMovedOnce(files, "after an undeletable failed copy")
    }

    @Test
    fun aRelocationEntryThatCannotBeForgottenStopsThePassWithTheRestLanded() {
        val files = landMany(5)
        // An old relocation of f002's promised place: a batch move must forget it before the
        // landed file goes, and a table that refuses writes stops the pass there.
        val f002 = store.uri(File(downloadsRoot, "docs/f002.bin"))
        val seeded = File(relocationsFile.parentFile, "seed.txt").apply { writeText("$tid\t$f002\tcontent://x/old\n") }
        val seededTable = Relocations(seeded)
        // A folder where the table file was makes every later append fail.
        seeded.delete()
        seeded.mkdirs()
        val results = mover(seededTable).moveBatch(tid, files)
        assertEquals(3, results.size)
        assertEquals(MoveResult(MoveOutcome.FAILED, stopPass = true), results.last())
        assertEquals(files.drop(2), landedNow())
        assertEquals(5, journal.readAll().size)
        assertTrue(warnings.any { it.contains("relocation table") })
        // Next pass with a working table: the rest moves and the old entry is dropped.
        assertTrue(recover())
        mover().moveBatch(tid, landedNow())
        assertAllMovedOnce(files, "after a relocation failure")
    }

    @Test
    fun aPromisedNameTakenMeanwhileGoesTheOneFileWayAndIsNumbered() {
        val files = landMany(4)
        File(downloadsRoot, "docs").mkdirs()
        File(downloadsRoot, "docs/f001.bin").writeText("someone else's")
        val results = mover().moveBatch(tid, files)
        assertEquals(3, results.count { it == MoveResult(MoveOutcome.COPIED) })
        assertEquals(1, results.count { it == MoveResult(MoveOutcome.COPIED, renumbered = true) })
        assertEquals(1, single.places)
        assertEquals("someone else's", File(downloadsRoot, "docs/f001.bin").readText())
        assertEquals("body of f001.bin", File(downloadsRoot, "docs/f001 (1).bin").readText())
        assertTrue(landedNow().isEmpty())
        assertNull(journal.read())
    }

    @Test
    fun aFailedInsertIsSettledAndTheRestOfThatPassMovesOneFileAtATime() {
        val files = landMany(6)
        store.duringReserve = { k -> if (k == 2) throw IOException("refused") }
        var settled = 0
        val m = mover(settle = { settled++; recover() })
        val results = m.moveBatch(tid, files)
        assertEquals(1, settled)
        assertEquals(List(6) { MoveResult(MoveOutcome.COPIED) }, results)
        assertEquals(6, single.places)
        // The two rows inserted before the failure were found by their tags and removed.
        assertEquals(emptyList<String>(), strayRows())
        assertTrue(landedNow().isEmpty())
        assertNull(journal.read())
        // The rest of this pass skips MediaStore...
        val more = (6 until 12).map { land("f%03d.bin".format(it)) }
        assertEquals(3, m.moveBatch(tid, more.take(3)).size)
        assertEquals(9, single.places)
        // ...and the next pass tries batches again.
        store.duringReserve = null
        m.beginPass()
        assertEquals(3, m.moveBatch(tid, more.drop(3)).size)
        assertEquals(9, single.places)
        assertEquals(3, store.fills.get())
    }

    @Test
    fun aFailedInsertThatCannotBeSettledStopsThePass() {
        val files = landMany(3)
        store.duringReserve = { k -> if (k == 1) throw IOException("refused") }
        val results = mover(settle = { false }).moveBatch(tid, files)
        assertEquals(listOf(MoveResult(MoveOutcome.FAILED, stopPass = true)), results)
        assertEquals(3, journal.readAll().size)
        assertEquals(files, landedNow())
    }

    @Test
    fun goneAndForeignFilesInABatchAreReportedAndSkipped() {
        val files = landMany(3)
        assertTrue(files[1].delete())
        val foreign = File(landingRoot, "${"d".repeat(32)}/docs/x.bin").apply { parentFile!!.mkdirs(); writeText("x") }
        val results = mover().moveBatch(tid, files + foreign)
        assertEquals(
            listOf(MoveResult(MoveOutcome.COPIED), MoveResult(MoveOutcome.GONE), MoveResult(MoveOutcome.COPIED), MoveResult(MoveOutcome.FAILED)),
            results,
        )
        assertTrue(foreign.exists())
        assertFalse(claims.heldByOther(File(downloadsRoot, "docs/f001.bin").path, null))
    }

    @Test
    fun aSingleFileOrNoBatchHalfUsesTheOneFileWay() {
        val f = land("only.bin")
        assertEquals(listOf(MoveResult(MoveOutcome.COPIED)), mover().moveBatch(tid, listOf(f)))
        assertEquals(1, single.places)
        val files = landMany(3)
        val plain = LandedMove(
            landingRoot = { landingRoot }, downloadsFile = { File(store.dir(it), it.name) }, placer = single,
            relocations = Relocations(relocationsFile), journal = journal, claims = claims, onRelocationsChanged = {},
            info = {}, warn = { _, _ -> },
        )
        assertEquals(3, plain.moveBatch(tid, files).size)
        assertEquals(4, single.places)
        assertTrue(store.rows.isEmpty())
    }

    @Test
    fun filesInSeveralFoldersMoveInOneBatch() {
        val a = land("a.txt", dirs = "x")
        val b = land("b.txt", dirs = "x/y")
        val c = land("c.txt", dirs = "z")
        assertEquals(3, mover().moveBatch(tid, listOf(a, b, c)).count { it.outcome == MoveOutcome.COPIED })
        assertEquals(mapOf("x/a.txt" to "body of a.txt", "x/y/b.txt" to "body of b.txt", "z/c.txt" to "body of c.txt"), downloadsNow())
        assertEquals(0, single.places)
    }

    /**
     * A pending row (a MediaStore delivery) keeps its file at a hidden path; its place is a
     * finished file of the same name, which recovery must not unlink.
     */
    @Test
    fun recoveringAPendingRowNeverUnlinksAFinishedFileAtItsPlace() {
        val f = land("x.bin")
        store.addRow("docs/x.bin", owner = "us", written = "finished file")
        val pending = store.addRow("docs/x.bin", owner = "us", written = "half", pending = true)
        journal.write(MoveJournal.Entry(f.path, pending))
        assertTrue(recover())
        assertEquals("finished file", File(downloadsRoot, "docs/x.bin").readText())
        assertFalse(store.rows.containsKey(pending))
        assertFalse(File(downloadsRoot, "docs/.pending-1700000000-x.bin").exists())
        assertTrue(f.exists())
    }

    @Test
    fun aPlaceThatAlreadyHasARowWhateverItsCaseGoesTheOneFileWay() {
        val files = landMany(3)
        // An unwritten row of ours named F001.BIN: emulated storage treats it as f001.bin.
        store.addRow("docs/F001.BIN", owner = "us")
        val results = mover().moveBatch(tid, files)
        assertEquals(2, results.count { it == MoveResult(MoveOutcome.COPIED) })
        assertEquals(1, results.count { it == MoveResult(MoveOutcome.COPIED, renumbered = true) })
        assertEquals(listOf("f001.bin"), single.avoided)
        assertEquals(2, store.fills.get())
        assertEquals("body of f001.bin", File(downloadsRoot, "docs/f001 (1).bin").readText())
        assertNull(journal.read())
    }

    @Test
    fun aJournalThatCannotBeWrittenStopsThePassAndMakesNothing() {
        val files = landMany(3)
        // A folder where the journal file goes makes every append fail.
        val jf = File(relocationsFile.parentFile, "landing-move.journal")
        jf.delete()
        jf.mkdirs()
        val results = mover().moveBatch(tid, files)
        assertEquals(listOf(MoveResult(MoveOutcome.FAILED, stopPass = true)), results)
        assertEquals(files, landedNow())
        assertTrue(store.rows.isEmpty())
        jf.delete()
        assertTrue(recover())
        mover().moveBatch(tid, landedNow())
        assertAllMovedOnce(files, "after a failed journal write")
    }

    @Test
    fun aDeviceThatDropsTheTagGetsTheBatchRemovedAtOnceAndMovesOneFileAtATime() {
        val files = landMany(4)
        store.dropTitle = true
        val results = mover().moveBatch(tid, files)
        assertEquals(List(4) { MoveResult(MoveOutcome.COPIED) }, results)
        assertEquals(4, single.places)
        assertEquals(0, store.fills.get())
        assertAllMovedOnce(files, "dropped tags")
        assertTrue(warnings.any { it.contains("did not keep") })
    }

    /**
     * Round-3 M1: another writer fills a row's place between the insert and the copy. The copy
     * never truncates their file; the row is left alone (a delete would unlink it), recovery is
     * told so, and the landed file goes the one-file way under a numbered name.
     */
    @Test
    fun aPlaceTakenBeforeTheCopyIsLeftAloneAndTheFileIsNumbered() {
        val files = landMany(3)
        store.takePlace["f001.bin"] = "theirs, and longer than ours"
        val results = mover().moveBatch(tid, files)
        assertEquals(2, results.count { it == MoveResult(MoveOutcome.COPIED) })
        assertEquals(1, results.count { it == MoveResult(MoveOutcome.COPIED, renumbered = true) })
        assertEquals("theirs, and longer than ours", File(downloadsRoot, "docs/f001.bin").readText())
        assertEquals("body of f001.bin", File(downloadsRoot, "docs/f001 (1).bin").readText())
        assertEquals(listOf("f001.bin"), single.avoided)
        assertTrue(landedNow().isEmpty())
        assertNull(journal.read())
    }

    /** The same, with a kill before the one-file move: recovery forgets the kept entry and touches nothing. */
    @Test
    fun aKeptEntryIsForgottenByRecovery() {
        val files = landMany(3)
        store.takePlace["f001.bin"] = "theirs, and longer than ours"
        val wrapped = object : BatchPlacer by store {}
        var calls = 0
        try {
            // The kill comes when f002 records its move: after f001's entry was marked kept.
            assertTrue(Relocations(relocationsFile).record("0".repeat(32), "content://x/p", "content://x/a"))
            mover(onRelocationsChanged = { if (calls++ == 1) throw ProcessKilled() }, batch = wrapped).moveBatch(tid, files)
            throw AssertionError("the simulated kill should have escaped")
        } catch (_: ProcessKilled) {
        }
        assertTrue(journal.readAll().any { it.targetIsKept })
        store.takePlace.clear()
        restartAndFinish()
        assertEquals("theirs, and longer than ours", File(downloadsRoot, "docs/f001.bin").readText())
        assertEquals("body of f001.bin", File(downloadsRoot, "docs/f001 (1).bin").readText())
        assertTrue(landedNow().isEmpty())
        assertNull(journal.read())
    }

    /** Recovery never removes a row whose file is bigger than the copy written to it: that file is someone else's. */
    @Test
    fun recoveryLeavesARowHoldingABiggerFileThanTheCopy() {
        val f = land("y.bin")
        val ref = store.addRow("docs/y.bin", owner = "us", written = "someone else's, longer than the landed file")
        val tag = MoveJournal.tag("2".repeat(8) + "-0000-0000-0000-" + "0".repeat(12), 0)
        journal.append(listOf(MoveJournal.Entry.forTag(f.path, tag, ref, f.length())))
        assertTrue(recover())
        assertTrue(store.rows.containsKey(ref))
        assertEquals("someone else's, longer than the landed file", File(downloadsRoot, "docs/y.bin").readText())
        // A partial copy of ours (at most the landed size) is removed.
        val g = land("z.bin")
        val ref2 = store.addRow("docs/z.bin", owner = "us", written = "body")
        journal.append(listOf(MoveJournal.Entry.forTag(g.path, tag, ref2, g.length())))
        assertTrue(recover())
        assertFalse(store.rows.containsKey(ref2))
        assertFalse(File(downloadsRoot, "docs/z.bin").exists())
    }

    /** [RowUnlink]: true only with every guard satisfied; each one flipped on its own makes it false. */
    @Test
    fun rowUnlinkNeedsEveryGuard() {
        val me = "org.playdrop.android"
        val place = "/storage/emulated/0/Download/PlayDrop/a.bin"
        assertTrue(RowUnlink.throughFileSystem(pending = false, owner = me, me = me, data = place, place = place))
        assertFalse("pending", RowUnlink.throughFileSystem(pending = true, owner = me, me = me, data = place, place = place))
        assertFalse("another owner", RowUnlink.throughFileSystem(pending = false, owner = "other.app", me = me, data = place, place = place))
        assertFalse("no owner", RowUnlink.throughFileSystem(pending = false, owner = null, me = me, data = place, place = place))
        assertFalse("file elsewhere", RowUnlink.throughFileSystem(pending = false, owner = me, me = me, data = "/x/.pending-1-a.bin", place = place))
        assertFalse("no data", RowUnlink.throughFileSystem(pending = false, owner = me, me = me, data = null, place = place))
        assertFalse("no place", RowUnlink.throughFileSystem(pending = false, owner = me, me = me, data = place, place = null))
    }

    @Test
    fun likePatternsEscapeWildcardsAndWidenNonAsciiToOneCharacterEach() {
        assertEquals("a\\_b\\%c\\\\d.txt", MediaStoreNames.likePattern("a_b%c\\d.txt"))
        assertEquals("R_sum_.pdf", MediaStoreNames.likePattern("R\u00e9sum\u00e9.pdf"))
        // An emoji is two UTF-16 chars but one character to SQLite.
        assertEquals("x_.txt", MediaStoreNames.likePattern("x\uD83D\uDE00.txt"))
        assertEquals("Download/PlayDrop/docs/", MediaStoreNames.likePattern("Download/PlayDrop/docs/"))
    }
}

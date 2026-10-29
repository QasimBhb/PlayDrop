package org.playdrop.android.core

import android.content.ContentValues
import android.os.Environment
import android.provider.MediaStore
import android.util.Log
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import java.io.File
import kotlin.random.Random

/**
 * The batch move of the real [DownloadsSink] against the phone's MediaProvider: a 1,000-file
 * move (timed, logged under `LandedMoveDevice`), the recovery of a batch cut short both after
 * its rows were inserted (tag entries) and during its copies (tag and URI entries), names
 * MediaProvider would clean (dotfiles, dot folders), and a taken name. The JVM tests
 * (`LandedMoveBatchTest`) cover the order; this covers the MediaStore half.
 *
 * Uses the app's real journal and landing folder under a made-up transfer ID and writes to
 * `Download/PlayDrop/devtest-<time>/`; everything is removed afterwards. Run with
 * `adb shell am instrument -w -e class org.playdrop.android.core.LandedMoveDeviceTest
 * org.playdrop.android.test/androidx.test.runner.AndroidJUnitRunner` while the app is idle.
 *
 * [leaveAKilledBatch] is skipped unless run with `-e killstate make`: it leaves the state a
 * batch killed after its inserts leaves (landed files, tag entries, empty published rows in
 * `Download/PlayDrop/killcheck/`), for a look at what Downloads shows and for the real app's
 * start-up pass to settle; `-e killstate clean` removes what is left of it.
 */
@RunWith(AndroidJUnit4::class)
class LandedMoveDeviceTest {
    private val ctx = InstrumentationRegistry.getInstrumentation().targetContext
    private val killState = InstrumentationRegistry.getArguments().getString("killstate")
    private val tid = (1..32).map { "0123456789abcdef"[Random.nextInt(16)] }.joinToString("")
    private val folder = "devtest-${System.currentTimeMillis()}"
    private val downloads = File(File(Environment.getExternalStoragePublicDirectory(Environment.DIRECTORY_DOWNLOADS), FileNames.RECEIVE_FOLDER), folder)
    private val rel = "${Environment.DIRECTORY_DOWNLOADS}/${FileNames.RECEIVE_FOLDER}/$folder/"
    private val collection = MediaStore.Downloads.getContentUri(MediaStore.VOLUME_EXTERNAL_PRIMARY)
    private lateinit var sink: DownloadsSink
    private lateinit var journal: MoveJournal

    @Before
    fun setUp() {
        if (killState != null) {
            // The app's own landing area: its start-up pass is what settles this state.
            sink = DownloadsSink(ctx, File(ctx.filesDir, "devtest-staging"), shapeOf = { null })
            journal = MoveJournal(File(ctx.filesDir, "landing-move.journal"))
            return
        }
        // An area of its own: the app's service can start in this process (a boot broadcast)
        // and its mover would sweep files landed in the app's landing folder.
        sink = DownloadsSink(ctx, File(ctx.filesDir, "devtest-staging"), shapeOf = { null }, area = AREA)
        journal = MoveJournal(File(ctx.filesDir, "$AREA-move.journal"))
        journal.clear()
    }

    @After
    fun tearDown() {
        if (killState != null) return
        ctx.getExternalFilesDir(AREA)?.deleteRecursively()
        journal.clear()
        File(ctx.filesDir, "$AREA-relocations.txt").delete()
        ctx.contentResolver.delete(collection, "${MediaStore.MediaColumns.RELATIVE_PATH} LIKE ?", arrayOf("$rel%"))
        downloads.deleteRecursively()
    }

    /**
     * Lands [names] (paths below the test folder, `/` between folders) in the real landing
     * folder of [transfer] under [under], 4 to 64 KiB each; returns path to size.
     */
    private fun land(names: List<String>, transfer: String = tid, under: String = folder): Map<String, Long> {
        val dir = File(sink.landingDir(transfer)!!, under).apply { mkdirs() }
        val rnd = Random(names.size)
        val buf = ByteArray(64 * 1024).also(rnd::nextBytes)
        val sizes = names.associateWith { rnd.nextInt(4 * 1024, 64 * 1024 + 1).toLong() }
        for ((n, len) in sizes) File(dir, n).apply { parentFile!!.mkdirs() }.outputStream().use { it.write(buf, 0, len.toInt()) }
        sink.rebuildClaims()
        return sizes
    }

    private fun tags(n: Int): List<String> {
        val batch = java.util.UUID.randomUUID().toString()
        return (0 until n).map { MoveJournal.tag(batch, it) }
    }

    private fun moveAll(): List<MoveResult> =
        sink.landedFiles(tid).chunked(LandedMove.BATCH).flatMap { sink.moveLandedBatch(tid, it) }

    private fun assertInDownloads(sizes: Map<String, Long>) {
        assertEquals(sizes.keys.sorted(), downloads.walkTopDown().filter { it.isFile }.map { it.relativeTo(downloads).path }.toList().sorted())
        for ((n, len) in sizes) assertEquals(n, len, File(downloads, n).length())
        assertTrue(sink.landedFiles(tid).isEmpty())
        assertTrue(journal.readAll().isEmpty())
    }

    @Test
    fun aThousandFilesMoveInBatchesUnderTheirPromisedNames() {
        val sizes = land((0 until 1000).map { "m%04d.bin".format(it) })
        assertTrue(sink.recoverJournal())
        val t0 = System.nanoTime()
        val results = moveAll()
        val ms = (System.nanoTime() - t0) / 1e6
        Log.i(TAG, "moved ${results.size} files in %.0f ms, %.2f ms a file".format(ms, ms / results.size))
        sink.moveEnded(tid) // logs the time per step under the playdrop tag
        assertEquals(List(1000) { MoveResult(MoveOutcome.COPIED) }, results)
        assertInDownloads(sizes)
    }

    /** A batch stopped after its rows were inserted, before their URIs were journaled. */
    @Test
    fun rowsInsertedBeforeTheirUrisWereJournaledAreRemovedAndTheFilesMovedAgain() {
        val sizes = land(listOf("a.bin", "b.bin", "c.bin"))
        val landed = sink.landedFiles(tid)
        val t = tags(3)
        journal.append(landed.mapIndexed { k, f -> MoveJournal.Entry.forTag(f.path, t[k]) })
        // The second row is numbered by MediaProvider (a file appeared first): the tag still finds it.
        downloads.mkdirs()
        File(downloads, "b.bin").writeText("in the way")
        for (k in 0..1) insertRow(landed[k].name, t[k])
        assertEquals(1, rowsNamed("b (1).bin"))
        File(downloads, "b.bin").delete()
        assertTrue(sink.recoverJournal())
        assertEquals(0, rowsHere())
        assertEquals(List(3) { MoveResult(MoveOutcome.COPIED) }, moveAll())
        assertInDownloads(sizes)
    }

    /** A batch stopped while copying: one row half written, the others empty. */
    @Test
    fun rowsJournaledByUriAreRemovedAndTheFilesMovedAgain() {
        val sizes = land(listOf("a.bin", "b.bin", "c.bin"))
        val landed = sink.landedFiles(tid)
        val t = tags(3)
        journal.append(landed.mapIndexed { k, f -> MoveJournal.Entry.forTag(f.path, t[k]) })
        val uris = landed.mapIndexed { k, f -> insertRow(f.name, t[k]) }
        journal.append(landed.indices.map { MoveJournal.Entry.forTag(landed[it].path, t[it], uris[it].toString()) })
        // Written (so its tag is gone after the scan): only its URI finds it.
        ctx.contentResolver.openOutputStream(uris[0])!!.use { it.write(ByteArray(1000)) }
        assertTrue(sink.recoverJournal())
        assertEquals(0, rowsHere())
        assertEquals(List(3) { MoveResult(MoveOutcome.COPIED) }, moveAll())
        assertInDownloads(sizes)
    }

    /** A name another file already holds in Downloads is numbered, never overwritten. */
    @Test
    fun aTakenNameIsNumberedAndTheOtherFileKept() {
        val sizes = land((0 until 5).map { "t$it.bin" })
        downloads.mkdirs()
        File(downloads, "t2.bin").writeText("someone else's")
        val results = moveAll()
        assertEquals(4, results.count { it == MoveResult(MoveOutcome.COPIED) })
        assertEquals(1, results.count { it == MoveResult(MoveOutcome.COPIED, renumbered = true) })
        assertEquals("someone else's", File(downloads, "t2.bin").readText())
        assertEquals(sizes.getValue("t2.bin"), File(downloads, "t2 (1).bin").length())
    }

    /** Dotfiles and dot folders go in the batch (the A54 keeps leading dots) and land as promised. */
    @Test
    fun dotFilesAndDotFoldersLandUnderTheirOwnNames() {
        // Names with : ? * < > | " \ or a trailing dot are left out: a direct create of those is
        // refused (EPERM) or loses the name on the A54, which is the one-file way's own matter.
        val sizes = land(listOf(".gitignore", ".git/HEAD", ".git/refs/main", "src/a.txt", "src/b.txt", "src/.env"))
        val results = moveAll()
        assertEquals(List(6) { MoveResult(MoveOutcome.COPIED) }, results)
        assertInDownloads(sizes)
        assertEquals(0, rowsNamed("_.gitignore"))
    }

    /** Recovery by tag never removes a finished file of the same name (an earlier delivery of ours). */
    @Test
    fun recoveryLeavesAFinishedFileOfTheSameName() {
        land(listOf("same.bin"))
        val earlier = insertRow("same.bin", null)
        ctx.contentResolver.openOutputStream(earlier)!!.use { it.write("earlier".toByteArray()) }
        Thread.sleep(500) // let the scan after the write settle
        val landed = sink.landedFiles(tid).single()
        journal.append(listOf(MoveJournal.Entry.forTag(landed.path, tags(1).single())))
        assertTrue(sink.recoverJournal())
        assertEquals("earlier", File(downloads, "same.bin").readText())
        assertEquals(1, rowsNamed("same.bin"))
    }

    /**
     * A pending row (a MediaStore delivery) keeps its file at a hidden `.pending-` path; its
     * place is a finished file of the same name. Recovering the pending row by URI must remove
     * only the pending row and never unlink the finished file.
     */
    @Test
    fun recoveringAPendingRowLeavesTheFinishedFileOfThatName() {
        land(listOf("x.bin"))
        val finished = insertRow("x.bin", null)
        ctx.contentResolver.openOutputStream(finished)!!.use { it.write("finished".toByteArray()) }
        val pending = ctx.contentResolver.insert(
            collection,
            ContentValues().apply {
                put(MediaStore.Downloads.DISPLAY_NAME, "x.bin")
                put(MediaStore.Downloads.MIME_TYPE, "application/octet-stream")
                put(MediaStore.Downloads.RELATIVE_PATH, rel)
                put(MediaStore.Downloads.IS_PENDING, 1)
            },
        )!!
        ctx.contentResolver.openOutputStream(pending)!!.use { it.write("half".toByteArray()) }
        journal.write(MoveJournal.Entry(sink.landedFiles(tid).single().path, pending.toString()))
        assertTrue(sink.recoverJournal())
        assertEquals("finished", File(downloads, "x.bin").readText())
        val bundle = android.os.Bundle().apply { putInt(MediaStore.QUERY_ARG_MATCH_PENDING, MediaStore.MATCH_INCLUDE) }
        assertEquals(0, ctx.contentResolver.query(pending, arrayOf(MediaStore.MediaColumns._ID), bundle, null)?.use { it.count } ?: 0)
        // Only the finished file is left: the pending row's hidden file went with it.
        assertEquals(listOf("x.bin"), downloads.list()!!.sorted())
    }

    /** An unwritten row at a promised place, in another case, sends that file the one-file way, which avoids the name. */
    @Test
    fun aRowAtAPromisedPlaceInAnotherCaseIsAvoided() {
        // Also a non-ASCII case variant (LIKE alone would miss it) and a name with LIKE wildcards.
        val sizes = land((0 until 4).map { "p$it.bin" } + listOf("r\u00e9sum\u00e9.bin", "a_b%c.bin"))
        insertRow("P2.BIN", null)
        insertRow("R\u00c9SUM\u00c9.BIN", null)
        val results = moveAll()
        assertEquals(4, results.count { it == MoveResult(MoveOutcome.COPIED) })
        assertEquals(2, results.count { it == MoveResult(MoveOutcome.COPIED, renumbered = true) })
        assertEquals(sizes.getValue("p2.bin"), File(downloads, "p2 (1).bin").length())
        assertEquals(sizes.getValue("r\u00e9sum\u00e9.bin"), File(downloads, "r\u00e9sum\u00e9 (1).bin").length())
        for (n in listOf("p0.bin", "p1.bin", "p3.bin", "a_b%c.bin")) assertEquals(sizes.getValue(n), File(downloads, n).length())
    }

    /** See the class comment: leaves (`make`) or removes (`clean`) the state of a batch killed after its inserts. */
    @Test
    fun leaveAKilledBatch() {
        org.junit.Assume.assumeTrue("run with -e killstate make|clean", killState == "make" || killState == "clean")
        val killTid = "c0ffee" + "0".repeat(26)
        val killRel = "${Environment.DIRECTORY_DOWNLOADS}/${FileNames.RECEIVE_FOLDER}/killcheck/"
        if (killState == "clean") {
            sink.landingDir(killTid)?.deleteRecursively()
            val n = ctx.contentResolver.delete(collection, "${MediaStore.MediaColumns.RELATIVE_PATH} = ?", arrayOf(killRel))
            File(downloads.parentFile, "killcheck").deleteRecursively()
            Log.i(TAG, "killcheck: removed $n rows")
            return
        }
        assertTrue("an earlier move left a journal", journal.readAll().isEmpty())
        land((0 until 5).map { "k$it.bin" }, transfer = killTid, under = "killcheck")
        val landed = sink.landedFiles(killTid)
        val t = tags(landed.size)
        journal.append(landed.mapIndexed { k, f -> MoveJournal.Entry.forTag(f.path, t[k]) })
        for ((k, f) in landed.withIndex()) {
            ctx.contentResolver.insert(
                collection,
                ContentValues().apply {
                    put(MediaStore.Downloads.DISPLAY_NAME, f.name)
                    put(MediaStore.Downloads.MIME_TYPE, "application/octet-stream")
                    put(MediaStore.Downloads.RELATIVE_PATH, killRel)
                    put(MediaStore.MediaColumns.TITLE, t[k])
                },
            )
        }
        Log.i(TAG, "killcheck: left ${landed.size} landed files, ${landed.size} tag entries and ${landed.size} empty rows")
    }

    private fun insertRow(name: String, tag: String?) = ctx.contentResolver.insert(
        collection,
        ContentValues().apply {
            put(MediaStore.Downloads.DISPLAY_NAME, name)
            put(MediaStore.Downloads.MIME_TYPE, "application/octet-stream")
            put(MediaStore.Downloads.RELATIVE_PATH, rel)
            if (tag != null) put(MediaStore.MediaColumns.TITLE, tag)
        },
    )!!

    private fun rowsNamed(name: String): Int =
        ctx.contentResolver.query(
            collection, arrayOf(MediaStore.MediaColumns._ID),
            "${MediaStore.MediaColumns.RELATIVE_PATH} = ? AND ${MediaStore.MediaColumns.DISPLAY_NAME} = ?", arrayOf(rel, name), null,
        )?.use { it.count } ?: 0

    private fun rowsHere(): Int =
        ctx.contentResolver.query(collection, arrayOf(MediaStore.MediaColumns._ID), "${MediaStore.MediaColumns.RELATIVE_PATH} = ?", arrayOf(rel), null)
            ?.use { it.count } ?: 0

    private companion object {
        const val TAG = "LandedMoveDevice"
        const val AREA = "devtest-landing"
    }
}

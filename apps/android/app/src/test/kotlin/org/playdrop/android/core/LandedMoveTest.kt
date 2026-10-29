package org.playdrop.android.core

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder
import java.io.File
import java.io.IOException

// The placer used here is a fake. The real one (descriptor close, MediaStore fallback, the
// renumbered rule, URI building) is only exercised on a device, not by these tests.
/**
 * The order of one landed-file move ([LandedMove], behind `DownloadsSink.moveLanded`): copy,
 * record the relocation, delete the landed file. The platform copy is a [FakeDownloads]: a temp
 * folder standing in for `Download/PlayDrop` that numbers a taken name like the real sink does.
 */
class LandedMoveTest {
    @get:Rule
    val tmp = TemporaryFolder()

    private val tid = "c".repeat(32)
    private lateinit var landingRoot: File
    private lateinit var downloads: FakeDownloads
    private lateinit var relocationsFile: File
    private lateinit var journal: MoveJournal
    private lateinit var claims: NameClaims
    private var changed = 0
    private val warnings = mutableListOf<String>()

    /** Stands in for the platform half: copies into a folder and picks "name (n)" on a clash. */
    private inner class FakeDownloads(val root: File) : DownloadsPlacer {
        var failCopy = false
        var killAfterCopy = false

        /** Runs right after the copy, before the placement is returned. */
        var afterCopy: (() -> Unit)? = null
        var copies = 0

        fun target(landed: LandingPaths.Landed) = File(File(root, landed.dirs.joinToString("/")), landed.name)

        fun uri(f: File) = "content://test.files/received/" + f.relativeTo(root).path

        override fun place(landed: LandingPaths.Landed, file: File, avoidRows: Boolean): Placement {
            val dir = File(root, landed.dirs.joinToString("/")).apply { mkdirs() }
            var n = 0
            // A name is also taken while it is promised to another landed file.
            while (File(dir, FileNames.numberedName(landed.name, n)).exists()) n++
            val dest = File(dir, FileNames.numberedName(landed.name, n))
            journal.write(MoveJournal.Entry(file.path, dest.path))
            if (failCopy) {
                // What placeInDownloads does: a half-written file is removed, the journal cleared.
                dest.writeBytes(file.readBytes().copyOf(file.length().toInt() / 2))
                dest.delete()
                journal.clear()
                throw IOException("write failed")
            }
            file.copyTo(dest)
            copies++
            afterCopy?.invoke()
            // A process kill is not an Exception: nothing after the copy runs.
            if (killAfterCopy) throw ProcessKilled()
            return Placement(uri(target(landed)), uri(dest), dest.name != landed.name)
        }
    }

    private class ProcessKilled : Error()

    @Before
    fun setUp() {
        landingRoot = tmp.newFolder("landing")
        downloads = FakeDownloads(tmp.newFolder("Download"))
        relocationsFile = File(tmp.root, "landing-relocations.txt")
        journal = MoveJournal(File(tmp.root, "landing-move.journal"))
        claims = NameClaims()
    }

    private fun mover(relocations: Relocations = Relocations(relocationsFile)) = LandedMove(
        landingRoot = { landingRoot },
        downloadsFile = { downloads.target(it) },
        placer = downloads,
        relocations = relocations,
        journal = journal,
        claims = claims,
        onRelocationsChanged = { changed++ },
        info = {},
        warn = { m, _ -> warnings.add(m) },
    )

    private fun land(name: String = "photo.jpg", body: String = "pixels"): File {
        val f = File(landingRoot, "$tid/docs/$name")
        f.parentFile!!.mkdirs()
        f.writeText(body)
        claims.reserve(downloads.target(LandingPaths.parse(listOf(tid, "docs", name))!!).path, f.path)
        return f
    }

    private fun promisedPath(name: String) = File(downloads.root, "docs/$name").path

    /** Relocation lines on disk, so a write that never happened is visible. */
    private fun relocationLines() = if (relocationsFile.isFile) relocationsFile.readLines() else emptyList()

    @Test
    fun aPlainMoveCopiesUnderThePromisedNameAndDeletesTheLandedFile() {
        val f = land()
        val r = mover().move(tid, f)
        assertEquals(MoveResult(MoveOutcome.COPIED, renumbered = false), r)
        assertEquals("pixels", File(downloads.root, "docs/photo.jpg").readText())
        assertFalse(f.exists())
        assertTrue(relocationLines().isEmpty())
        assertEquals(0, changed)
        assertNull(journal.read())
        assertFalse(claims.heldByOther(promisedPath("photo.jpg"), null))
    }

    @Test
    fun aTakenNameLandsNumberedAndIsRecordedBeforeTheLandedFileGoes() {
        val f = land()
        File(downloads.root, "docs").mkdirs()
        File(downloads.root, "docs/photo.jpg").writeText("someone else's")
        // Snapshot the table when the change callback runs, with the landed file still there.
        var linesWhileLanded: List<String>? = null
        val m = LandedMove(
            landingRoot = { landingRoot },
            downloadsFile = { downloads.target(it) },
            placer = downloads,
            relocations = Relocations(relocationsFile),
            journal = journal,
            claims = claims,
            onRelocationsChanged = { linesWhileLanded = relocationLines().takeIf { f.exists() } },
            info = {},
            warn = { _, _ -> },
        )
        val r = m.move(tid, f)
        assertEquals(MoveResult(MoveOutcome.COPIED, renumbered = true), r)
        assertEquals("someone else's", File(downloads.root, "docs/photo.jpg").readText())
        assertEquals("pixels", File(downloads.root, "docs/photo (1).jpg").readText())
        assertFalse(f.exists())
        val promised = downloads.uri(File(downloads.root, "docs/photo.jpg"))
        val actual = downloads.uri(File(downloads.root, "docs/photo (1).jpg"))
        assertEquals(listOf("$tid\t$promised\t$actual"), relocationLines())
        // The change callback runs after the line is written and before the landed file is deleted.
        assertEquals(relocationLines(), linesWhileLanded)
        assertEquals(actual, Relocations(relocationsFile).lookup(tid, promised))
    }

    @Test
    fun aRelocationThatCannotBeRecordedKeepsTheLandedFileAndStopsThePass() {
        val f = land()
        File(downloads.root, "docs").mkdirs()
        File(downloads.root, "docs/photo.jpg").writeText("someone else's")
        // A directory where the table file should be makes every append fail.
        val blocked = File(tmp.root, "blocked-table").apply { mkdirs() }
        val r = mover(Relocations(blocked)).move(tid, f)
        assertEquals(MoveOutcome.FAILED, r.outcome)
        // Phase 8: the pass stops, the landed file stays and the journal entry is kept so the
        // next pass removes the copy and redoes the move (the file then waits for the retry timer).
        assertTrue(r.stopPass)
        assertTrue(f.exists())
        assertEquals("pixels", f.readText())
        assertNotNull(journal.read())
        assertTrue(claims.heldByOther(promisedPath("photo.jpg"), null))
        assertEquals(0, changed)
        assertTrue(warnings.single().contains("relocation table"))
    }

    @Test
    fun aCopyThatFailsHalfWayLeavesNoRelocationNoDownloadsFileAndKeepsTheLandedFile() {
        val f = land(body = "0123456789")
        downloads.failCopy = true
        val r = mover().move(tid, f)
        // Not a stopPass failure: the next file may still work.
        assertEquals(MoveResult(MoveOutcome.FAILED), r)
        assertTrue(f.exists())
        assertEquals("0123456789", f.readText())
        assertTrue(relocationLines().isEmpty())
        assertEquals(0, changed)
        assertEquals(emptyList<String>(), File(downloads.root, "docs").list()?.toList().orEmpty())
        assertNull(journal.read())
        // The promise stays, so nothing delivered meanwhile takes the name.
        assertTrue(claims.heldByOther(promisedPath("photo.jpg"), null))
    }

    @Test
    fun aStopBetweenTheCopyAndTheDeleteIsSettledAndRedoneWithoutADuplicate() {
        val f = land()
        downloads.killAfterCopy = true
        try {
            mover().move(tid, f)
            throw AssertionError("the simulated kill should have escaped")
        } catch (_: ProcessKilled) {
        }
        // The state a force-stop leaves: copy in Downloads, landed file kept, journal entry kept.
        assertTrue(f.exists())
        assertTrue(File(downloads.root, "docs/photo.jpg").exists())
        val entry = journal.read()!!
        assertEquals(f.path, entry.landing)

        // The next start: the mover settles the journal first, then moves the file again.
        val target = File(entry.target)
        val settled = journal.settle(entry, f.exists(), target.exists()) { target.delete() }
        assertTrue(settled)
        assertNull(journal.read())
        downloads.killAfterCopy = false
        val r = mover(Relocations(relocationsFile)).move(tid, f)

        assertEquals(MoveResult(MoveOutcome.COPIED, renumbered = false), r)
        assertEquals(listOf("photo.jpg"), File(downloads.root, "docs").list()!!.sorted())
        assertEquals("pixels", File(downloads.root, "docs/photo.jpg").readText())
        assertFalse(f.exists())
        assertTrue(relocationLines().isEmpty())
        assertEquals(2, downloads.copies)
    }

    @Test
    fun aSecondPassOverAMovedFileFindsItGone() {
        val f = land()
        assertEquals(MoveOutcome.COPIED, mover().move(tid, f).outcome)
        assertEquals(MoveOutcome.GONE, mover().move(tid, f).outcome)
        assertEquals(1, downloads.copies)
    }

    @Test
    fun aPathPlayDropWouldNotWriteIsLeftInPlace() {
        val f = File(landingRoot, "not-a-transfer-id/x.txt")
        f.parentFile!!.mkdirs()
        f.writeText("x")
        assertEquals(MoveOutcome.FAILED, mover().move(tid, f).outcome)
        assertTrue(f.exists())
        assertEquals(0, downloads.copies)
    }

    @Test
    fun aLandedFileThatCannotBeDeletedFailsTheMoveStopsThePassAndKeepsTheJournal() {
        val f = land()
        val dir = f.parentFile!!
        // A read-only parent stops the delete, but only where the filesystem and user honour it.
        downloads.afterCopy = { dir.setWritable(false) }
        try {
            val r = mover().move(tid, f)
            org.junit.Assume.assumeTrue(
                "this machine ignores read-only folders (running as root?), so a delete cannot be made to fail",
                f.exists(),
            )
            assertEquals(MoveResult(MoveOutcome.FAILED, stopPass = true), r)
            assertEquals("pixels", f.readText())
            assertNotNull(journal.read())
            assertTrue(claims.heldByOther(promisedPath("photo.jpg"), null))
            assertTrue(File(downloads.root, "docs/photo.jpg").exists())
        } finally {
            dir.setWritable(true)
        }
    }

    @Test
    fun aStopAfterTheRelocationIsRecordedIsSettledAndRedoneWithoutADuplicateFile() {
        val f = land()
        File(downloads.root, "docs").mkdirs()
        File(downloads.root, "docs/photo.jpg").writeText("someone else's")
        // The change callback runs after the table is updated and before the delete.
        val killing = LandedMove(
            landingRoot = { landingRoot }, downloadsFile = { downloads.target(it) }, placer = downloads,
            relocations = Relocations(relocationsFile), journal = journal, claims = claims,
            onRelocationsChanged = { throw ProcessKilled() }, info = {}, warn = { _, _ -> },
        )
        try {
            killing.move(tid, f)
            throw AssertionError("the simulated kill should have escaped")
        } catch (_: ProcessKilled) {
        }
        assertEquals(1, relocationLines().size)
        assertTrue(f.exists())
        val entry = journal.read()!!
        assertTrue(File(entry.target).exists())

        val target = File(entry.target)
        assertTrue(journal.settle(entry, f.exists(), target.exists()) { target.delete() })
        val r = mover().move(tid, f)

        assertEquals(MoveResult(MoveOutcome.COPIED, renumbered = true), r)
        assertEquals(listOf("photo (1).jpg", "photo.jpg"), File(downloads.root, "docs").list()!!.sorted())
        assertFalse(f.exists())
        val promised = downloads.uri(File(downloads.root, "docs/photo.jpg"))
        val actual = downloads.uri(File(downloads.root, "docs/photo (1).jpg"))
        assertEquals(actual, Relocations(relocationsFile).lookup(tid, promised))
        // Recording appends without checking for an equal line, so the redo adds an identical one
        // (the latest line for a key wins, so it is harmless).
        assertEquals(setOf("$tid\t$promised\t$actual"), relocationLines().toSet())
    }

    @Test
    fun aStopAfterTheDeleteLeavesAnEntryThatRecoveryForgetsWithoutTouchingDownloads() {
        // Nothing runs between the delete and journal.clear(), so no hook can stop there; build the
        // state such a stop leaves: landed file gone, copy in Downloads, entry still written.
        val f = land()
        assertEquals(MoveOutcome.COPIED, mover().move(tid, f).outcome)
        val copy = File(downloads.root, "docs/photo.jpg")
        journal.write(MoveJournal.Entry(f.path, copy.path))
        val entry = journal.read()!!
        assertEquals(MoveJournal.Recovery.FORGET, MoveJournal.recovery(f.exists(), copy.exists()))
        var removed = false
        assertTrue(journal.settle(entry, f.exists(), copy.exists()) { removed = true; true })
        assertFalse(removed)
        assertNull(journal.read())
        assertEquals("pixels", copy.readText())
    }

    @Test
    fun aMoveThatKeepsItsPromisedNameDropsAnOlderRelocationOfThatName() {
        val f = land()
        val promised = downloads.uri(File(downloads.root, "docs/photo.jpg"))
        val old = Relocations(relocationsFile)
        assertTrue(old.record(tid, promised, downloads.uri(File(downloads.root, "docs/photo (1).jpg"))))
        val table = Relocations(relocationsFile)
        assertFalse(table.isEmpty())
        val r = mover(table).move(tid, f)
        assertEquals(MoveResult(MoveOutcome.COPIED, renumbered = false), r)
        assertNull(table.lookup(tid, promised))
        assertNull(Relocations(relocationsFile).lookup(tid, promised))
        assertEquals(1, changed)
        assertFalse(f.exists())
    }

    @Test
    fun aGoneFileReleasesItsStillHeldClaim() {
        val f = land()
        assertTrue(claims.heldByOther(promisedPath("photo.jpg"), null))
        assertTrue(f.delete())
        assertEquals(MoveOutcome.GONE, mover().move(tid, f).outcome)
        assertFalse(claims.heldByOther(promisedPath("photo.jpg"), null))
    }

    @Test
    fun aFileOfAnotherTransferIsLeftInPlace() {
        val other = "d".repeat(32)
        val f = File(landingRoot, "$other/docs/photo.jpg")
        f.parentFile!!.mkdirs()
        f.writeText("x")
        assertEquals(MoveResult(MoveOutcome.FAILED), mover().move(tid, f))
        assertTrue(f.exists())
        assertEquals(0, downloads.copies)
        assertTrue(warnings.single().contains("would not write"))
    }
}

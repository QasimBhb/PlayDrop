package org.playdrop.android.core

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder
import java.io.File

/** The relocation table: records, overlay, reload, torn writes and pruning. */
class RelocationsTest {
    @get:Rule
    val tmp = TemporaryFolder()

    private val tid = "a".repeat(32)
    private val other = "b".repeat(32)
    private val promised = "content://org.playdrop.android.files/received/dir/photo.jpg"
    private val actual = "content://org.playdrop.android.files/received/dir/photo%20(1).jpg"

    private fun table(): Relocations = Relocations(File(tmp.root, "landing-relocations.txt"))

    @Test
    fun aRenumberedMoveIsRecordedBeforeTheLandedFileMayGo() {
        // The decision DownloadsSink.moveLanded takes before it deletes the landed file.
        val tally = MoveTally(1)
        val result = MoveResult(MoveOutcome.COPIED, renumbered = true)
        tally.record(result)
        val t = table()
        assertTrue(t.noteMove(tid, promised, actual, renumbered = true))
        assertEquals(1, tally.renumbered)
        assertEquals(actual, t.lookup(tid, promised))
    }

    @Test
    fun aRenumberedMoveThatCannotBeRecordedMustKeepTheLandedFile() {
        val t = table()
        assertFalse(t.noteMove(tid, promised, null, renumbered = true))
        assertEquals(0, t.size)
        // A table whose file cannot be written (its folder is a regular file).
        val blocker = tmp.newFile("blocker")
        val broken = Relocations(File(blocker, "r.txt"))
        assertFalse(broken.noteMove(tid, promised, actual, renumbered = true))
        assertNull(broken.lookup(tid, promised))
    }

    @Test
    fun aRedoneMoveThatGetsItsPromisedNameForgetsTheOldEntry() {
        val t = table()
        t.record(tid, promised, actual)
        assertTrue(t.noteMove(tid, promised, null, renumbered = false))
        assertNull(t.lookup(tid, promised))
        assertEquals(promised, t.resolve(tid, promised))
        // The tombstone survives a reload.
        assertEquals(promised, table().resolve(tid, promised))
        assertEquals(0, table().size)
    }

    @Test
    fun forgettingNothingWritesNothing() {
        val t = table()
        assertTrue(t.noteMove(tid, promised, null, renumbered = false))
        assertFalse(File(tmp.root, "landing-relocations.txt").exists())
    }

    @Test
    fun theOverlayResolvesARecordedLocation() {
        val t = table()
        t.record(tid, promised, actual)
        assertEquals(actual, t.resolve(tid, promised))
    }

    @Test
    fun unknownKeysFallThroughToTheStoredLocation() {
        val t = table()
        t.record(tid, promised, actual)
        assertEquals(promised, t.resolve(other, promised))
        assertEquals("content://x/other", t.resolve(tid, "content://x/other"))
        assertNull(t.lookup(tid, "content://x/other"))
    }

    @Test
    fun theTableSurvivesAReload() {
        table().apply {
            record(tid, promised, actual)
            record(other, "content://x/1", "content://x/2")
        }
        val again = table()
        assertEquals(actual, again.resolve(tid, promised))
        assertEquals("content://x/2", again.resolve(other, "content://x/1"))
        assertEquals(2, again.size)
    }

    @Test
    fun theLatestLineForAKeyWins() {
        table().apply {
            record(tid, promised, "content://x/first")
            record(tid, promised, actual)
        }
        assertEquals(actual, table().resolve(tid, promised))
    }

    @Test
    fun aTornLastLineIsIgnoredAndCutOffSoTheNextAppendIsNotMerged() {
        val f = File(tmp.root, "landing-relocations.txt")
        f.writeText("$tid\t$promised\t$actual\n$other\tcontent://x/1\tcontent://x/")
        val t = Relocations(f)
        assertEquals(actual, t.resolve(tid, promised))
        assertEquals(1, t.size)
        assertTrue(t.record(other, "content://x/3", "content://x/4"))
        val again = Relocations(f)
        assertEquals(actual, again.resolve(tid, promised))
        assertEquals("content://x/4", again.resolve(other, "content://x/3"))
        assertEquals(2, again.size)
        assertTrue(f.readText().endsWith("\n"))
    }

    @Test
    fun valuesThatWouldBreakALineAreRefused() {
        val t = table()
        assertFalse(t.record(tid, "content://x/a\tb", actual))
        assertFalse(t.record(tid, promised, "content://x/a\nb"))
        assertFalse(t.record(tid, "", actual))
        assertEquals(0, t.size)
    }

    @Test
    fun pruningDropsForgottenTransfersAndKeepsLiveOnes() {
        val t = table()
        t.record(tid, promised, actual)
        t.record(other, "content://x/1", "content://x/2")
        assertEquals(1, t.prune(setOf(tid)))
        assertEquals(setOf(tid), t.transferIds())
        assertEquals(promised, t.resolve(other, promised))
        assertEquals("content://x/1", t.resolve(other, "content://x/1"))

        // The rewrite reached the file.
        val again = table()
        assertEquals(setOf(tid), again.transferIds())
        assertEquals(actual, again.resolve(tid, promised))
        assertEquals(0, again.prune(setOf(tid)))
    }

    @Test
    fun pruningWithNothingLiveEmptiesTheTable() {
        val t = table()
        t.record(tid, promised, actual)
        assertEquals(1, t.prune(emptySet()))
        assertEquals(0, table().size)
        assertFalse(File(tmp.root, "landing-relocations.txt.tmp").exists())
    }

    @Test
    fun recordPruneAndLookupFromTwoThreadsStayConsistent() {
        val t = table()
        val errors = java.util.concurrent.atomic.AtomicReference<Throwable?>(null)
        val writer = Thread {
            try {
                for (i in 0 until 500) assertTrue(t.record(tid, "content://x/$i", "content://y/$i"))
            } catch (e: Throwable) {
                errors.set(e)
            }
        }
        val pruner = Thread {
            try {
                for (i in 0 until 50) t.prune(setOf(tid))
            } catch (e: Throwable) {
                errors.set(e)
            }
        }
        val reader = Thread {
            try {
                for (i in 0 until 2000) {
                    val v = t.resolve(tid, "content://x/${i % 500}")
                    assertTrue(v == "content://x/${i % 500}" || v == "content://y/${i % 500}")
                }
            } catch (e: Throwable) {
                errors.set(e)
            }
        }
        listOf(writer, pruner, reader).forEach { it.start() }
        listOf(writer, pruner, reader).forEach { it.join() }
        errors.get()?.let { throw it }
        assertEquals(500, t.size)
        assertEquals(500, table().size)
    }
}

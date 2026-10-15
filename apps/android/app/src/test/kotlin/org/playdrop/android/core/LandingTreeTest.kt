package org.playdrop.android.core

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder
import java.io.File

/** Landing-to-Downloads path mapping, the tree helpers and the move journal. */
class LandingTreeTest {
    @get:Rule
    val tmp = TemporaryFolder()

    private val tid = "0123456789abcdef0123456789abcdef"

    @Test
    fun landedPathMapsOntoDownloads() {
        val rel = LandingPaths.landingRel(tid, listOf("Photos", "2026"), "a (1).jpg")
        assertEquals(listOf(tid, "Photos", "2026", "a (1).jpg"), rel)
        val landed = LandingPaths.parse(rel)!!
        assertEquals(tid, landed.transferId)
        assertEquals(listOf("Photos", "2026"), landed.dirs)
        assertEquals("a (1).jpg", landed.name)
        assertEquals(listOf("Photos", "2026", "a (1).jpg"), landed.downloadsRel)
        assertEquals(listOf("top.txt"), LandingPaths.parse(listOf(tid, "top.txt"))!!.downloadsRel)
    }

    @Test
    fun foreignOrUnsafePathsAreRejected() {
        assertNull(LandingPaths.parse(listOf(tid)))
        assertNull(LandingPaths.parse(listOf("not-a-transfer", "a")))
        assertNull(LandingPaths.parse(listOf(tid.uppercase(), "a")))
        assertNull(LandingPaths.parse(listOf(tid, "..", "a")))
        assertNull(LandingPaths.parse(listOf(tid, ".")))
        assertNull(LandingPaths.parse(listOf(tid, "x\u0007y")))
        assertNull(LandingPaths.parse(listOf(tid, "a/b")))
        assertNull(LandingPaths.parse(listOf(tid, "a\\b")))
        assertNull(LandingPaths.parse(listOf(tid, "evil\u202Etxt.exe")))
        assertNull(LandingPaths.parse(listOf(tid, "")))
        assertNull(LandingPaths.parse(listOf(tid, "x".repeat(201))))
        assertFalse(LandingPaths.isTransferId(tid + "0"))
        assertTrue(LandingPaths.isTransferId(tid))
    }

    /** Every name the sink can produce is moved, including cut and numbered long names. */
    @Test
    fun everyNameTheSinkMakesIsAccepted() {
        val raw = listOf(
            "x".repeat(199) + " tail", // cut lands right after a space
            "y".repeat(300) + ".jpeg",
            " padded name ",
            "a".repeat(150) + "." + "e".repeat(120),
            ".bashrc",
            "plain.txt",
        )
        for (r in raw) {
            val name = FileNames.sanitize(r)!!
            for (n in listOf(0, 1, 12, 10_000)) {
                val numbered = FileNames.numberedName(name, n)
                assertTrue("$numbered (from $r, n $n)", LandingPaths.parse(listOf(tid, "d", numbered)) != null)
            }
        }
    }

    @Test
    fun relativeSplitsBelowTheRootOnly() {
        val root = tmp.root
        assertEquals(listOf(tid, "d", "f"), LandingTree.relative(root, File(root, "$tid/d/f")))
        assertNull(LandingTree.relative(File(root, "a"), File(root, "ab/f")))
        assertNull(LandingTree.relative(root, root))
    }

    @Test
    fun filesListsEveryFileInStableOrder() {
        val dir = tmp.newFolder(tid)
        File(dir, "b/c").mkdirs()
        File(dir, "b/c/2.txt").writeText("2")
        File(dir, "1.txt").writeText("1")
        File(dir, "empty").mkdirs()
        val rels = LandingTree.files(dir).map { LandingTree.relative(dir, it)!!.joinToString("/") }
        assertEquals(listOf("1.txt", "b/c/2.txt"), rels)
        assertEquals(emptyList<File>(), LandingTree.files(File(tmp.root, "missing")))
    }

    @Test
    fun pruneRemovesOnlyEmptyFolders() {
        val dir = tmp.newFolder(tid)
        File(dir, "a/b").mkdirs()
        File(dir, "keep").mkdirs()
        File(dir, "keep/f").writeText("x")
        assertEquals(2, LandingTree.pruneEmpty(dir))
        assertTrue(File(dir, "keep/f").exists())
        assertFalse(File(dir, "a").exists())
        File(dir, "keep/f").delete()
        LandingTree.pruneEmpty(dir)
        assertFalse(dir.exists())
    }

    @Test
    fun pruneCanKeepTheRoot() {
        val dir = tmp.newFolder(tid)
        File(dir, "a").mkdirs()
        assertEquals(1, LandingTree.pruneEmpty(dir, keepRoot = true))
        assertTrue(dir.isDirectory)
    }

    @Test
    fun journalRoundTrips() {
        val j = MoveJournal(File(tmp.root, "sub/journal"))
        assertNull(j.read())
        j.write(MoveJournal.Entry("/l/$tid/a b.txt", "/d/a b.txt"))
        assertEquals(MoveJournal.Entry("/l/$tid/a b.txt", "/d/a b.txt"), j.read())
        j.write(MoveJournal.Entry("/l/2", "/d/2"))
        assertEquals("/l/2", j.read()!!.landing)
        j.clear()
        assertNull(j.read())
    }

    @Test
    fun brokenJournalIsIgnored() {
        val f = File(tmp.root, "journal")
        f.writeText("only one line")
        assertNull(MoveJournal(f).read())
        f.writeText("\n\n")
        assertNull(MoveJournal(f).read())
    }

    @Test
    fun journalTellsMediaStoreRowsFromPaths() {
        assertTrue(MoveJournal.Entry("/l/a", "content://media/external/downloads/42").targetIsUri)
        assertFalse(MoveJournal.Entry("/l/a", "/storage/emulated/0/Download/PlayDrop/a").targetIsUri)
        val j = MoveJournal(File(tmp.root, "journal"))
        j.write(MoveJournal.Entry("/l/a", "content://media/external/downloads/42"))
        assertEquals("content://media/external/downloads/42", j.read()!!.target)
    }

    @Test
    fun recoveryRemovesOnlyAnUnfinishedCopy() {
        // Landed file still there and a target exists: the copy never finished its job.
        assertEquals(MoveJournal.Recovery.DELETE_TARGET, MoveJournal.recovery(landingExists = true, targetExists = true))
        // Landed file gone: the move finished, the target is the real file.
        assertEquals(MoveJournal.Recovery.FORGET, MoveJournal.recovery(landingExists = false, targetExists = true))
        // Nothing in Downloads yet: just move again.
        assertEquals(MoveJournal.Recovery.FORGET, MoveJournal.recovery(landingExists = true, targetExists = false))
        assertEquals(MoveJournal.Recovery.FORGET, MoveJournal.recovery(landingExists = false, targetExists = false))
    }

    /**
     * A copy whose landed file cannot be deleted keeps its journal entry; the next pass settles
     * it (removes the copy) before moving again, so one file ends up in Downloads, not two.
     */
    @Test
    fun undeletableLandedFileIsNotDuplicated() {
        val landed = File(tmp.root, "landing/$tid/a.txt").apply { parentFile!!.mkdirs(); writeText("payload") }
        val downloads = File(tmp.root, "Download/PlayDrop").apply { mkdirs() }
        val journal = MoveJournal(File(tmp.root, "journal"))
        // Pass 1: copy done, the landed delete "fails", the pass stops with the entry kept.
        val first = File(downloads, "a.txt").apply { writeText("payload") }
        journal.write(MoveJournal.Entry(landed.path, first.path))
        // Pass 2 starts by settling the journal.
        val e = journal.read()!!
        assertEquals(MoveJournal.Recovery.DELETE_TARGET, MoveJournal.recovery(File(e.landing).exists(), File(e.target).exists()))
        assertTrue(File(e.target).delete())
        journal.clear()
        // Then it moves the file again under its promised name.
        File(downloads, "a.txt").writeText(landed.readText())
        assertTrue(landed.delete())
        assertEquals(listOf("a.txt"), downloads.list()!!.toList())
    }

    /**
     * Replays the mover's copy order with a "kill" after every step, then the start-up
     * recovery and a second pass, and checks each run ends with exactly one full copy in
     * Downloads and nothing left landed.
     */
    @Test
    fun killedCopyAtAnyStepEndsWithExactlyOneFile() {
        val steps = 5 // create, journal, copy, delete landed, clear journal
        for (killAfter in 0..steps) {
            val base = tmp.newFolder("run$killAfter")
            val landed = File(base, "landing/$tid/a.txt").apply { parentFile!!.mkdirs(); writeText("payload") }
            val downloads = File(base, "Download/PlayDrop").apply { mkdirs() }
            val journal = MoveJournal(File(base, "journal"))

            fun copyMove(stopAfter: Int) {
                if (!landed.exists()) return
                // Pick the first free name, like the folder-locked claim.
                val name = FileNames.firstFree("a.txt", 10) { File(downloads, it).exists() }!!
                val target = File(downloads, name)
                var step = 0
                target.createNewFile(); if (++step > stopAfter) return
                journal.write(MoveJournal.Entry(landed.path, target.path)); if (++step > stopAfter) return
                target.writeText(landed.readText()); if (++step > stopAfter) return
                landed.delete(); if (++step > stopAfter) return
                journal.clear()
            }

            fun recover() {
                val e = journal.read() ?: return
                if (MoveJournal.recovery(File(e.landing).exists(), File(e.target).exists()) == MoveJournal.Recovery.DELETE_TARGET) {
                    File(e.target).delete()
                }
                journal.clear()
            }

            copyMove(killAfter)
            recover()
            copyMove(Int.MAX_VALUE)
            copyMove(Int.MAX_VALUE) // a second pass over the same folder changes nothing

            val inDownloads = downloads.listFiles()!!.filter { it.length() > 0 }
            assertFalse("landed file left after kill at step $killAfter", landed.exists())
            assertEquals("full copies after kill at step $killAfter", 1, inDownloads.size)
            assertEquals("payload", inDownloads.single().readText())
            // The one accepted leftover: a kill between create and journal leaves an empty file.
            val empties = downloads.listFiles()!!.count { it.length() == 0L }
            assertEquals("empty leftovers after kill at step $killAfter", if (killAfter == 0) 1 else 0, empties)
        }
    }
}

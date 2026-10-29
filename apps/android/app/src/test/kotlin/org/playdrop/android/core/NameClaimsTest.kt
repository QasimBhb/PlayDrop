package org.playdrop.android.core

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import java.util.Collections
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit

/** Promised names and the per-folder lock that every name choice runs under. */
class NameClaimsTest {
    @Test
    fun onlyTheOwnerMayTakeAPromisedName() {
        val c = NameClaims()
        assertTrue(c.reserve("/d/a.txt", "/l/t/a.txt"))
        assertTrue(c.reserve("/d/a.txt", "/l/t/a.txt")) // rebuilding is idempotent
        assertFalse(c.reserve("/d/a.txt", "/l/u/a.txt"))
        assertFalse(c.heldByOther("/d/a.txt", "/l/t/a.txt"))
        assertTrue(c.heldByOther("/d/a.txt", "/l/u/a.txt"))
        assertTrue(c.heldByOther("/d/a.txt", null))
        assertFalse(c.heldByOther("/d/b.txt", null))
    }

    @Test
    fun releaseNeedsTheOwner() {
        val c = NameClaims()
        c.reserve("/d/a.txt", "/l/t/a.txt")
        c.release("/d/a.txt", "/l/u/a.txt")
        assertEquals(1, c.size)
        c.release("/d/a.txt", "/l/t/a.txt")
        assertEquals(0, c.size)
    }

    @Test
    fun firstFreeSkipsTakenAndPromisedNames() {
        val taken = setOf("a.txt", "a (1).txt")
        assertEquals("a (2).txt", FileNames.firstFree("a.txt", 10) { it in taken })
        assertEquals("a.txt", FileNames.firstFree("a.txt", 10) { false })
        assertNull(FileNames.firstFree("a.txt", 2) { true })
    }

    @Test
    fun firstFreeStopsAskingAtTheFirstFreeName() {
        val asked = ArrayList<String>()
        FileNames.firstFree("n", 100) { asked.add(it); asked.size < 3 }
        assertEquals(listOf("n", "n (1)", "n (2)"), asked)
    }

    /** Many threads picking names in one folder under the lock never pick the same one. */
    @Test
    fun folderLockKeepsNamesUnique() {
        val c = NameClaims()
        val existing = Collections.synchronizedSet(HashSet<String>())
        val picked = Collections.synchronizedList(ArrayList<String>())
        val pool = Executors.newFixedThreadPool(8)
        repeat(400) { i ->
            pool.execute {
                c.inFolder("/d") {
                    val name = FileNames.firstFree("f.bin", 1_000) { cand ->
                        c.heldByOther("/d/$cand", null) || cand in existing
                    }!!
                    // Half "land" (promise), half "create" (exist), like the two sink paths.
                    if (i % 2 == 0) c.reserve("/d/$name", "/l/$i") else existing.add(name)
                    picked.add(name)
                }
            }
        }
        pool.shutdown()
        assertTrue(pool.awaitTermination(30, TimeUnit.SECONDS))
        assertEquals(400, picked.size)
        assertEquals(400, picked.toSet().size)
    }

    /** Shared storage is case-insensitive: a case variant of a promised name is held too (the core's fold). */
    @Test
    fun promisesAndLocksIgnoreCase() {
        val c = NameClaims()
        assertTrue(c.reserve("/d/PlayDrop/Report.pdf", "/l/a"))
        assertTrue(c.heldByOther("/d/PlayDrop/report.pdf", null))
        assertTrue(c.heldByOther("/d/playdrop/REPORT.PDF", "/l/b"))
        assertFalse(c.heldByOther("/d/PlayDrop/report.pdf", "/l/a"))
        assertFalse(c.reserve("/d/PlayDrop/REPORT.pdf", "/l/b"))
        c.release("/d/PlayDrop/report.PDF", "/l/a")
        assertFalse(c.heldByOther("/d/PlayDrop/Report.pdf", null))
        assertEquals(CaseFold.key("STRASSE"), CaseFold.key("stra\u00dfe"))
        // One lock for case variants of a folder: a nested call on the other spelling would
        // deadlock with two locks only if they were different objects; here it re-enters.
        assertEquals(1, c.inFolder("/d/A") { c.inFolder("/d/a") { 1 } })
    }
}

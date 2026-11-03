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

    @Test
    fun generationMovesAfterAnythingThatMayCreateAFile() {
        val c = NameClaims()
        assertEquals(0L, c.generation("/d"))
        // A section that only promises a name leaves it alone.
        c.inFolder("/d", changesFiles = false) { c.reserve("/d/a.txt", "/l/t/a.txt") }
        assertEquals(0L, c.generation("/d"))
        // Any other section moves it, after its block, even when the block throws.
        val during = c.inFolder("/D") { c.generation("/d") }
        assertEquals(0L, during)
        assertEquals(1L, c.generation("/d"))
        runCatching { c.inFolder("/d") { throw IllegalStateException("create failed") } }
        assertEquals(2L, c.generation("/d"))
        // Giving a promise back moves its folder's count; a refused release does not.
        c.release("/d/a.txt", "/l/u/a.txt")
        assertEquals(2L, c.generation("/d"))
        c.release("/D/A.txt", "/l/t/a.txt")
        assertEquals(3L, c.generation("/d"))
        assertEquals(0L, c.generation("/e"))
    }

    @Test
    fun aListingServesItsGenerationAndIsRefreshedOnlyWhenOld() {
        var time = 1_000L
        var calls = 0
        var content: Collection<String>? = listOf("Report.PDF")
        val l = FolderListings(2_000L, 10) { time }
        val names = { generation: Long ->
            l.names("/D", generation) {
                calls++
                content
            }
        }
        assertEquals(setOf(CaseFold.key("report.pdf")), names(5L))
        assertEquals(setOf(CaseFold.key("report.pdf")), l.names("/d", 5L) { error("not listed again") })
        assertEquals(1, calls)
        // Another generation inside the age limit: nothing to rely on, and no second readdir.
        content = emptyList()
        assertNull(names(6L))
        assertEquals(1, calls)
        // Old enough: listed again, for the generation asked about.
        time += 2_000L
        assertEquals(emptySet<String>(), names(6L))
        assertEquals(2, calls)
        // A folder that cannot be read gives null and is not remembered.
        time += 2_000L
        content = null
        assertNull(names(6L))
        content = listOf("a")
        assertEquals(setOf("a"), names(6L))
    }

    @Test
    fun listingsStayBounded() {
        var time = 0L
        val l = FolderListings(2_000L, 2) { time }
        // Old listings go when the next one is taken, and no more than a few folders stay.
        for (i in 0 until 100) {
            l.names("/d$i", 1L) { listOf("a") }
            assertTrue(l.size <= 16)
        }
        time += 2_000L
        l.names("/new", 1L) { emptyList() }
        assertEquals(1, l.size)
        // An oversized folder keeps no names and is not listed again for a good while.
        var calls = 0
        val big = { l.names("/big", 1L) { calls++; listOf("a", "b", "c") } }
        assertNull(big())
        assertNull(big())
        time += 2_000L
        assertNull(big())
        assertEquals(1, calls)
        time += 2_000L * 30
        assertNull(big())
        assertEquals(2, calls)
        l.clear()
        assertEquals(0, l.size)
    }

    @Test
    fun oneThreadListsAtATime() {
        var listing = 0
        var most = 0
        val lock = Any()
        val l = FolderListings(2_000L, 10) { 0L }
        val pool = Executors.newFixedThreadPool(4)
        repeat(4) {
            pool.execute {
                l.names("/d", 1L) {
                    synchronized(lock) { most = maxOf(most, ++listing) }
                    Thread.sleep(50)
                    synchronized(lock) { listing-- }
                    listOf("a")
                }
            }
        }
        pool.shutdown()
        assertTrue(pool.awaitTermination(10, TimeUnit.SECONDS))
        assertEquals(1, most)
    }

    @Test
    fun aReleaseMovesTheCountBeforeTheNameIsFree() {
        val c = NameClaims()
        c.reserve("/d/a.txt", "/l/a.txt")
        val before = c.generation("/d")
        c.release("/d/a.txt", "/l/a.txt")
        assertTrue(c.generation("/d") > before)
        assertFalse(c.heldByOther("/d/a.txt", null))
    }
}

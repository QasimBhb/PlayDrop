package org.playdrop.android.service

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** The record of what is mirrored where (SR-42). */
class MirrorBookTest {
    @Test
    fun recordsAndRemoves() {
        val book = MirrorBook()
        assertTrue(book.record("a", "k1", "p", setOf("d1"), 1L).isEmpty())
        assertEquals("a", book.androidKeyFor("d1", "k1"))
        assertNull("another peer cannot dismiss it", book.androidKeyFor("d2", "k1"))
        assertEquals(MirrorBook.Entry("a", "k1", "p", mapOf("d1" to 1L)), book.remove("a"))
        assertNull(book.remove("a"))
    }

    @Test
    fun nothingReachedIsNotRecorded() {
        val book = MirrorBook()
        book.record("a", "k1", "p", emptySet(), 1L)
        assertEquals(0, book.size)
    }

    @Test
    fun aRepostKeepsThePeersItReached() {
        val book = MirrorBook()
        book.record("a", "k1", "p", setOf("d1"), 1L)
        book.record("a", "k1", "p", setOf("d2"), 1L)
        assertEquals(setOf("d1", "d2"), book.remove("a")?.peers)
    }

    @Test
    fun boundedOldestOut() {
        val book = MirrorBook(capacity = 2)
        book.record("a", "k1", "p", setOf("d1"), 1L)
        book.record("b", "k2", "p", setOf("d1"), 1L)
        book.record("a", "k1", "p", setOf("d1"), 1L) // a repost makes it the newest
        val evicted = book.record("c", "k3", "p", setOf("d1"), 1L)
        assertEquals(listOf("b"), evicted.map { it.androidKey })
        assertEquals(2, book.size)
    }

    @Test
    fun dropPeerReturnsItsKeysAndKeepsOthers() {
        val book = MirrorBook()
        book.record("a", "k1", "p", setOf("d1", "d2"), 1L)
        book.record("b", "k2", "p", setOf("d1"), 1L)
        assertEquals(listOf("k1", "k2"), book.dropPeer("d1"))
        assertEquals(setOf("d2"), book.peers())
        assertEquals(1, book.size)
    }

    @Test
    fun removeAppTakesOnlyThatApp() {
        val book = MirrorBook()
        book.record("a", "k1", "chat", setOf("d1"), 1L)
        book.record("b", "k2", "mail", setOf("d1"), 1L)
        assertEquals(listOf("a"), book.removeApp("chat").map { it.androidKey })
        assertEquals(1, book.size)
    }

    @Test
    fun clearReturnsEverything() {
        val book = MirrorBook()
        book.record("a", "k1", "p", setOf("d1"), 1L)
        book.record("b", "k2", "p", setOf("d2"), 1L)
        assertEquals(2, book.clear().size)
        assertEquals(0, book.size)
    }

    @Test
    fun anIdenticalRepostIsUnchangedPerPeer() {
        val book = MirrorBook()
        book.record("a", "k1", "p", setOf("d1"), 7L)
        assertTrue(book.unchanged("a", "d1", 7L))
        assertFalse("new content", book.unchanged("a", "d1", 8L))
        assertFalse("a peer that never got it", book.unchanged("a", "d2", 7L))
        book.record("a", "k1", "p", setOf("d2"), 8L)
        assertTrue("d1 keeps what it got", book.unchanged("a", "d1", 7L))
        assertTrue(book.unchanged("a", "d2", 8L))
        book.dropPeer("d1")
        assertFalse("forgotten after offline", book.unchanged("a", "d1", 7L))
        book.remove("a")
        assertFalse(book.unchanged("a", "d2", 8L))
    }
}

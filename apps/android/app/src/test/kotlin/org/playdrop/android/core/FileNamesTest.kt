package org.playdrop.android.core

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** Name rules for shared URIs and received files. */
class FileNamesTest {
    @Test
    fun displayNameWins() {
        assertEquals("IMG_0001.jpg", FileNames.sendName("IMG_0001.jpg", "1234"))
    }

    @Test
    fun fallsBackToLastPathSegment() {
        assertEquals("report.pdf", FileNames.sendName(null, "docs/report.pdf"))
        assertEquals("report.pdf", FileNames.sendName("  ", "report.pdf"))
    }

    @Test
    fun fallsBackToFile() {
        assertEquals(FileNames.FALLBACK_NAME, FileNames.sendName(null, null))
        assertEquals(FileNames.FALLBACK_NAME, FileNames.sendName("..", "."))
    }

    @Test
    fun separatorsControlsAndBidiAreReplaced() {
        assertEquals("a_b_c", FileNames.sanitize("a/b\\c"))
        assertEquals("x_y", FileNames.sanitize("x\u0000y"))
        assertEquals("evil_gpj.exe", FileNames.sanitize("evil‮gpj.exe"))
    }

    @Test
    fun dotNamesAreRefused() {
        assertNull(FileNames.sanitize("."))
        assertNull(FileNames.sanitize(".."))
        assertNull(FileNames.sanitize(""))
    }

    @Test
    fun longNamesAreCut() {
        assertEquals(200, FileNames.sanitize("a".repeat(500))!!.length)
    }

    @Test
    fun extensions() {
        assertEquals("jpg", FileNames.extension("a.JPG"))
        assertNull(FileNames.extension(".bashrc"))
        assertNull(FileNames.extension("noext"))
        assertNull(FileNames.extension("trailing."))
    }

    @Test
    fun relativePathKeepsManifestFolders() {
        assertEquals("Download/PlayDrop/", FileNames.relativePath("Download", listOf("a.txt")))
        assertEquals("Download/PlayDrop/photos/2026/", FileNames.relativePath("Download", listOf("photos", "2026", "a.jpg")))
    }

    @Test
    fun receivedNameIsLastComponent() {
        assertEquals("a.jpg", FileNames.receivedName(listOf("photos", "a.jpg")))
        assertEquals(FileNames.FALLBACK_NAME, FileNames.receivedName(emptyList()))
    }

    @Test
    fun receivedDirsAreCleanedFolderNames() {
        assertEquals(emptyList<String>(), FileNames.receivedDirs(listOf("a.txt")))
        assertEquals(listOf("photos", "2026"), FileNames.receivedDirs(listOf("photos", "2026", "a.jpg")))
        // Separators inside a component can never add a level or climb out (SR-17).
        assertEquals(listOf("a_b", "c"), FileNames.receivedDirs(listOf("a/b", "..", "c", ".", "x")))
        assertEquals(emptyList<String>(), FileNames.receivedDirs(emptyList()))
    }

    @Test
    fun numberedNamesKeepTheExtension() {
        assertEquals("a.txt", FileNames.numberedName("a.txt", 0))
        assertEquals("a (1).txt", FileNames.numberedName("a.txt", 1))
        assertEquals("a (12).JPG", FileNames.numberedName("a.JPG", 12))
        assertEquals("archive.tar (2).gz", FileNames.numberedName("archive.tar.gz", 2))
        assertEquals("noext (3)", FileNames.numberedName("noext", 3))
        assertEquals(".bashrc (1)", FileNames.numberedName(".bashrc", 1))
        assertEquals("trailing. (1)", FileNames.numberedName("trailing.", 1))
    }

    @Test
    fun riskyTypes() {
        assertTrue(FileNames.isRiskyToOpen("app.apk", null))
        assertTrue(FileNames.isRiskyToOpen("run.SH", null))
        assertTrue(FileNames.isRiskyToOpen("thing", "application/vnd.android.package-archive"))
        assertFalse(FileNames.isRiskyToOpen("photo.jpg", "image/jpeg"))
        assertFalse(FileNames.isRiskyToOpen("notes.txt", null))
    }

    @Test
    fun sanitizeCutsThenTrimsAndIsIdempotent() {
        val cut = FileNames.sanitize("x".repeat(199) + " tail")!!
        assertEquals("x".repeat(199), cut)
        for (raw in listOf("x".repeat(199) + " tail", " a ", "a".repeat(500), "na\u0000me", "ok.txt")) {
            val once = FileNames.sanitize(raw)!!
            assertEquals(once, FileNames.sanitize(once))
            assertTrue(FileNames.isCleanName(once))
        }
    }

    @Test
    fun numberedNamesStayWithinTheLimit() {
        val long = "y".repeat(FileNames.MAX_NAME_CHARS - 5) + ".jpeg"
        assertEquals(FileNames.MAX_NAME_CHARS, long.length)
        val n1 = FileNames.numberedName(long, 1)
        assertTrue(n1.length <= FileNames.MAX_NAME_CHARS)
        assertTrue(n1.endsWith(" (1).jpeg"))
        val n9999 = FileNames.numberedName(long, 9_999)
        assertTrue(n9999.length <= FileNames.MAX_NAME_CHARS)
        assertTrue(FileNames.isCleanName(n9999))
        // A cut never leaves a space before the number.
        val spaced = "z".repeat(195) + " " + "w".repeat(4)
        assertFalse(FileNames.numberedName(spaced, 1).contains("  ("))
        // An extension too long to keep is cut with the rest.
        val hugeExt = "a." + "e".repeat(198)
        val n = FileNames.numberedName(hugeExt, 3)
        assertTrue(n.length <= FileNames.MAX_NAME_CHARS)
        assertTrue(n.endsWith(" (3)"))
        // Short names are unchanged from before.
        assertEquals("a (1).txt", FileNames.numberedName("a.txt", 1))
        assertEquals("a (2)", FileNames.numberedName("a", 2))
    }

    @Test
    fun cleanNameRules() {
        assertTrue(FileNames.isCleanName("a b.txt"))
        assertTrue(FileNames.isCleanName(" leading space is allowed"))
        assertFalse(FileNames.isCleanName(""))
        assertFalse(FileNames.isCleanName("."))
        assertFalse(FileNames.isCleanName(".."))
        assertFalse(FileNames.isCleanName("a/b"))
        assertFalse(FileNames.isCleanName("a\\b"))
        assertFalse(FileNames.isCleanName("a\tb"))
        assertFalse(FileNames.isCleanName("a\u202Eb"))
        assertFalse(FileNames.isCleanName("x".repeat(201)))
        assertTrue(FileNames.isCleanName("x".repeat(200)))
    }
}

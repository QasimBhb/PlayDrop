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
    fun riskyTypes() {
        assertTrue(FileNames.isRiskyToOpen("app.apk", null))
        assertTrue(FileNames.isRiskyToOpen("run.SH", null))
        assertTrue(FileNames.isRiskyToOpen("thing", "application/vnd.android.package-archive"))
        assertFalse(FileNames.isRiskyToOpen("photo.jpg", "image/jpeg"))
        assertFalse(FileNames.isRiskyToOpen("notes.txt", null))
    }
}

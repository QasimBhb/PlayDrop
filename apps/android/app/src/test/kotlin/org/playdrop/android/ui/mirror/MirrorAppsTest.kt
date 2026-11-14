package org.playdrop.android.ui.mirror

import org.junit.Assert.assertEquals
import org.junit.Test
import java.util.Locale

/** The app picker's list. */
class MirrorAppsTest {
    @Test
    fun sortedByNameOncePerPackageWithoutPlayDrop() {
        val found = listOf(
            MirrorApp("org.example.zed", "zed"),
            MirrorApp("org.playdrop.android", "PlayDrop"),
            MirrorApp("org.example.mail", "Mail"),
            MirrorApp("org.example.mail", "Mail (work)"),
            MirrorApp("org.example.chat", "Ápp chat"),
            MirrorApp("org.example.blank", " "),
        )
        val list = MirrorApps.forPicker(found, "org.playdrop.android", Locale.ENGLISH)
        assertEquals(listOf("Ápp chat", "Mail", "org.example.blank", "zed"), list.map { it.label })
    }

    @Test
    fun toggling() {
        assertEquals(setOf("a", "b"), MirrorApps.toggled(setOf("a"), "b", true))
        assertEquals(setOf("a"), MirrorApps.toggled(setOf("a", "b"), "b", false))
    }

    @Test
    fun appsTheQueryMissedAreReportedNotDropped() {
        val allowed = setOf("chat", "gone", "archived")
        // "chat" is listed; the other two are reported for the person to remove, sorted.
        assertEquals(listOf("archived", "gone"), MirrorApps.notFound(allowed, setOf("chat", "maps")))
        assertEquals(emptyList<String>(), MirrorApps.notFound(setOf("chat"), setOf("chat")))
        // A failed query (nothing listed) reports nothing.
        assertEquals(emptyList<String>(), MirrorApps.notFound(allowed, emptySet()))
    }

    private val notFound = { _: String -> MirrorApps.Lookup.NOT_FOUND }

    @Test
    fun onlyAnAppEveryCheckMissesIsPrunable() {
        val allowed = setOf("chat", "gone", "disabled", "archived", "shown", "failed", "visible")
        val lookup = { p: String ->
            when (p) {
                "visible" -> MirrorApps.Lookup.FOUND
                "failed" -> MirrorApps.Lookup.FAILED
                else -> MirrorApps.Lookup.NOT_FOUND
            }
        }
        val gone = MirrorApps.prunable(
            allowed = allowed,
            launcher = setOf("chat", "maps"),
            wider = setOf("chat", "maps", "disabled", "archived"),
            lookup = lookup,
            active = setOf("shown"),
        )
        assertEquals(setOf("gone"), gone)
    }

    @Test
    fun aFailedOrEmptyQueryPrunesNothing() {
        val allowed = setOf("chat", "gone")
        assertEquals(emptySet<String>(), MirrorApps.prunable(allowed, emptySet(), setOf("maps"), notFound, emptySet()))
        assertEquals(emptySet<String>(), MirrorApps.prunable(allowed, setOf("maps"), emptySet(), notFound, emptySet()))
        assertEquals(emptySet<String>(), MirrorApps.prunable(emptySet(), setOf("maps"), setOf("maps"), notFound, emptySet()))
    }
}

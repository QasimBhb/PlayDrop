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
}

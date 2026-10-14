package org.playdrop.android.ui.device

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test
import org.playdrop.android.R
import org.playdrop.android.ui.common.UiMessage

class MediaRefusalTest {
    @Test
    fun forbiddenMediaNamesTheComputerAndTheFix() {
        assertEquals(
            UiMessage(R.string.ui_media_refused, listOf("Office PC")),
            MediaRefusal.message("media", "forbidden", null, "Office PC"),
        )
    }

    @Test
    fun otherMediaRefusalShowsTheDesktopsReason() {
        assertEquals(
            UiMessage(R.string.ui_media_refused_other, listOf("no such player")),
            MediaRefusal.message("media", "not_found", "no such player", "Office PC"),
        )
        assertEquals(
            UiMessage(R.string.ui_media_refused_other, listOf("busy")),
            MediaRefusal.message("media", "busy", null, "Office PC"),
        )
    }

    @Test
    fun otherFrameTypesAreIgnored() {
        assertNull(MediaRefusal.message("input", "forbidden", null, "Office PC"))
        assertNull(MediaRefusal.message(null, "forbidden", null, "Office PC"))
    }
}

package org.playdrop.android.ui.input

import org.playdrop.core.InputEvent
import org.playdrop.core.KeyAction
import org.playdrop.core.Modifier

/**
 * Turns edits in the keyboard tab's text field into key and text frames. The field always
 * starts with [SENTINEL], an invisible character, so Backspace on an "empty" field still
 * changes the text and reaches the desktop. Pure, so it is unit-tested.
 */
object KeyboardInput {
    /** Zero-width space kept at the start of the field. */
    const val SENTINEL = "​"

    /** W3C UI Events `key` values for the special key row, in display order. */
    val SPECIAL_KEYS = listOf(
        "Escape", "Tab", "Enter", "Backspace", "Delete",
        "ArrowLeft", "ArrowUp", "ArrowDown", "ArrowRight",
        "Home", "End", "PageUp", "PageDown",
    )

    /** Media keys, W3C names. */
    val MEDIA_KEYS = listOf("AudioVolumeDown", "AudioVolumeMute", "AudioVolumeUp", "MediaPlayPause")

    /** Volume up. */
    const val VOLUME_UP = "AudioVolumeUp"

    /** Volume down. */
    const val VOLUME_DOWN = "AudioVolumeDown"

    /**
     * The frames that turn [old] into [new] on the desktop: one Backspace per character
     * removed after the common prefix, then the inserted text. A newline becomes Enter. With
     * [mods] held, a single inserted character goes as that key with the modifiers (Ctrl+C)
     * instead of as text.
     */
    fun diff(old: String, new: String, mods: List<Modifier>): List<InputEvent> {
        val prefix = old.commonPrefixWith(new).length
        val removed = old.length - prefix
        val inserted = new.substring(prefix)
        val out = ArrayList<InputEvent>()
        repeat(removed) { out += key("Backspace") }
        if (inserted.isEmpty()) return out
        if (mods.isNotEmpty() && inserted.codePointCount(0, inserted.length) == 1 && inserted != "\n") {
            out += key(inserted.lowercase(), mods)
            return out
        }
        val parts = inserted.split('\n')
        parts.forEachIndexed { i, part ->
            if (part.isNotEmpty()) out += InputEvent.Text(text = part)
            if (i < parts.lastIndex) out += key("Enter", mods)
        }
        return out
    }

    /**
     * The field text after an edit: always starts with [SENTINEL] (restored if the user
     * deleted it) and is cut back to the sentinel once it grows past [maxLength], so the
     * field never fills up.
     */
    fun normalise(text: String, maxLength: Int = 200): String = when {
        !text.startsWith(SENTINEL) -> SENTINEL
        text.length > maxLength -> SENTINEL
        else -> text
    }

    /** A pressed (down then up) named key. */
    fun key(name: String, mods: List<Modifier> = emptyList()): InputEvent =
        InputEvent.Key(key = name, mods = mods, action = KeyAction.PRESS)
}

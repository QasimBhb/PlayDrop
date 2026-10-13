package org.playdrop.android.ui.input

import org.junit.Assert.assertEquals
import org.junit.Test
import org.playdrop.core.InputEvent
import org.playdrop.core.Modifier

/** Text field edits to key and text frames. */
class KeyboardInputTest {
    private val s = KeyboardInput.SENTINEL

    @Test
    fun typedCharactersGoAsText() {
        assertEquals(listOf(InputEvent.Text(text = "hi")), KeyboardInput.diff(s, s + "hi", emptyList()))
    }

    @Test
    fun backspaceOnTheEmptyFieldStillReachesTheDesktop() {
        assertEquals(listOf(KeyboardInput.key("Backspace")), KeyboardInput.diff(s, "", emptyList()))
        assertEquals(s, KeyboardInput.normalise(""))
    }

    @Test
    fun autocorrectBecomesBackspacesThenText() {
        val out = KeyboardInput.diff(s + "teh", s + "the ", emptyList())
        assertEquals(listOf(KeyboardInput.key("Backspace"), KeyboardInput.key("Backspace"), InputEvent.Text(text = "he ")), out)
    }

    @Test
    fun newlineIsEnterAndHeldModifiersMakeAShortcut() {
        assertEquals(
            listOf(InputEvent.Text(text = "a"), KeyboardInput.key("Enter"), InputEvent.Text(text = "b")),
            KeyboardInput.diff(s, s + "a\nb", emptyList()),
        )
        assertEquals(listOf(KeyboardInput.key("c", listOf(Modifier.CTRL))), KeyboardInput.diff(s, s + "C", listOf(Modifier.CTRL)))
    }

    @Test
    fun keyNamesAreW3cValues() {
        assertEquals(
            setOf("Escape", "Tab", "Enter", "Backspace", "Delete", "ArrowLeft", "ArrowUp", "ArrowDown", "ArrowRight", "Home", "End", "PageUp", "PageDown"),
            KeyboardInput.SPECIAL_KEYS.toSet(),
        )
        assertEquals(listOf("AudioVolumeDown", "AudioVolumeMute", "AudioVolumeUp", "MediaPlayPause"), KeyboardInput.MEDIA_KEYS)
    }

    @Test
    fun capabilityCheckNeedsPointerOrKey() {
        assertEquals(true, InputSupport.canControl(listOf("media.control", "input.pointer")))
        assertEquals(true, InputSupport.canControl(listOf("input.key")))
        assertEquals(false, InputSupport.canControl(listOf("input.presenter", "media.control")))
        assertEquals(false, InputSupport.canControl(emptyList()))
        val f = InputSupport.of(listOf("input.key", "input.presenter"))
        assertEquals(InputSupport.Features(pointer = false, keys = true, presenter = true), f)
    }
}

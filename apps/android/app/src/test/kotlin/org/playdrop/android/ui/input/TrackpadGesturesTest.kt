package org.playdrop.android.ui.input

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.playdrop.core.ButtonAction
import org.playdrop.core.InputEvent
import org.playdrop.core.PointerButton

/** Gesture to frame mapping on the trackpad, at 2 px per dp. */
class TrackpadGesturesTest {
    private val density = 2f

    private fun pad(sensitivity: Float = 1.5f) = TrackpadGestures(density, sensitivity)

    private fun click(b: PointerButton) = InputEvent.PointerButton(button = b, action = ButtonAction.CLICK)

    @Test
    fun quickTapIsALeftClick() {
        val g = pad()
        assertTrue(g.onDown(1, 100f, 100f, 0).isEmpty())
        assertTrue(g.onMove(1, 104f, 102f).isEmpty()) // 2 dp: under the slop
        assertEquals(listOf(click(PointerButton.LEFT)), g.onUp(1, 120))
    }

    @Test
    fun slowLiftWithoutMovingIsNotAClick() {
        val g = pad()
        g.onDown(1, 0f, 0f, 0)
        assertTrue(g.onUp(1, 300).isEmpty())
    }

    @Test
    fun dragPastTheSlopMovesByDpTimesSensitivity() {
        val g = pad(sensitivity = 1.5f)
        g.onDown(1, 0f, 0f, 0)
        // 20 px right, 0 down = 10 dp, past the 8 dp slop: the whole travel goes out.
        assertEquals(listOf(InputEvent.PointerMove(dx = 15.0, dy = 0.0)), g.onMove(1, 20f, 0f))
        // Then 4 px left and 8 px down = -2 dp, 4 dp, times 1.5.
        assertEquals(listOf(InputEvent.PointerMove(dx = -3.0, dy = 6.0)), g.onMove(1, 16f, 8f))
        // A drag never ends in a click.
        assertTrue(g.onUp(1, 100).isEmpty())
    }

    @Test
    fun sensitivityChangeAppliesToTheNextMove() {
        val g = pad(sensitivity = 1f)
        g.onDown(1, 0f, 0f, 0)
        g.onMove(1, 40f, 0f)
        g.sensitivity = 3f
        assertEquals(listOf(InputEvent.PointerMove(dx = 6.0, dy = 0.0)), g.onMove(1, 44f, 0f))
    }

    @Test
    fun twoFingerTapIsARightClick() {
        val g = pad()
        g.onDown(1, 100f, 100f, 0)
        g.onDown(2, 200f, 100f, 30)
        assertEquals(listOf(click(PointerButton.RIGHT)), g.onUp(2, 120))
        // The second finger's lift sends nothing more.
        assertTrue(g.onUp(1, 130).isEmpty())
    }

    @Test
    fun longPressThenDragIsDragAndDrop() {
        val g = pad(sensitivity = 1f)
        g.onDown(1, 0f, 0f, 1_000)
        assertEquals(1_450L, g.longPressDeadline())
        assertTrue("too early", g.onLongPressTimeout(1_200).isEmpty())
        val down = g.onLongPressTimeout(1_450)
        assertEquals(listOf(InputEvent.PointerButton(button = PointerButton.LEFT, action = ButtonAction.DOWN)), down)
        assertTrue(TrackpadGestures.wantsHaptic(down))
        assertNull(g.longPressDeadline())
        // Moves while held are plain moves, even small ones.
        assertEquals(listOf(InputEvent.PointerMove(dx = 1.0, dy = 0.0)), g.onMove(1, 2f, 0f))
        assertEquals(
            listOf(InputEvent.PointerButton(button = PointerButton.LEFT, action = ButtonAction.UP)),
            g.onUp(1, 3_000),
        )
    }

    @Test
    fun cancelReleasesAHeldDrag() {
        val g = pad()
        g.onDown(1, 0f, 0f, 0)
        g.onLongPressTimeout(500)
        assertEquals(
            listOf(InputEvent.PointerButton(button = PointerButton.LEFT, action = ButtonAction.UP)),
            g.onCancel(),
        )
        assertTrue(g.onCancel().isEmpty())
    }

    @Test
    fun movingCancelsTheLongPress() {
        val g = pad()
        g.onDown(1, 0f, 0f, 0)
        g.onMove(1, 40f, 0f)
        assertNull(g.longPressDeadline())
        assertTrue(g.onLongPressTimeout(1_000).isEmpty())
    }

    @Test
    fun twoFingerDragScrollsOneLinePer48Dp() {
        val g = pad()
        g.onDown(1, 100f, 400f, 0)
        g.onDown(2, 200f, 400f, 0)
        // Both fingers up by 60 px = 30 dp: past the slop, but under a line.
        assertTrue(g.onMove(1, 100f, 340f).isEmpty())
        assertTrue(g.onMove(2, 200f, 340f).isEmpty())
        // Another 40 px each (20 dp): centroid travel 50 dp, one line, scrolling down
        // (natural direction: fingers up, page down).
        g.onMove(1, 100f, 300f)
        assertEquals(listOf(InputEvent.Scroll(dx = 0.0, dy = 1.0, pixels = false)), g.onMove(2, 200f, 300f))
        // The 2 dp remainder carries: 46 dp more (92 px per finger) makes the next line.
        assertTrue(g.onMove(1, 100f, 208f).isEmpty())
        assertEquals(listOf(InputEvent.Scroll(dx = 0.0, dy = 1.0, pixels = false)), g.onMove(2, 200f, 208f))
        // Ending a scroll never clicks.
        assertTrue(g.onUp(1, 2_000).isEmpty())
        assertTrue(g.onUp(2, 2_010).isEmpty())
    }

    @Test
    fun twoFingerSideDragScrollsHorizontally() {
        val g = pad()
        g.onDown(1, 400f, 100f, 0)
        g.onDown(2, 400f, 300f, 0)
        // One finger 200 px (100 dp) left moves the centroid 50 dp: one line right, 2 dp left over.
        assertEquals(listOf(InputEvent.Scroll(dx = 1.0, dy = 0.0, pixels = false)), g.onMove(1, 200f, 100f))
        // The other finger does the same: 52 dp, one more line.
        val out = g.onMove(2, 200f, 300f)
        assertEquals(listOf(InputEvent.Scroll(dx = 1.0, dy = 0.0, pixels = false)), out)
        assertFalse(TrackpadGestures.wantsHaptic(out))
    }

    @Test
    fun aThirdFingerEndsTheGesture() {
        val g = pad()
        g.onDown(1, 0f, 0f, 0)
        g.onDown(2, 50f, 0f, 0)
        g.onDown(3, 100f, 0f, 0)
        assertTrue(g.onMove(1, 300f, 300f).isEmpty())
        assertTrue(g.onUp(3, 50).isEmpty())
        assertTrue(g.onUp(2, 60).isEmpty())
        assertTrue(g.onUp(1, 70).isEmpty())
        // Back to normal afterwards.
        g.onDown(4, 0f, 0f, 1_000)
        assertEquals(listOf(click(PointerButton.LEFT)), g.onUp(4, 1_050))
    }
}

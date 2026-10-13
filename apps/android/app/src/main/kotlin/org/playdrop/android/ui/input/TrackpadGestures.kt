package org.playdrop.android.ui.input

import org.playdrop.core.ButtonAction
import org.playdrop.core.InputEvent
import org.playdrop.core.PointerButton
import kotlin.math.hypot
import kotlin.math.truncate

/**
 * Turns raw touches on the trackpad surface into remote input frames. Pure: the composable
 * feeds it pointer downs, moves and ups (in pixels, with uptime milliseconds) and sends what
 * comes back, so every rule is unit-tested.
 *
 * Mapping:
 * - one finger drag: [InputEvent.PointerMove] with the travel in dp times [sensitivity]
 * - one finger tap (up within [Config.tapMs], travel under [Config.slopDp]): left click
 * - two finger tap: right click
 * - long press ([Config.longPressMs] without moving), then drag: left down, moves, left up on
 *   release (drag and drop)
 * - two finger drag: [InputEvent.Scroll] in lines, one line per [Config.lineDp] of travel,
 *   both axes, natural direction (fingers up scrolls the page down, like a laptop trackpad)
 *
 * A third finger, or a finger added mid-gesture, ends the gesture: nothing more is sent until
 * every finger is up (a held drag is released first).
 */
class TrackpadGestures(
    /** Pixels per dp of the screen. */
    private val density: Float,
    /** Pointer speed multiplier on finger travel in dp. */
    var sensitivity: Float,
    private val config: Config = Config(),
) {
    /** Thresholds; the defaults follow Android's touch slop and long-press timeouts. */
    data class Config(
        /** Travel under this many dp still counts as a tap. */
        val slopDp: Float = 8f,
        /** A tap must end within this long. */
        val tapMs: Long = 250,
        /** Holding still this long starts a drag. */
        val longPressMs: Long = 450,
        /** Two finger travel per scroll line. */
        val lineDp: Float = 48f,
    )

    private enum class Phase { IDLE, ONE_DOWN, MOVING, DRAGGING, TWO_DOWN, SCROLLING, WAIT_ALL_UP }

    private data class Point(var x: Float, var y: Float)

    private var phase = Phase.IDLE
    private val down = LinkedHashMap<Long, Point>()
    private val start = HashMap<Long, Point>()
    private var downAt = 0L
    private var scrollX = 0f
    private var scrollY = 0f

    /**
     * The uptime at which [onLongPressTimeout] should run, or null when no long press is
     * pending. The composable waits for the next touch at most until then.
     */
    fun longPressDeadline(): Long? = if (phase == Phase.ONE_DOWN) downAt + config.longPressMs else null

    /** A finger touched the surface. */
    fun onDown(id: Long, x: Float, y: Float, timeMs: Long): List<InputEvent> {
        down[id] = Point(x, y)
        start[id] = Point(x, y)
        return when (phase) {
            Phase.IDLE -> {
                phase = Phase.ONE_DOWN
                downAt = timeMs
                emptyList()
            }
            Phase.ONE_DOWN, Phase.MOVING -> {
                if (down.size == 2) {
                    phase = Phase.TWO_DOWN
                    downAt = timeMs
                    scrollX = 0f
                    scrollY = 0f
                    down.forEach { (k, p) -> start[k] = Point(p.x, p.y) }
                } else {
                    phase = Phase.WAIT_ALL_UP
                }
                emptyList()
            }
            Phase.DRAGGING -> {
                phase = Phase.WAIT_ALL_UP
                listOf(button(PointerButton.LEFT, ButtonAction.UP))
            }
            Phase.TWO_DOWN, Phase.SCROLLING, Phase.WAIT_ALL_UP -> {
                phase = Phase.WAIT_ALL_UP
                emptyList()
            }
        }
    }

    /** A finger moved to ([x], [y]). */
    fun onMove(id: Long, x: Float, y: Float): List<InputEvent> {
        val last = down[id] ?: return emptyList()
        val dxDp = (x - last.x) / density
        val dyDp = (y - last.y) / density
        return when (phase) {
            Phase.ONE_DOWN -> {
                val origin = start.getValue(id)
                if (hypot((x - origin.x) / density, (y - origin.y) / density) < config.slopDp) return emptyList()
                phase = Phase.MOVING
                last.x = x
                last.y = y
                move(dxDp, dyDp)
            }
            Phase.MOVING, Phase.DRAGGING -> {
                last.x = x
                last.y = y
                move(dxDp, dyDp)
            }
            Phase.TWO_DOWN -> {
                last.x = x
                last.y = y
                val travel = down.keys.maxOf { k ->
                    val p = down.getValue(k)
                    val s = start.getValue(k)
                    hypot((p.x - s.x) / density, (p.y - s.y) / density)
                }
                if (travel >= config.slopDp) {
                    phase = Phase.SCROLLING
                    // Count the travel that crossed the slop, from the centroid's start.
                    val (cx, cy) = centroidTravelDp()
                    scroll(cx, cy)
                } else {
                    emptyList()
                }
            }
            Phase.SCROLLING -> {
                last.x = x
                last.y = y
                // Each finger contributes half of the centroid's travel.
                scroll(dxDp / down.size, dyDp / down.size)
            }
            Phase.IDLE, Phase.WAIT_ALL_UP -> {
                last.x = x
                last.y = y
                emptyList()
            }
        }
    }

    /** A finger lifted. */
    fun onUp(id: Long, timeMs: Long): List<InputEvent> {
        if (down.remove(id) == null) return emptyList()
        start.remove(id)
        val out = when (phase) {
            Phase.ONE_DOWN -> {
                if (timeMs - downAt <= config.tapMs) listOf(button(PointerButton.LEFT, ButtonAction.CLICK)) else emptyList()
            }
            Phase.DRAGGING -> listOf(button(PointerButton.LEFT, ButtonAction.UP))
            Phase.TWO_DOWN -> {
                if (timeMs - downAt <= config.tapMs) listOf(button(PointerButton.RIGHT, ButtonAction.CLICK)) else emptyList()
            }
            else -> emptyList()
        }
        phase = if (down.isEmpty()) Phase.IDLE else Phase.WAIT_ALL_UP
        return out
    }

    /**
     * The long-press deadline passed with the finger still down and still: press the left
     * button for a drag. Does nothing when the gesture already became something else.
     */
    fun onLongPressTimeout(timeMs: Long): List<InputEvent> {
        val deadline = longPressDeadline() ?: return emptyList()
        if (timeMs < deadline) return emptyList()
        phase = Phase.DRAGGING
        return listOf(button(PointerButton.LEFT, ButtonAction.DOWN))
    }

    /** The system took the touch stream away: release a held drag and reset. */
    fun onCancel(): List<InputEvent> {
        val out = if (phase == Phase.DRAGGING) listOf(button(PointerButton.LEFT, ButtonAction.UP)) else emptyList()
        down.clear()
        start.clear()
        phase = Phase.IDLE
        return out
    }

    private fun move(dxDp: Float, dyDp: Float): List<InputEvent> {
        if (dxDp == 0f && dyDp == 0f) return emptyList()
        return listOf(InputEvent.PointerMove(dx = (dxDp * sensitivity).toDouble(), dy = (dyDp * sensitivity).toDouble()))
    }

    /** The centroid's travel from the two fingers' start points, in dp. */
    private fun centroidTravelDp(): Pair<Float, Float> {
        var sx = 0f
        var sy = 0f
        down.forEach { (k, p) ->
            val s = start.getValue(k)
            sx += p.x - s.x
            sy += p.y - s.y
        }
        return Pair(sx / down.size / density, sy / down.size / density)
    }

    /** Accumulates finger travel in dp and emits whole lines; the remainder carries over. */
    private fun scroll(dxDp: Float, dyDp: Float): List<InputEvent> {
        // Natural scrolling: content follows the fingers, so finger travel and scroll are opposite.
        scrollX -= dxDp
        scrollY -= dyDp
        val lx = truncate(scrollX / config.lineDp)
        val ly = truncate(scrollY / config.lineDp)
        if (lx == 0f && ly == 0f) return emptyList()
        scrollX -= lx * config.lineDp
        scrollY -= ly * config.lineDp
        return listOf(InputEvent.Scroll(dx = lx.toDouble(), dy = ly.toDouble(), pixels = false))
    }

    private fun button(b: PointerButton, a: ButtonAction) = InputEvent.PointerButton(button = b, action = a)

    companion object {
        /** True when [events] hold a click or a press, which the screen marks with a haptic tick. */
        fun wantsHaptic(events: List<InputEvent>): Boolean = events.any {
            it is InputEvent.PointerButton && (it.action == ButtonAction.CLICK || it.action == ButtonAction.DOWN)
        }
    }
}

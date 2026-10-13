package org.playdrop.android.ui.input

import android.view.KeyEvent

/**
 * Lets the presenter tab borrow the phone's hardware volume keys. While a handler is set,
 * `MainActivity.dispatchKeyEvent` hands it volume key presses, and the phone's own volume
 * stays untouched. Main thread only.
 */
object VolumeKeys {
    private var handler: ((up: Boolean) -> Unit)? = null

    /** Routes volume presses to [onPress] until [release] is called with the same function. */
    fun capture(onPress: (up: Boolean) -> Unit) {
        handler = onPress
    }

    /** Gives the keys back to the system if [onPress] still holds them. */
    fun release(onPress: (up: Boolean) -> Unit) {
        if (handler === onPress) handler = null
    }

    /**
     * Called by the activity for every key event. True when the event was a volume key
     * taken by the handler (both its down and its up are consumed, repeats send again).
     */
    fun dispatch(event: KeyEvent): Boolean {
        val h = handler ?: return false
        val up = when (event.keyCode) {
            KeyEvent.KEYCODE_VOLUME_UP -> true
            KeyEvent.KEYCODE_VOLUME_DOWN -> false
            else -> return false
        }
        if (event.action == KeyEvent.ACTION_DOWN) h(up)
        return true
    }
}

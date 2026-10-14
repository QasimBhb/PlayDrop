package org.playdrop.android.service

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import androidx.core.content.ContextCompat
import org.playdrop.android.core.HostLog

/**
 * Tells the service when this phone starts or stops sharing its connection (Wi-Fi hotspot,
 * USB tethering). Those downstream interfaces (`ap0`, `swlan0`, `rndis0`, `ncm0`) are not
 * Android "networks", so the default-network callback never fires for them, yet the computer
 * that joins the hotspot or the cable needs a multicast window and an interface reload to be
 * found within seconds. The actions are the long-standing system broadcasts; if a vendor does
 * not send them the core's own interface poll still finds the interface, only later.
 */
internal class TetherWatcher(private val context: Context, private val onChange: (String) -> Unit) {
    private var receiver: BroadcastReceiver? = null

    /** Starts listening. Safe to call once. */
    fun register() {
        if (receiver != null) return
        val r = object : BroadcastReceiver() {
            override fun onReceive(c: Context, intent: Intent) {
                val why = reasonFor(intent.action)
                if (why != null) onChange(why)
            }
        }
        val filter = IntentFilter().apply { ACTIONS.keys.forEach(::addAction) }
        try {
            // System broadcasts reach a non-exported receiver.
            ContextCompat.registerReceiver(context, r, filter, ContextCompat.RECEIVER_NOT_EXPORTED)
            receiver = r
        } catch (e: Exception) {
            HostLog.w("service", "tether watcher not registered; hotspot changes are found by polling", e)
        }
    }

    /** Stops listening. */
    fun unregister() {
        receiver?.let { runCatching { context.unregisterReceiver(it) } }
        receiver = null
    }

    companion object {
        /** Tether state, Wi-Fi hotspot state and USB state (RNDIS/NCM tethering shows up here). */
        internal val ACTIONS = mapOf(
            "android.net.conn.TETHER_STATE_CHANGED" to "tether state",
            "android.net.wifi.WIFI_AP_STATE_CHANGED" to "hotspot state",
            "android.hardware.usb.action.USB_STATE" to "usb state",
        )

        /** The text for the log line, or null for an action this watcher does not handle. */
        internal fun reasonFor(action: String?): String? = ACTIONS[action]
    }
}

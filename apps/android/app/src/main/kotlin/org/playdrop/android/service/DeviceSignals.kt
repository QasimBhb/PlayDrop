package org.playdrop.android.service

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.os.BatteryManager
import android.os.PowerManager
import androidx.core.content.ContextCompat
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.launch
import org.playdrop.android.core.CoreHost
import org.playdrop.android.core.HostLog
import org.playdrop.core.PlayDropHost
import org.playdrop.core.PresenceState

/**
 * Reports this phone's battery and presence to the core, which forwards them to peers
 * (protocol.md 5.8 and 5.10; the core rate-limits battery to once per 60 s unless charging
 * or low changes).
 *
 * Presence: screen off is `Locked`; screen on or unlock is `Active`; entering Doze is
 * `Asleep` (reason "doze", back within 15 minutes) so peers expect the drop; leaving Doze is
 * `Active`, or `Locked` when the screen is still off (a maintenance window).
 */
internal class DeviceSignals(private val context: Context, private val scope: CoroutineScope) {
    private val power = context.getSystemService(PowerManager::class.java)

    private val battery = object : BroadcastReceiver() {
        override fun onReceive(c: Context, intent: Intent) = reportBattery(intent)
    }

    private val presence = object : BroadcastReceiver() {
        override fun onReceive(c: Context, intent: Intent) {
            when (intent.action) {
                Intent.ACTION_SCREEN_OFF -> report(PresenceState.LOCKED, null, null)
                Intent.ACTION_SCREEN_ON, Intent.ACTION_USER_PRESENT -> report(PresenceState.ACTIVE, null, null)
                PowerManager.ACTION_DEVICE_IDLE_MODE_CHANGED -> reportIdle()
            }
        }
    }

    private var registered = false

    /** Registers the receivers. */
    fun register() {
        if (registered) return
        ContextCompat.registerReceiver(context, battery, IntentFilter(Intent.ACTION_BATTERY_CHANGED), ContextCompat.RECEIVER_NOT_EXPORTED)
        val filter = IntentFilter().apply {
            addAction(Intent.ACTION_SCREEN_OFF)
            addAction(Intent.ACTION_SCREEN_ON)
            addAction(Intent.ACTION_USER_PRESENT)
            addAction(PowerManager.ACTION_DEVICE_IDLE_MODE_CHANGED)
        }
        ContextCompat.registerReceiver(context, presence, filter, ContextCompat.RECEIVER_NOT_EXPORTED)
        registered = true
    }

    /** Pushes the current battery and presence; call once the core is running. */
    fun reportNow() {
        // A null receiver just reads the sticky battery broadcast.
        val sticky = context.registerReceiver(null, IntentFilter(Intent.ACTION_BATTERY_CHANGED))
        sticky?.let(::reportBattery)
        if (power.isDeviceIdleMode) reportIdle() else report(currentAwake(), null, null)
    }

    /** Unregisters the receivers. */
    fun unregister() {
        if (!registered) return
        runCatching { context.unregisterReceiver(battery) }
        runCatching { context.unregisterReceiver(presence) }
        registered = false
    }

    private fun reportBattery(intent: Intent) {
        val level = intent.getIntExtra(BatteryManager.EXTRA_LEVEL, -1)
        val scale = intent.getIntExtra(BatteryManager.EXTRA_SCALE, 100)
        if (level < 0 || scale <= 0) return
        val percent = (level * 100 / scale).coerceIn(0, 100)
        val status = intent.getIntExtra(BatteryManager.EXTRA_STATUS, -1)
        val charging = status == BatteryManager.BATTERY_STATUS_CHARGING || status == BatteryManager.BATTERY_STATUS_FULL
        val low = percent <= LOW_PERCENT && !charging
        call("battery") { it.setBattery(percent.toUByte(), charging, low) }
    }

    private fun reportIdle() {
        if (power.isDeviceIdleMode) {
            report(PresenceState.ASLEEP, "doze", DOZE_EXPECT_BACK_MS)
        } else {
            report(currentAwake(), null, null)
        }
    }

    private fun currentAwake(): PresenceState = if (power.isInteractive) PresenceState.ACTIVE else PresenceState.LOCKED

    private fun report(state: PresenceState, reason: String?, expectBackMs: ULong?) {
        call("presence") { it.setPresence(state, reason, expectBackMs) }
    }

    /** Runs a core call off the main thread; skipped quietly while the core is still starting. */
    private fun call(what: String, block: (PlayDropHost) -> Unit) {
        if (!CoreHost.running.value) return
        scope.launch {
            try {
                CoreHost.withHost { block(it) }
            } catch (e: Exception) {
                HostLog.w("signals", "reporting $what failed", e)
            }
        }
    }

    private companion object {
        const val LOW_PERCENT = 15
        val DOZE_EXPECT_BACK_MS: ULong = 900_000u
    }
}

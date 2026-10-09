package org.playdrop.android.service

import android.content.Context
import android.media.AudioAttributes
import android.media.AudioManager
import android.media.MediaPlayer
import android.media.RingtoneManager
import android.os.Handler
import android.os.Looper
import org.playdrop.android.core.HostLog

/**
 * Rings the phone for "find my phone": the alarm ringtone on the alarm stream at full
 * volume, looping, until a peer sends `find stop`, the user taps Stop, or 60 seconds pass.
 * The alarm stream's previous volume is restored afterwards. Main-thread only.
 */
internal object FindRinger {
    private const val MAX_RING_MS = 60_000L

    private val main = Handler(Looper.getMainLooper())
    private var player: MediaPlayer? = null
    private var savedVolume: Int? = null
    private val timeout = Runnable { stopNow(appContext ?: return@Runnable) }
    private var appContext: Context? = null

    /** Starts ringing (or restarts the 60 s timer if already ringing). Any thread. */
    fun start(context: Context) {
        val app = context.applicationContext
        main.post { startNow(app) }
    }

    /** Stops ringing and removes the notification. Any thread. */
    fun stop(context: Context) {
        val app = context.applicationContext
        main.post { stopNow(app) }
    }

    private fun startNow(app: Context) {
        appContext = app
        main.removeCallbacks(timeout)
        main.postDelayed(timeout, MAX_RING_MS)
        if (player != null) return
        val audio = app.getSystemService(AudioManager::class.java)
        try {
            if (savedVolume == null) savedVolume = audio.getStreamVolume(AudioManager.STREAM_ALARM)
            audio.setStreamVolume(AudioManager.STREAM_ALARM, audio.getStreamMaxVolume(AudioManager.STREAM_ALARM), 0)
        } catch (e: SecurityException) {
            // Do Not Disturb policy can refuse volume changes; ring at the current volume.
            HostLog.w("find", "could not raise the alarm volume", e)
        }
        val uri = RingtoneManager.getActualDefaultRingtoneUri(app, RingtoneManager.TYPE_ALARM)
            ?: RingtoneManager.getDefaultUri(RingtoneManager.TYPE_ALARM)
            ?: RingtoneManager.getDefaultUri(RingtoneManager.TYPE_RINGTONE)
        try {
            player = MediaPlayer().apply {
                setAudioAttributes(
                    AudioAttributes.Builder()
                        .setUsage(AudioAttributes.USAGE_ALARM)
                        .setContentType(AudioAttributes.CONTENT_TYPE_SONIFICATION)
                        .build(),
                )
                setDataSource(app, uri)
                isLooping = true
                prepare()
                start()
            }
            HostLog.i("find", "ringing")
        } catch (e: Exception) {
            HostLog.w("find", "could not play the alarm sound; the notification still vibrates", e)
            player?.release()
            player = null
        }
    }

    private fun stopNow(app: Context) {
        main.removeCallbacks(timeout)
        player?.let {
            runCatching { it.stop() }
            it.release()
            HostLog.i("find", "stopped")
        }
        player = null
        savedVolume?.let { v ->
            runCatching {
                app.getSystemService(AudioManager::class.java).setStreamVolume(AudioManager.STREAM_ALARM, v, 0)
            }.onFailure { HostLog.w("find", "could not restore the alarm volume", it) }
        }
        savedVolume = null
        Notifications.cancel(app, NotificationPlan.FIND_ID)
    }
}

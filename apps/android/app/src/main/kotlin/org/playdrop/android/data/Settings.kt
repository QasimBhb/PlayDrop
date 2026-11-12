package org.playdrop.android.data

import android.content.Context
import kotlinx.coroutines.flow.Flow

/**
 * User settings in DataStore preferences. Owned by the service agent; read and written by
 * every screen through this API. Keep the signatures.
 */
object Settings {
    /** Everything the app remembers. */
    data class Values(
        /** Name shown to peers; defaults to the device model. */
        val deviceName: String,
        /** Hex Device ID of the default peer for one-tap sends, or null until chosen. */
        val defaultPeer: String?,
        /** TCP port; 42187 unless overridden. */
        val port: Int,
        /** Push the clipboard automatically while the app is in the foreground. */
        val autoClipboard: Boolean,
        /** The user completed the first-run screens. */
        val onboarded: Boolean,
        /** The battery-optimisation request was offered after a user action (anti-pattern 14). */
        val batteryPromptShown: Boolean,
        /** Trackpad pointer speed on the remote input screen, a multiplier on finger travel in dp. */
        val trackpadSensitivity: Float = DEFAULT_TRACKPAD_SENSITIVITY,
        /**
         * Package names whose notifications may be mirrored to devices with the
         * `notificationsOut` grant (SR-41, third lock). Empty until the user picks apps.
         */
        val mirrorApps: Set<String> = emptySet(),
        /**
         * Ask GitHub for a newer release when the app opens, at most once a day
         * (`docs/design/update-check.md`, SR-45). On until the user turns it off.
         */
        val updateCheck: Boolean = true,
        /** When an update check last got an answer, Unix milliseconds; null before the first. */
        val updateLastAnswerMs: Long? = null,
        /** When an update check last failed (no network, refused answer), Unix milliseconds. */
        val updateLastFailureMs: Long? = null,
        /** The version the user answered "Later" for, or null. */
        val updateLaterVersion: String? = null,
        /** When the user answered "Later", Unix milliseconds, or null. */
        val updateLaterAtMs: Long? = null,
    )

    /** Trackpad speed used until the user picks one. */
    const val DEFAULT_TRACKPAD_SENSITIVITY = 1.5f

    /** Slowest trackpad speed the screen offers. */
    const val MIN_TRACKPAD_SENSITIVITY = 0.5f

    /** Fastest trackpad speed the screen offers. */
    const val MAX_TRACKPAD_SENSITIVITY = 4f

    /** Live settings. */
    fun flow(context: Context): Flow<Values> = SettingsImpl.flow(context)

    /** One read. */
    suspend fun read(context: Context): Values = SettingsImpl.read(context)

    /** Applies a change. */
    suspend fun update(context: Context, transform: (Values) -> Values) = SettingsImpl.update(context, transform)
}

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

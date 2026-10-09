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
    )

    /** Live settings. */
    fun flow(context: Context): Flow<Values> = SettingsImpl.flow(context)

    /** One read. */
    suspend fun read(context: Context): Values = SettingsImpl.read(context)

    /** Applies a change. */
    suspend fun update(context: Context, transform: (Values) -> Values) = SettingsImpl.update(context, transform)
}

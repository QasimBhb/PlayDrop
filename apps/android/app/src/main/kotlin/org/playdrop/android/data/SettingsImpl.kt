package org.playdrop.android.data

import android.content.Context
import android.os.Build
import android.util.Log
import androidx.datastore.core.DataStore
import androidx.datastore.preferences.core.Preferences
import androidx.datastore.preferences.core.booleanPreferencesKey
import androidx.datastore.preferences.core.edit
import androidx.datastore.preferences.core.emptyPreferences
import androidx.datastore.preferences.core.floatPreferencesKey
import androidx.datastore.preferences.core.intPreferencesKey
import androidx.datastore.preferences.core.stringPreferencesKey
import androidx.datastore.preferences.preferencesDataStore
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.catch
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.map
import java.io.IOException

/** The one DataStore file for [Settings]; excluded from backups by `data_extraction_rules`. */
private val Context.settingsStore: DataStore<Preferences> by preferencesDataStore(name = "settings")

/**
 * Backing DataStore for [Settings]. Every field of [Settings.Values] maps to one preference
 * key; a missing key reads as its documented default.
 */
internal object SettingsImpl {
    /** TCP port from protocol.md; used when the user never overrode it. */
    const val DEFAULT_PORT = 42187

    private val DEVICE_NAME = stringPreferencesKey("device_name")
    private val DEFAULT_PEER = stringPreferencesKey("default_peer")
    private val PORT = intPreferencesKey("port")
    private val AUTO_CLIPBOARD = booleanPreferencesKey("auto_clipboard")
    private val ONBOARDED = booleanPreferencesKey("onboarded")
    private val BATTERY_PROMPT_SHOWN = booleanPreferencesKey("battery_prompt_shown")
    private val TRACKPAD_SENSITIVITY = floatPreferencesKey("trackpad_sensitivity")

    /** Live settings; a read error logs and falls back to the defaults instead of crashing. */
    fun flow(context: Context): Flow<Settings.Values> =
        context.applicationContext.settingsStore.data
            .catch { e ->
                if (e is IOException) {
                    Log.w(TAG, "settings read failed (${e.javaClass.simpleName}); using defaults")
                    emit(emptyPreferences())
                } else {
                    throw e
                }
            }
            .map(::fromPreferences)

    /** One read of the current settings. */
    suspend fun read(context: Context): Settings.Values = flow(context).first()

    /** Applies [transform] atomically and writes only the fields that changed. */
    suspend fun update(context: Context, transform: (Settings.Values) -> Settings.Values) {
        context.applicationContext.settingsStore.edit { prefs ->
            val before = fromPreferences(prefs)
            val after = transform(before)
            if (after == before) return@edit
            prefs[DEVICE_NAME] = after.deviceName
            if (after.defaultPeer == null) prefs.remove(DEFAULT_PEER) else prefs[DEFAULT_PEER] = after.defaultPeer
            prefs[PORT] = after.port
            prefs[AUTO_CLIPBOARD] = after.autoClipboard
            prefs[ONBOARDED] = after.onboarded
            prefs[BATTERY_PROMPT_SHOWN] = after.batteryPromptShown
            prefs[TRACKPAD_SENSITIVITY] = after.trackpadSensitivity
        }
    }

    /** Maps stored preferences to values, filling in defaults. */
    private fun fromPreferences(prefs: Preferences): Settings.Values = Settings.Values(
        deviceName = prefs[DEVICE_NAME]?.takeIf { it.isNotBlank() } ?: defaultDeviceName(),
        defaultPeer = prefs[DEFAULT_PEER]?.takeIf { it.isNotBlank() },
        port = prefs[PORT]?.takeIf { it in 1..65535 } ?: DEFAULT_PORT,
        // Zero-action clipboard sync is a defining goal (CLAUDE.md), so it starts on.
        autoClipboard = prefs[AUTO_CLIPBOARD] ?: true,
        onboarded = prefs[ONBOARDED] ?: false,
        batteryPromptShown = prefs[BATTERY_PROMPT_SHOWN] ?: false,
        trackpadSensitivity = prefs[TRACKPAD_SENSITIVITY]
            ?.takeIf { it in Settings.MIN_TRACKPAD_SENSITIVITY..Settings.MAX_TRACKPAD_SENSITIVITY }
            ?: Settings.DEFAULT_TRACKPAD_SENSITIVITY,
    )

    /** The device model, e.g. "SM-A546B"; the core sanitises it further (SR-8). */
    private fun defaultDeviceName(): String = Build.MODEL?.takeIf { it.isNotBlank() } ?: "Android phone"

    private const val TAG = "playdrop"
}

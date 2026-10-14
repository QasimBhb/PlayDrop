package org.playdrop.android.ui.help

import android.content.Intent
import android.provider.Settings

/** Which part of the "Can't find your computer?" screen to point at first. */
enum class HelpTopic(val arg: String) {
    /** Same network but discovery is filtered: pair by QR or address. */
    SAME_NETWORK("same"),

    /** The devices are on different networks: hotspot or USB tethering. */
    OTHER_NETWORK("other"),
    ;

    companion object {
        /** The topic for a route argument; null for none or an unknown value. */
        fun of(arg: String?): HelpTopic? = entries.firstOrNull { it.arg == arg }
    }
}

/**
 * Settings screens that turn on the hotspot or USB tethering. The tether screen has no public
 * action, so it is tried by component first, with the public wireless settings as fallback;
 * the caller starts them in order and stops at the first one that opens.
 */
object TetherSettings {
    private const val SETTINGS_PACKAGE = "com.android.settings"
    private const val TETHER_ACTIVITY = "com.android.settings.TetherSettings"

    /** Intents to try, most specific first. */
    fun intents(): List<Intent> = listOf(
        Intent(Intent.ACTION_MAIN).setClassName(SETTINGS_PACKAGE, TETHER_ACTIVITY),
        Intent(Settings.ACTION_WIRELESS_SETTINGS),
        Intent(Settings.ACTION_SETTINGS),
    ).map { it.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK) }
}

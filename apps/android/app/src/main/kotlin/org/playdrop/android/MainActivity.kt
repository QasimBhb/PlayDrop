package org.playdrop.android

import androidx.lifecycle.lifecycleScope
import kotlinx.coroutines.launch
import org.playdrop.android.core.CoreHost
import org.playdrop.android.data.Settings
import android.content.ClipDescription
import android.content.ClipboardManager
import android.content.Intent
import android.os.Build
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.activity.viewModels
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import org.playdrop.android.ui.AppViewModel
import org.playdrop.android.ui.PlayDropRoot
import org.playdrop.android.ui.theme.PlayDropTheme

/**
 * The single activity. Hosts the Compose UI and, while resumed with auto clipboard on, pushes
 * clipboard changes to peers (Android 10 and later only let the focused app read the
 * clipboard, so this is the only place the phone can do it without a tap).
 */
class MainActivity : ComponentActivity() {
    private val appVm: AppViewModel by viewModels()
    private var openTab by mutableStateOf<String?>(null)
    private val clipboard by lazy { getSystemService(ClipboardManager::class.java) }
    private val clipListener = ClipboardManager.OnPrimaryClipChangedListener { readClipboard() }

    override fun onCreate(savedInstanceState: Bundle?) {
        enableEdgeToEdge()
        super.onCreate(savedInstanceState)
        if (savedInstanceState == null) openTab = intent?.getStringExtra(EXTRA_OPEN_TAB)
        setContent {
            PlayDropTheme {
                PlayDropRoot(appVm = appVm, openTab = openTab, onTabOpened = { openTab = null })
            }
        }
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        setIntent(intent)
        intent.getStringExtra(EXTRA_OPEN_TAB)?.let { openTab = it }
    }

    override fun onStart() {
        super.onStart()
        // The service owns the core; the window is only a client of it. Opening the app
        // after onboarding makes sure the service is up even when no screen asked for it.
        lifecycleScope.launch {
            if (Settings.read(applicationContext).onboarded) {
                CoreHost.ensureServiceRunning(applicationContext)
            }
        }
    }

    override fun onResume() {
        super.onResume()
        clipboard?.addPrimaryClipChangedListener(clipListener)
    }

    override fun onPause() {
        clipboard?.removePrimaryClipChangedListener(clipListener)
        super.onPause()
    }

    override fun onWindowFocusChanged(hasFocus: Boolean) {
        super.onWindowFocusChanged(hasFocus)
        // Reading needs window focus on Android 10+, so a clip copied in another app is
        // picked up here, the moment the user comes back.
        if (hasFocus) readClipboard()
    }

    /** Hands the current clip to the view model; the text is never logged (SR-32). */
    private fun readClipboard() {
        val cm = clipboard ?: return
        val clip = try {
            cm.primaryClip
        } catch (_: SecurityException) {
            null
        } ?: return
        if (clip.itemCount == 0) return
        val extras = clip.description?.extras
        val sensitive = extras != null && (
            extras.getBoolean(EXTRA_IS_SENSITIVE_COMPAT, false) ||
                (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU && extras.getBoolean(ClipDescription.EXTRA_IS_SENSITIVE, false))
            )
        val text = clip.getItemAt(0).text?.toString()
        appVm.onClipboardChanged(text, sensitive)
    }

    companion object {
        /** Intent extra naming a tab to open: `devices`, `transfers` or `settings`. */
        const val EXTRA_OPEN_TAB = "org.playdrop.android.extra.OPEN_TAB"

        /** `ClipDescription.EXTRA_IS_SENSITIVE`, spelled out for API levels below 33 (SR-24). */
        private const val EXTRA_IS_SENSITIVE_COMPAT = "android.content.extra.IS_SENSITIVE"
    }
}

package org.playdrop.android.update

import android.app.Application
import android.content.Intent
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.flow.stateIn
import kotlinx.coroutines.launch
import org.playdrop.android.BuildConfig
import org.playdrop.android.data.Settings
import org.playdrop.core.updateRedirectAllowed
import org.playdrop.core.updateSigUrl
import org.playdrop.core.updateSumsUrl

/** What the Settings "Updates" rows show. */
data class UpdateSettingsState(
    /** The "Check for updates when PlayDrop opens" switch. */
    val enabled: Boolean = true,
    /** Last check that got an answer, Unix ms, or null. */
    val lastCheckedMs: Long? = null,
    /** False for a build that is not a plain release: it never checks. */
    val available: Boolean = true,
)

/**
 * The update check for the whole activity (`MainActivity` owns it, so a download survives a
 * rotation). A download that is running when the activity finishes for good is cancelled and
 * its file deleted ([onCleared]); the person starts it again from the next offer.
 */
class UpdateViewModel(application: Application) : AndroidViewModel(application) {
    private val hooks: UpdateHooks = BuildUpdateHooks
    /** Read once, on first use (off the main thread, inside a check). */
    private val limits by lazy { AndroidUpdate.limits() }
    private val controller = UpdateController(
        scope = viewModelScope,
        store = SettingsUpdateStore(application),
        rules = CoreUpdateRules,
        channel = ::channel,
        files = CacheDownloadFiles(application.cacheDir),
        installer = PackageInstallerUpdater(application),
        // As it is: a build that is not a plain release does not check at all.
        currentVersion = BuildConfig.VERSION_NAME,
    )

    /** The flow's state, for the dialog. */
    val state: StateFlow<UpdateState> = controller.state

    /** The Settings rows. */
    val settings: StateFlow<UpdateSettingsState> = Settings.flow(application)
        .map { UpdateSettingsState(it.updateCheck, it.updateLastAnswerMs, controller.checksAvailable) }
        .stateIn(viewModelScope, SharingStarted.WhileSubscribed(5_000), UpdateSettingsState())

    init {
        controller.start()
        viewModelScope.launch {
            UpdateInstallBus.results.collect { controller.onInstallResult(it) }
        }
    }

    /**
     * `MainActivity` came to the foreground: the only automatic trigger. A debug build reads
     * its test extras from [intent] first; the release build has no code that reads them.
     */
    fun onForeground(intent: Intent?) {
        val request = hooks.read(intent)
        controller.onPermissionMaybeGranted()
        if (request.checkNow) controller.checkNow() else controller.onForeground()
    }

    /** "Check now" in Settings. */
    fun checkNow() = controller.checkNow()

    /** The Settings switch. */
    fun setEnabled(on: Boolean) {
        viewModelScope.launch { Settings.update(getApplication()) { it.copy(updateCheck = on) } }
    }

    /** "Update". */
    fun update() = controller.update()

    /** "Later". */
    fun later() = controller.later()

    /** Closes the dialog; a running download is cancelled and deleted. */
    fun dismiss() = controller.dismiss()

    override fun onCleared() {
        controller.close()
    }

    /** GitHub and the release keys, or a debug build's stand-in and test key. */
    private fun channel(): UpdateChannel {
        val override = hooks.current()
        val endpoints = UpdateEndpoints(updateSumsUrl(), updateSigUrl(), ::updateRedirectAllowed, override)
        val decider = if (override != null) OfferDecider(override::offer) else CoreOfferDecider
        return UpdateChannel(endpoints, decider, UpdateFetcher(UrlConnectionTransport(), limits, endpoints::redirectAllowed), limits)
    }
}

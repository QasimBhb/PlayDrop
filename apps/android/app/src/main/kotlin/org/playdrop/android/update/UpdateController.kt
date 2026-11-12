package org.playdrop.android.update

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineDispatcher
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import org.playdrop.core.UpdateOffer
import java.io.File
import java.io.IOException
import java.io.InterruptedIOException

/** What the update check stores between runs (in `Settings.Values`). */
data class UpdateRecord(
    /** The "Check for updates when PlayDrop opens" switch. */
    val enabled: Boolean,
    /** Last check that got an answer, Unix ms. */
    val lastAnswerMs: Long?,
    /** Last check that failed, Unix ms. */
    val lastFailureMs: Long?,
    /** The version "Later" was answered for. */
    val laterVersion: String?,
    /** When "Later" was answered, Unix ms. */
    val laterAtMs: Long?,
)

/** Reads and changes the [UpdateRecord]. */
interface UpdateStore {
    /** One read. */
    suspend fun load(): UpdateRecord

    /** Applies [transform] atomically. */
    suspend fun save(transform: (UpdateRecord) -> UpdateRecord)
}

/** The core's update decisions (`playdrop_core::update`), behind an interface for JUnit. */
interface UpdateRules {
    /** `update_check_due`. */
    fun due(lastAnswerMs: Long?, lastFailureMs: Long?, nowMs: Long): Boolean

    /** `update_asset_matches`. */
    fun assetMatches(asset: org.playdrop.core.UpdateAsset, sha256: ByteArray): Boolean
}

/**
 * What to offer for the two files: null when the release is not newer. Throws
 * [UpdateFetchException] with [UpdateFailure.NOT_VERIFIED], [UpdateFailure.TOO_LARGE] or
 * [UpdateFailure.BAD_VERSION] when the core refuses the answer (SR-46).
 */
fun interface OfferDecider {
    @Throws(UpdateFetchException::class)
    fun offer(sums: ByteArray, sig: ByteArray, currentVersion: String): UpdateOffer?
}

/** Fresh files for downloads, under the app's cache. */
interface DownloadFiles {
    /** A new, empty file that no earlier download used. */
    fun create(): File

    /** Deletes [file]; never throws. */
    fun delete(file: File)

    /** Deletes what a killed process left behind. */
    fun clearLeftovers()
}

/** How handing a verified APK to Android went. */
sealed interface InstallStart {
    /** Committed: Android answers through [UpdateController.onInstallResult]. */
    data object Committed : InstallStart

    /** Android refused before asking anyone. */
    data class Failed(val failure: UpdateFailure) : InstallStart
}

/** What Android answered about a committed install. */
sealed interface InstallResult {
    /** The person must confirm in Android's dialog (the app shows it). */
    data object NeedsConfirmation : InstallResult

    /** Installed; Android stops this process and starts the new version's service on its own. */
    data object Installed : InstallResult

    /** Refused, cancelled or failed. */
    data class Failed(val failure: UpdateFailure) : InstallResult
}

/** The system installer, behind an interface for JUnit. */
interface UpdateInstaller {
    /** The person allowed PlayDrop to install apps ("Install unknown apps"). */
    fun canInstall(): Boolean

    /** Hands [apk] to Android. Blocking; the caller deletes [apk] afterwards. */
    fun install(apk: File): InstallStart
}

/**
 * Where requests go and who decides on the answer. Read again for every check, so a debug
 * build can point at a stand-in server from an intent without a restart.
 */
class UpdateChannel(
    /** URLs and the redirect rule. */
    val endpoints: UpdateEndpoints,
    /** The core's decision, with the release keys (or a debug test key). */
    val decider: OfferDecider,
    /** The HTTP side, using [endpoints]' redirect rule. */
    val fetcher: UpdateFetcher,
    /** The core's limits. */
    val limits: UpdateLimits,
)

/** Where the update flow stands; the dialog and the Settings row show it. */
sealed interface UpdateState {
    /** Nothing to show. */
    data object Idle : UpdateState

    /** A check is running; [manual] when "Check now" started it. */
    data class Checking(val manual: Boolean) : UpdateState

    /** "Check now" found nothing newer. */
    data object UpToDate : UpdateState

    /** A newer release: "PlayDrop <version> is available". */
    data class Available(val offer: UpdateOffer) : UpdateState

    /** "Update" was pressed but PlayDrop may not install apps yet: explain and open the setting. */
    data class NeedsPermission(val offer: UpdateOffer) : UpdateState

    /** Downloading [done] of [total] bytes (-1 while unknown). */
    data class Downloading(val offer: UpdateOffer, val done: Long, val total: Long) : UpdateState

    /** Checking the download against the signed checksum. */
    data class Verifying(val offer: UpdateOffer) : UpdateState

    /** Handed to Android; its own dialog takes over. */
    data class HandingOver(val offer: UpdateOffer) : UpdateState

    /** Failed with a plain sentence; [offer] is set when an update (not a check) failed. */
    data class Failed(val failure: UpdateFailure, val offer: UpdateOffer?) : UpdateState
}

/**
 * The update check and update flow of the Android app (`docs/design/update-check.md`,
 * SR-45 to SR-48), free of Android types so JUnit runs it with fakes.
 *
 * - [onForeground] (only `MainActivity` coming to the front calls it) checks when the
 *   switch is on and the core's once-a-day rule says so; [checkNow] skips the day, not the
 *   switch. Nothing else starts a check: no service, receiver or worker.
 * - "Later" ([later]) counts as the start of a new day: the next automatic check is a day
 *   after it, so the offer stays hidden until then.
 * - [update] runs only on the person's tap: permission, download into a fresh cache file,
 *   SHA-256 while streaming, `update_asset_matches`, then the system installer. The file is
 *   deleted on every path, success or not.
 * - A failed automatic check is silent (its time is recorded for the one-hour retry); a
 *   failed "Check now" or update says why.
 * - [close] (the activity is finishing) cancels a running download and deletes its file.
 */
/**
 * Whether a version name is a plain release, `major.minor.patch` with no leading zeros and
 * nothing after it: the only kind of build that checks for updates. A pre-release or test
 * build (`0.11.0-debug`) never contacts GitHub, since it cannot tell whether a release is
 * newer. The same rule as the core's `Version::parse`; decided here, before any request.
 */
object ReleaseVersion {
    private val PLAIN = Regex("""(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)""")

    /** True for `0.11.0`, false for `0.11.0-debug`, `0.11`, `v0.11.0` or `01.2.3`. */
    fun isPlain(versionName: String): Boolean = PLAIN.matches(versionName)
}

class UpdateController(
    private val scope: CoroutineScope,
    private val store: UpdateStore,
    private val rules: UpdateRules,
    private val channel: () -> UpdateChannel,
    private val files: DownloadFiles,
    private val installer: UpdateInstaller,
    private val currentVersion: String,
    private val now: () -> Long = System::currentTimeMillis,
    private val io: CoroutineDispatcher = Dispatchers.IO,
) {
    private val mutable = MutableStateFlow<UpdateState>(UpdateState.Idle)
    private var job: Job? = null

    /**
     * False for a build whose version is not a plain release ([ReleaseVersion]): it makes
     * no request at all, and Settings says so.
     */
    val checksAvailable: Boolean = ReleaseVersion.isPlain(currentVersion)

    /** The offer whose APK Android is installing now, until it answers. */
    @Volatile
    private var handedOver: UpdateOffer? = null

    /** The current state. */
    val state: StateFlow<UpdateState> = mutable

    private val busy: Boolean get() = job?.isActive == true

    /** Starts [block] as the one running job; it is recorded before it runs, so it can always be cancelled. */
    private fun launchJob(block: suspend () -> Unit) {
        val j = scope.launch(start = CoroutineStart.LAZY) { block() }
        job = j
        j.start()
    }

    /** Removes downloads a killed process left in the cache. */
    fun start() {
        scope.launch(io) { files.clearLeftovers() }
    }

    /** `MainActivity` came to the foreground: check if the switch is on and a check is due. */
    fun onForeground() {
        if (!checksAvailable || busy || mutable.value !is UpdateState.Idle) return
        launchJob {
            val record = store.load()
            if (!record.enabled) return@launchJob
            // The rule lives in the core library: ask it off the main thread.
            if (withContext(io) { UpdateSchedule.due(record, now(), rules) }) check(manual = false)
        }
    }

    /** "Check now": skips the once-a-day rule, never the switch. */
    fun checkNow() {
        if (!checksAvailable || busy) return
        launchJob {
            if (store.load().enabled) check(manual = true)
        }
    }

    /** "Later": hide the offer until the next due check. */
    fun later() {
        val offer = (mutable.value as? UpdateState.Available)?.offer ?: return
        mutable.value = UpdateState.Idle
        scope.launch {
            val at = now()
            store.save { it.copy(laterVersion = offer.version, laterAtMs = at) }
        }
    }

    /** "Update" on the offer: ask for the install permission when missing, else download. */
    fun update() {
        val offer = when (val s = mutable.value) {
            is UpdateState.Available -> s.offer
            is UpdateState.NeedsPermission -> s.offer
            else -> return
        }
        if (busy) return
        val asset = offer.asset ?: return
        if (!installer.canInstall()) {
            mutable.value = UpdateState.NeedsPermission(offer)
            return
        }
        launchJob { download(offer, asset) }
    }

    /** The app came back from the "Install unknown apps" setting: go on when it is now allowed. */
    fun onPermissionMaybeGranted() {
        if (mutable.value is UpdateState.NeedsPermission && installer.canInstall()) update()
    }

    /**
     * Android's answer about the install. A failure is shown even when the hand-over notice
     * was closed meanwhile; an answer with no hand-over of ours is ignored.
     */
    fun onInstallResult(result: InstallResult) {
        val offer = handedOver ?: return
        when (result) {
            InstallResult.NeedsConfirmation -> Unit
            InstallResult.Installed -> {
                handedOver = null
                mutable.value = UpdateState.Idle
            }
            is InstallResult.Failed -> {
                handedOver = null
                mutable.value = UpdateState.Failed(result.failure, offer)
            }
        }
    }

    /** Hides whatever is shown; a running download is cancelled and its file deleted. */
    fun dismiss() {
        job?.takeIf { mutable.value is UpdateState.Downloading || mutable.value is UpdateState.Verifying }?.cancel()
        mutable.value = UpdateState.Idle
    }

    /** The activity is finishing: cancel a running download (its file is deleted). */
    fun close() {
        job?.cancel()
    }

    private suspend fun check(manual: Boolean) {
        mutable.value = UpdateState.Checking(manual)
        val outcome = try {
            val (ch, offer) = withContext(io) {
                // Built here, off the main thread: it calls into the core library.
                val ch = channel()
                val sums = ch.fetcher.getSmall(ch.endpoints.sums, ch.limits.maxSumsLen)
                val sig = ch.fetcher.getSmall(ch.endpoints.sig, ch.limits.maxSigLen)
                ch to ch.decider.offer(sums, sig, currentVersion)
            }
            val at = now()
            store.save { it.copy(lastAnswerMs = at) }
            when {
                offer != null -> UpdateState.Available(ch.endpoints.rebase(offer))
                manual -> UpdateState.UpToDate
                else -> UpdateState.Idle
            }
        } catch (e: UpdateFetchException) {
            val at = now()
            store.save { it.copy(lastFailureMs = at) }
            if (manual) UpdateState.Failed(e.failure, null) else UpdateState.Idle
        }
        mutable.value = outcome
    }

    private suspend fun download(offer: UpdateOffer, asset: org.playdrop.core.UpdateAsset) {
        mutable.value = UpdateState.Downloading(offer, 0, -1)
        var file: File? = null
        try {
            val outcome = withContext(io) {
                val ch = channel()
                val apk = files.create().also { file = it }
                val ctx = currentCoroutineContext()
                val got = try {
                    apk.outputStream().use { out ->
                        ch.fetcher.download(asset.url, out, ch.limits.maxDownloadLen, stopped = { !ctx.isActive }) { done, total ->
                            mutable.value = UpdateState.Downloading(offer, done, total)
                        }
                    }
                } catch (e: InterruptedIOException) {
                    throw CancellationException("download cancelled").apply { initCause(e) }
                } catch (e: UpdateFetchException) {
                    throw e
                } catch (e: IOException) {
                    // Creating or closing the file failed: the phone's storage, not the network.
                    throw UpdateFetchException(UpdateFailure.STORAGE, e)
                }
                mutable.value = UpdateState.Verifying(offer)
                if (!rules.assetMatches(asset, got.sha256)) return@withContext UpdateState.Failed(UpdateFailure.DIGEST_MISMATCH, offer)
                mutable.value = UpdateState.HandingOver(offer)
                handedOver = offer
                when (val r = installer.install(apk)) {
                    InstallStart.Committed -> null
                    is InstallStart.Failed -> {
                        handedOver = null
                        UpdateState.Failed(r.failure, offer)
                    }
                }
            }
            // Committed: stay on HandingOver until Android answers.
            if (outcome != null) mutable.value = outcome
        } catch (e: UpdateFetchException) {
            mutable.value = UpdateState.Failed(e.failure, offer)
        } catch (e: CancellationException) {
            if (mutable.value !is UpdateState.Idle) mutable.value = UpdateState.Idle
            throw e
        } finally {
            // Every path: success, mismatch, failure or cancel (design note section 6).
            file?.let(files::delete)
        }
    }
}

/** The once-a-day rule with "Later" folded in. Pure. */
object UpdateSchedule {
    /**
     * Whether an automatic check may run at [nowMs]: the core's rule, counting a "Later"
     * answer as the start of a new day (it came after the last answered check, so the offer
     * it hid is not asked about again for a day).
     */
    fun due(record: UpdateRecord, nowMs: Long, rules: UpdateRules): Boolean {
        val dayStart = listOfNotNull(record.lastAnswerMs, record.laterAtMs).maxOrNull()
        return rules.due(dayStart, record.lastFailureMs, nowMs)
    }
}

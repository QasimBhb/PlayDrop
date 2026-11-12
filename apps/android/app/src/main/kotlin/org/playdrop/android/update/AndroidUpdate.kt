package org.playdrop.android.update

import android.app.PendingIntent
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.pm.PackageInstaller
import android.os.Build
import androidx.core.content.IntentCompat
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharedFlow
import org.playdrop.android.core.HostLog
import org.playdrop.android.data.Settings
import org.playdrop.core.UpdateAsset
import org.playdrop.core.UpdateCheckException
import org.playdrop.core.UpdateInstallKind
import org.playdrop.core.UpdateOffer
import org.playdrop.core.updateAssetMatches
import org.playdrop.core.updateCheckDue
import org.playdrop.core.updateMaxDownloadLen
import org.playdrop.core.updateMaxRedirects
import org.playdrop.core.updateMaxSigLen
import org.playdrop.core.updateMaxSumsLen
import org.playdrop.core.updateOffer
import org.playdrop.core.updateRequestTimeoutMs
import org.playdrop.core.updateUserAgent
import java.io.File
import java.util.UUID

/** The Android side of the update check: settings, the core's rules and the system installer. */
internal object AndroidUpdate {
    /** The core's fixed limits. */
    fun limits(): UpdateLimits = UpdateLimits(
        userAgent = updateUserAgent(),
        maxRedirects = updateMaxRedirects().toInt(),
        maxSumsLen = updateMaxSumsLen().toLong(),
        maxSigLen = updateMaxSigLen().toLong(),
        maxDownloadLen = updateMaxDownloadLen().toLong(),
        timeoutMs = updateRequestTimeoutMs().toInt(),
    )
}

/** [UpdateStore] over [Settings]. */
internal class SettingsUpdateStore(private val context: Context) : UpdateStore {
    override suspend fun load(): UpdateRecord = Settings.read(context).toRecord()

    override suspend fun save(transform: (UpdateRecord) -> UpdateRecord) {
        Settings.update(context) { v ->
            val r = transform(v.toRecord())
            v.copy(
                updateCheck = r.enabled,
                updateLastAnswerMs = r.lastAnswerMs,
                updateLastFailureMs = r.lastFailureMs,
                updateLaterVersion = r.laterVersion,
                updateLaterAtMs = r.laterAtMs,
            )
        }
    }

    private fun Settings.Values.toRecord() = UpdateRecord(
        enabled = updateCheck,
        lastAnswerMs = updateLastAnswerMs,
        lastFailureMs = updateLastFailureMs,
        laterVersion = updateLaterVersion,
        laterAtMs = updateLaterAtMs,
    )
}

/** [UpdateRules] from `playdrop-ffi`. */
internal object CoreUpdateRules : UpdateRules {
    override fun due(lastAnswerMs: Long?, lastFailureMs: Long?, nowMs: Long): Boolean =
        updateCheckDue(lastAnswerMs?.toULong(), lastFailureMs?.toULong(), nowMs.toULong())

    override fun assetMatches(asset: UpdateAsset, sha256: ByteArray): Boolean = updateAssetMatches(asset, sha256)
}

/** The release decision: only the release keys built into the core are trusted (SR-46). */
internal object CoreOfferDecider : OfferDecider {
    override fun offer(sums: ByteArray, sig: ByteArray, currentVersion: String): UpdateOffer? =
        refusalsAsFailures { updateOffer(sums, sig, currentVersion, UpdateInstallKind.ANDROID) }
}

/** Runs a core decision, turning its refusals into [UpdateFetchException]s. */
internal inline fun refusalsAsFailures(decide: () -> UpdateOffer?): UpdateOffer? = try {
    decide()
} catch (e: UpdateCheckException) {
    val failure = when (e) {
        is UpdateCheckException.TooLarge -> UpdateFailure.TOO_LARGE
        is UpdateCheckException.BadCurrentVersion -> UpdateFailure.BAD_VERSION
        else -> UpdateFailure.NOT_VERIFIED
    }
    throw UpdateFetchException(failure, e)
}

/** Downloads go to fresh files in `cache/updates`, which nothing else uses. */
internal class CacheDownloadFiles(cacheDir: File) : DownloadFiles {
    private val dir = File(cacheDir, "updates")

    override fun create(): File {
        if (!dir.isDirectory && !dir.mkdirs()) throw java.io.IOException("could not create the download folder")
        val file = File(dir, "playdrop-${UUID.randomUUID()}.apk")
        if (!file.createNewFile()) throw java.io.IOException("could not create the download file")
        return file
    }

    override fun delete(file: File) {
        if (file.exists() && !file.delete()) HostLog.w("update", "could not delete a downloaded update")
    }

    override fun clearLeftovers() {
        dir.listFiles()?.forEach(::delete)
    }
}

/**
 * Hands a verified APK to Android with the `PackageInstaller` session API. The person always
 * confirms in Android's own dialog (SR-47; `USER_ACTION_REQUIRED` on Android 12 and newer),
 * and Android refuses an APK signed with another key than the installed app.
 */
internal class PackageInstallerUpdater(context: Context) : UpdateInstaller {
    private val app = context.applicationContext

    override fun canInstall(): Boolean = app.packageManager.canRequestPackageInstalls()

    override fun install(apk: File): InstallStart {
        val installer = app.packageManager.packageInstaller
        val params = PackageInstaller.SessionParams(PackageInstaller.SessionParams.MODE_FULL_INSTALL).apply {
            setAppPackageName(app.packageName)
            setSize(apk.length())
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
                setRequireUserAction(PackageInstaller.SessionParams.USER_ACTION_REQUIRED)
            }
        }
        val id = try {
            installer.createSession(params)
        } catch (e: Exception) {
            HostLog.w("update", "could not open an install session", e)
            return InstallStart.Failed(UpdateFailure.INSTALL_FAILED)
        }
        val session = installer.openSession(id)
        return try {
            session.openWrite("playdrop.apk", 0, apk.length()).use { out ->
                apk.inputStream().use { it.copyTo(out, 64 * 1024) }
                session.fsync(out)
            }
            session.commit(UpdateInstallReceiver.pendingIntent(app).intentSender)
            HostLog.i("update", "update handed to the system installer")
            InstallStart.Committed
        } catch (e: Exception) {
            HostLog.w("update", "writing the install session failed", e)
            session.abandon()
            InstallStart.Failed(if (e is java.io.IOException) UpdateFailure.INSTALL_STORAGE else UpdateFailure.INSTALL_FAILED)
        } finally {
            session.close()
        }
    }
}

/** Carries the installer's answers from [UpdateInstallReceiver] to the screen. */
internal object UpdateInstallBus {
    private val mutableResults = MutableSharedFlow<InstallResult>(extraBufferCapacity = 4)

    /** Every answer, for the update controller. */
    val results: SharedFlow<InstallResult> = mutableResults

    /** Android's confirmation screen, waiting for `MainActivity` to be in front to show it. */
    val confirm = MutableStateFlow<Intent?>(null)

    fun publish(result: InstallResult) {
        mutableResults.tryEmit(result)
    }
}

/**
 * Receives the install session's status. Not exported: only the explicit PendingIntent of
 * [PackageInstallerUpdater] reaches it. The PendingIntent is mutable because Android fills in
 * the status extras; being explicit, it cannot be redirected.
 */
class UpdateInstallReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        val status = intent.getIntExtra(PackageInstaller.EXTRA_STATUS, InstallStatus.FAILURE)
        val legacy = intent.getIntExtra(EXTRA_LEGACY_STATUS, 0)
        HostLog.i("update", "installer status $status")
        if (status == InstallStatus.PENDING_USER_ACTION) {
            UpdateInstallBus.confirm.value = IntentCompat.getParcelableExtra(intent, Intent.EXTRA_INTENT, Intent::class.java)
        }
        UpdateInstallBus.publish(InstallStatus.of(status, legacy))
    }

    internal companion object {
        /** `PackageInstaller.EXTRA_LEGACY_STATUS`, hidden in the SDK but sent by every release since 5.0. */
        private const val EXTRA_LEGACY_STATUS = "android.content.pm.extra.LEGACY_STATUS"
        private const val REQUEST_CODE = 0x5044

        fun pendingIntent(context: Context): PendingIntent {
            val intent = Intent(context, UpdateInstallReceiver::class.java).setPackage(context.packageName)
            val mutable = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) PendingIntent.FLAG_MUTABLE else 0
            return PendingIntent.getBroadcast(context, REQUEST_CODE, intent, PendingIntent.FLAG_UPDATE_CURRENT or mutable)
        }
    }
}

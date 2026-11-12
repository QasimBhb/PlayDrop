package org.playdrop.android.share

import android.app.AlertDialog
import android.content.ClipDescription
import android.content.ClipboardManager
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.widget.Toast
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.lifecycle.lifecycleScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.suspendCancellableCoroutine
import kotlinx.coroutines.withContext
import org.playdrop.android.R
import org.playdrop.android.core.CoreHost
import org.playdrop.android.core.CoreHostImpl
import org.playdrop.android.core.HostLog
import org.playdrop.android.data.Settings
import org.playdrop.android.service.ClipboardApplier
import kotlin.coroutines.resume

/**
 * The "Send clipboard" notification action. Android 10+ lets only the focused app read the
 * clipboard, so this activity takes focus and reads the clip in [onWindowFocusChanged].
 *
 * Only devices that would receive it count (online, "Receive this phone's clipboard" on and,
 * for an image, able to take one). One such device: sent at once. More than one: the device
 * chooser, the others listed greyed out with their reason. None: the plain reason. The toast
 * names the devices. A clip marked sensitive asks once first and goes to every chosen device
 * with `sensitive = true` (SR-24). The clip is never logged (SR-32). Not exported.
 */
class ClipboardSendActivity : ComponentActivity() {
    private var handled = false
    private val sheet = SendSheet(onMakeDefault = { id -> storeDefaultDevice(id) })

    /** A recreation after the clip was read ends quietly, so nothing is sent twice. */
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        handled = savedInstanceState?.getBoolean(KEY_HANDLED) ?: false
        if (handled) {
            finish()
            return
        }
        setContent { SendSheetContent(sheet) }
    }

    /** Saves the handled flag. */
    override fun onSaveInstanceState(outState: Bundle) {
        super.onSaveInstanceState(outState)
        outState.putBoolean(KEY_HANDLED, handled)
    }

    /** The chooser left the screen before "Send": nothing is sent. */
    override fun onStop() {
        super.onStop()
        if (sheet.state != SheetState.Hidden && !isChangingConfigurations && !isFinishing) {
            sheet.leave()
            finish()
        }
    }

    /** Reads the clipboard the first time the window has focus. */
    override fun onWindowFocusChanged(hasFocus: Boolean) {
        super.onWindowFocusChanged(hasFocus)
        if (!hasFocus || handled) return
        handled = true
        val cm = getSystemService(ClipboardManager::class.java)
        val clip = cm.primaryClip
        if (clip == null || clip.itemCount == 0) return done(getString(R.string.toast_nothing_to_send))
        val desc = clip.description
        val sensitive = desc.extras?.let { extras ->
            extras.getBoolean(ClipboardApplier.EXTRA_IS_SENSITIVE, false) ||
                (Build.VERSION.SDK_INT >= 33 && extras.getBoolean(ClipDescription.EXTRA_IS_SENSITIVE, false))
        } ?: false
        val item = clip.getItemAt(0)
        val imageUri = item.uri?.takeIf { desc.mimeTypeCount > 0 && desc.getMimeType(0).startsWith("image/") }
        // The item's own text only: a link to other content is never opened on the main thread
        // (no size limit there); an image is read later, off the main thread, by ClipImage.
        val text = if (imageUri == null) item.text?.toString()?.takeIf { it.isNotEmpty() } else null
        if (imageUri == null && text == null) return done(getString(R.string.toast_nothing_to_send))
        val mime = imageUri?.let { desc.getMimeType(0) }
        lifecycleScope.launch { run(text, imageUri, mime, sensitive) }
    }

    private suspend fun run(text: String?, imageUri: Uri?, mime: String?, sensitive: Boolean) {
        // SR-24: ask once, before any device is chosen, not once per device.
        if (sensitive && !confirmSensitive()) return done(getString(R.string.toast_cancelled))
        if (!CoreHost.awaitRunning(this)) return done(getString(R.string.toast_not_running))
        val clip = try {
            if (imageUri != null) ShareSender.Clip.Image(readImage(imageUri), mime ?: "image/png") else ShareSender.Clip.Text(text.orEmpty())
        } catch (e: Exception) {
            HostLog.w("clipboard", "reading the copied image failed", e)
            return done(getString(R.string.toast_send_failed, ShareSender.reason(e)))
        }
        val kind = if (clip is ShareSender.Clip.Image) SendKind.CLIPBOARD_IMAGE else SendKind.CLIPBOARD_TEXT
        val peers = CoreHost.peers.value
        val defaultId = Settings.read(this).defaultPeer?.takeIf { id -> peers.any { it.deviceId == id } }
        val chosen = when (val launch = Launch.forClipboard(peers.map(Candidate::of), defaultId, kind)) {
            Launch.NoDevices -> return done(getString(R.string.toast_no_paired_devices))
            is Launch.NoneEligible -> return done(noneText(launch.reason))
            is Launch.Direct -> listOf(launch.id)
            is Launch.Choose -> sheet.choose(launch.selection, kind, 1) ?: return done(getString(R.string.toast_cancelled))
        }
        val app = applicationContext
        CoreHostImpl.scope.launch {
            val results = ShareSender.deliverClip(chosen, clip, sensitive, dialFirst = false)
            ShareSender.toast(app, SendReport.sentence(results, SharePhrases.clipboard(app)))
        }
        finish()
    }

    /** The sensitive-clip question (SR-24); true for Send. */
    private suspend fun confirmSensitive(): Boolean = suspendCancellableCoroutine { cont ->
        val dialog = AlertDialog.Builder(this)
            .setTitle(R.string.sensitive_title)
            .setMessage(R.string.sensitive_text)
            .setPositiveButton(R.string.sensitive_send) { _, _ -> if (cont.isActive) cont.resume(true) }
            .setNegativeButton(R.string.cancel) { _, _ -> if (cont.isActive) cont.resume(false) }
            .setOnCancelListener { if (cont.isActive) cont.resume(false) }
            .show()
        cont.invokeOnCancellation { dialog.dismiss() }
    }

    private suspend fun readImage(uri: Uri): ByteArray = withContext(Dispatchers.IO) { ClipImage.read(contentResolver, uri) }

    private fun noneText(outcome: ClipboardOutcome): String = when (outcome) {
        is ClipboardOutcome.Sent, ClipboardOutcome.AlreadySent -> getString(R.string.toast_clipboard_unchanged)
        ClipboardOutcome.NoPairedDevice -> getString(R.string.toast_no_paired_devices)
        ClipboardOutcome.NoGrant -> getString(R.string.toast_no_clipboard_grant)
        ClipboardOutcome.NoDeviceOnline -> getString(R.string.toast_no_device_online)
        ClipboardOutcome.NoImageSupport -> getString(R.string.share_no_image_device)
    }

    private fun done(message: String) {
        Toast.makeText(applicationContext, message, Toast.LENGTH_SHORT).show()
        finish()
    }

    private companion object {
        const val KEY_HANDLED = "handled"
    }
}

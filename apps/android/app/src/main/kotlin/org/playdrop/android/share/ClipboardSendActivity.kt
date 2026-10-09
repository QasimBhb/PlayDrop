package org.playdrop.android.share

import android.app.AlertDialog
import android.content.ClipDescription
import android.content.ClipboardManager
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.widget.Toast
import androidx.activity.ComponentActivity
import androidx.lifecycle.lifecycleScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import org.playdrop.android.R
import org.playdrop.android.core.CoreHost
import org.playdrop.android.core.HostLog
import org.playdrop.android.service.ClipboardApplier
import org.playdrop.core.ClipboardPush
import java.io.ByteArrayOutputStream

/**
 * The "Send clipboard" notification action. Android 10+ lets only the focused app read the
 * clipboard, so this invisible activity takes focus, reads the clip in
 * [onWindowFocusChanged], pushes it to every eligible device, shows the result as a toast
 * and finishes. A clip marked sensitive asks first and goes out with `sensitive = true`
 * (SR-24). Not exported.
 */
class ClipboardSendActivity : ComponentActivity() {
    private var handled = false

    /** Restores the handled flag so a rotation does not send twice. */
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        handled = savedInstanceState?.getBoolean(KEY_HANDLED) ?: false
    }

    /** Saves the handled flag. */
    override fun onSaveInstanceState(outState: Bundle) {
        super.onSaveInstanceState(outState)
        outState.putBoolean(KEY_HANDLED, handled)
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
        val text = if (imageUri == null) item.coerceToText(this)?.toString()?.takeIf { it.isNotEmpty() } else null
        if (imageUri == null && text == null) return done(getString(R.string.toast_nothing_to_send))
        val send = { push(text, imageUri, imageUri?.let { desc.getMimeType(0) }, sensitive) }
        if (sensitive) confirmSensitive(send) else send()
    }

    private fun confirmSensitive(send: () -> Unit) {
        AlertDialog.Builder(this)
            .setTitle(R.string.sensitive_title)
            .setMessage(R.string.sensitive_text)
            .setPositiveButton(R.string.sensitive_send) { _, _ -> send() }
            .setNegativeButton(R.string.cancel) { _, _ -> done(getString(R.string.toast_cancelled)) }
            .setOnCancelListener { done(getString(R.string.toast_cancelled)) }
            .show()
    }

    private fun push(text: String?, imageUri: Uri?, mime: String?, sensitive: Boolean) {
        lifecycleScope.launch {
            if (!CoreHost.awaitRunning(this@ClipboardSendActivity)) return@launch done(getString(R.string.toast_not_running))
            val message = try {
                val bytes = imageUri?.let { readImage(it) }
                val result: ClipboardPush = CoreHost.withHost { h ->
                    if (bytes != null) h.pushClipboardImage(bytes, mime ?: "image/png", sensitive) else h.pushClipboard(text.orEmpty(), sensitive)
                }
                outcomeText(result)
            } catch (e: Exception) {
                HostLog.w("clipboard", "manual clipboard push failed", e)
                getString(R.string.toast_send_failed, ShareSender.reason(e))
            }
            done(message)
        }
    }

    private suspend fun readImage(uri: Uri): ByteArray = withContext(Dispatchers.IO) {
        contentResolver.openInputStream(uri)?.use { input ->
            val out = ByteArrayOutputStream()
            val buf = ByteArray(64 * 1024)
            var total = 0
            while (true) {
                val n = input.read(buf)
                if (n < 0) break
                total += n
                if (total > MAX_IMAGE_BYTES) throw IllegalArgumentException("the image is larger than 16 MiB")
                out.write(buf, 0, n)
            }
            out.toByteArray()
        } ?: throw IllegalArgumentException("the copied image could not be read")
    }

    private fun outcomeText(p: ClipboardPush): String {
        val outcome = ClipboardOutcome.of(
            sentTo = p.sentTo.size,
            skippedEcho = p.skippedEcho,
            skippedOffline = p.skippedOffline.toInt(),
            skippedNoGrant = p.skippedNoGrant.toInt(),
            pairedCount = CoreHost.peers.value.size,
        )
        return when (outcome) {
            is ClipboardOutcome.Sent -> resources.getQuantityString(R.plurals.toast_sent_to, outcome.count, outcome.count)
            ClipboardOutcome.AlreadySent -> getString(R.string.toast_clipboard_unchanged)
            ClipboardOutcome.NoPairedDevice -> getString(R.string.toast_no_paired_devices)
            ClipboardOutcome.NoGrant -> getString(R.string.toast_no_clipboard_grant)
            ClipboardOutcome.NoDeviceOnline -> getString(R.string.toast_no_device_online)
        }
    }

    private fun done(message: String) {
        Toast.makeText(applicationContext, message, Toast.LENGTH_SHORT).show()
        finish()
    }

    private companion object {
        const val KEY_HANDLED = "handled"
        const val MAX_IMAGE_BYTES = 16 * 1024 * 1024
    }
}

package org.playdrop.android.share

import android.content.Intent
import android.net.Uri
import android.os.Bundle
import android.widget.Toast
import androidx.activity.ComponentActivity
import androidx.activity.result.PickVisualMediaRequest
import androidx.activity.result.contract.ActivityResultContracts
import androidx.lifecycle.lifecycleScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import org.playdrop.android.R
import org.playdrop.android.core.CoreHost
import org.playdrop.android.core.HostLog
import org.playdrop.android.core.UriSource
import org.playdrop.android.data.Settings

/**
 * The "Send to <default>" notification action: opens the photo picker (or the document
 * picker where the photo picker is unavailable), sends the picks to the default device and
 * finishes. Without a default device it opens the app instead. Not exported.
 */
class PickAndSendActivity : ComponentActivity() {
    private var peer: String? = null

    private val pickMedia = registerForActivityResult(ActivityResultContracts.PickMultipleVisualMedia()) { onPicked(it) }
    private val pickDocuments = registerForActivityResult(ActivityResultContracts.OpenMultipleDocuments()) { onPicked(it) }

    /** Resolves the default device and launches a picker (once; not again after rotation). */
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        peer = savedInstanceState?.getString(KEY_PEER)
        if (savedInstanceState != null) return
        lifecycleScope.launch {
            val default = Settings.read(this@PickAndSendActivity).defaultPeer
            if (default == null) {
                packageManager.getLaunchIntentForPackage(packageName)?.let {
                    startActivity(it.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
                }
                finish()
                return@launch
            }
            peer = default
            if (ActivityResultContracts.PickVisualMedia.isPhotoPickerAvailable(this@PickAndSendActivity)) {
                pickMedia.launch(PickVisualMediaRequest(ActivityResultContracts.PickVisualMedia.ImageAndVideo))
            } else {
                pickDocuments.launch(arrayOf("*/*"))
            }
        }
    }

    /** Keeps the chosen peer across a recreation while the picker is open. */
    override fun onSaveInstanceState(outState: Bundle) {
        super.onSaveInstanceState(outState)
        outState.putString(KEY_PEER, peer)
    }

    private fun onPicked(uris: List<Uri>) {
        val target = peer
        if (uris.isEmpty() || target == null) {
            finish()
            return
        }
        lifecycleScope.launch {
            if (!CoreHost.awaitRunning(this@PickAndSendActivity)) {
                toast(getString(R.string.toast_not_running))
                finish()
                return@launch
            }
            try {
                // Open the items while this activity still holds the picker's read grant.
                val prepared = withContext(Dispatchers.IO) { UriSource.prepare(this@PickAndSendActivity, uris) }
                ShareSender.sendFiles(this@PickAndSendActivity, target, prepared)
            } catch (e: Exception) {
                HostLog.w("pick", "preparing picked items failed", e)
                toast(getString(R.string.toast_send_failed, ShareSender.reason(e)))
            }
            finish()
        }
    }

    private fun toast(text: String) = Toast.makeText(applicationContext, text, Toast.LENGTH_LONG).show()

    private companion object {
        const val KEY_PEER = "peer"
    }
}

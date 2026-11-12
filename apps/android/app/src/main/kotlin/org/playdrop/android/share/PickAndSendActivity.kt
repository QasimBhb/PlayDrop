package org.playdrop.android.share

import android.content.Intent
import android.net.Uri
import android.os.Bundle
import android.widget.Toast
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.result.PickVisualMediaRequest
import androidx.activity.result.contract.ActivityResultContracts
import androidx.activity.viewModels
import androidx.lifecycle.lifecycleScope
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.launch
import org.playdrop.android.R
import org.playdrop.android.core.CoreHost
import org.playdrop.android.core.HostLog
import org.playdrop.android.core.UriSource
import org.playdrop.android.data.Settings

/**
 * The "Send to <device>" or "Send files" notification action: opens the photo picker (or the document
 * picker where the photo picker is unavailable). With one usable paired device the picks go
 * to it (the pick is the confirmation, as before); with more than one the device chooser
 * follows, the same one as the share sheet's, and "Send" hands the sends to the process
 * scope ([ManySend]). Without a usable paired device it opens the app. The picked items are
 * kept across a rotation ([SendFlowModel]). Not exported.
 */
class PickAndSendActivity : ComponentActivity() {
    private val sheet = SendSheet(onMakeDefault = { id -> storeDefaultDevice(id) })
    private val model: SendFlowModel by viewModels()

    /** True once the picker returned items, so a recreation does not open it again. */
    private var picked = false

    private val pickMedia = registerForActivityResult(ActivityResultContracts.PickMultipleVisualMedia()) { onPicked(it) }
    private val pickDocuments = registerForActivityResult(ActivityResultContracts.OpenMultipleDocuments()) { onPicked(it) }

    /** Launches a picker once; after a recreation goes on where the earlier instance was. */
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContent { SendSheetContent(sheet) }
        model.runToken?.let { token ->
            picked = true
            val run = ManySends.find(token) ?: return finish()
            lifecycleScope.launch { holdSendingCard(sheet, run) }
            return
        }
        if (model.handedOver) {
            finish()
            return
        }
        model.picked?.let { uris ->
            // Recreated (a rotation) after the pick: go on with the same items.
            picked = true
            sheet.show(SheetState.Preparing)
            lifecycleScope.launch { send(uris) }
            return
        }
        if (savedInstanceState?.getBoolean(KEY_PICKED) == true) {
            // The process was restarted after the pick: the picked items are gone with it.
            toast(getString(R.string.toast_pick_again))
            finish()
            return
        }
        // Recreated while the picker is open: its result arrives at this instance.
        if (savedInstanceState != null) return
        lifecycleScope.launch {
            if (!CoreHost.awaitRunning(this@PickAndSendActivity)) {
                toast(getString(R.string.toast_not_running))
                finish()
                return@launch
            }
            if (CoreHost.peers.value.none { !it.removedByPeer }) {
                packageManager.getLaunchIntentForPackage(packageName)?.let {
                    startActivity(it.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
                }
                finish()
                return@launch
            }
            if (ActivityResultContracts.PickVisualMedia.isPhotoPickerAvailable(this@PickAndSendActivity)) {
                pickMedia.launch(PickVisualMediaRequest(ActivityResultContracts.PickVisualMedia.ImageAndVideo))
            } else {
                pickDocuments.launch(arrayOf("*/*"))
            }
        }
    }

    /** Remembers that the picker already returned. */
    override fun onSaveInstanceState(outState: Bundle) {
        super.onSaveInstanceState(outState)
        outState.putBoolean(KEY_PICKED, picked)
    }

    /**
     * The card left the screen: before "Send" that cancels, after it the sends go on. While
     * the picker covers this activity the sheet is hidden and nothing happens.
     */
    override fun onStop() {
        super.onStop()
        if (sheet.state != SheetState.Hidden && !isChangingConfigurations && !isFinishing) {
            sheet.leave()
            finish()
        }
    }

    private fun onPicked(uris: List<Uri>) {
        if (uris.isEmpty()) {
            finish()
            return
        }
        if (uris.size > UriSource.MAX_ITEMS) return finishWith(getString(R.string.toast_too_many_items))
        picked = true
        model.picked = uris
        sheet.show(SheetState.Preparing)
        lifecycleScope.launch { send(uris) }
    }

    private suspend fun send(uris: List<Uri>) {
        if (!CoreHost.awaitRunning(this)) return finishWith(getString(R.string.toast_not_running))
        try {
            // Open the items while this activity still holds the picker's read grant; the
            // model owns them until a send takes them, so a rotation loses nothing.
            model.prepare(applicationContext, uris).await()
        } catch (e: CancellationException) {
            throw e
        } catch (e: Exception) {
            HostLog.wNoDetail("pick", "preparing picked items failed", e)
            return finishWith(ShareSender.failedText(this, e))
        }
        val peers = CoreHost.peers.value
        val defaultId = Settings.read(this).defaultPeer?.takeIf { id -> peers.any { it.deviceId == id } }
        when (val launch = Launch.forShare(peers.map(Candidate::of), defaultId, SendKind.FILES)) {
            Launch.NoDevices, is Launch.NoneEligible -> finishWith(getString(R.string.toast_no_paired_devices))
            is Launch.Direct -> {
                model.take()?.let { ShareSender.sendFiles(this, launch.id, it) }
                model.handedOver = true
                finish()
            }
            is Launch.Choose -> {
                val title = { chosen: List<String> ->
                    resources.getQuantityString(R.plurals.share_sending, uris.size, uris.size, joinNames(chosen.map(CoreHost::labelOf)))
                }
                val chosen = sheet.choose(launch.selection, SendKind.FILES, uris.size, title)
                    ?: return finishWith(getString(R.string.toast_cancelled))
                DefaultRule.after(defaultId, chosen)?.let { storeDefaultDevice(it) }
                val prepared = model.take() ?: return finish()
                val run = startFileSends(this, prepared, chosen, title(chosen))
                model.runToken = run.token
                holdSendingCard(sheet, run)
            }
        }
    }

    private fun finishWith(message: String) {
        toast(message)
        finish()
    }

    private fun toast(text: String) = Toast.makeText(applicationContext, text, Toast.LENGTH_LONG).show()

    private companion object {
        const val KEY_PICKED = "picked"
    }
}

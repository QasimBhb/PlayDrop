package org.playdrop.android.share

import android.content.Intent
import android.net.Uri
import android.os.Bundle
import android.widget.Toast
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.viewModels
import androidx.core.content.IntentCompat
import androidx.lifecycle.lifecycleScope
import androidx.lifecycle.withResumed
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.launch
import org.playdrop.android.R
import org.playdrop.android.core.CoreHost
import org.playdrop.android.core.CoreHostImpl
import org.playdrop.android.core.HostLog
import org.playdrop.android.core.UriSource
import org.playdrop.android.data.Settings

/**
 * The share-sheet target and the only exported component besides the launcher (SR-29).
 *
 * One usable paired device: "Sending <n> items to <label>" with Cancel for 2 s, then the send
 * goes to the service and the activity finishes (one tap, as before). More than one: the
 * device chooser, with the default device pre-ticked; nothing leaves the phone before "Send"
 * in it. "Send" turns the card into "Sending to A and B" at once, and the sends are prepared
 * and started in the process scope ([ManySend]): leaving or rotating keeps them going, and
 * Cancel stops every one of them. With no default yet, a send to a single ticked device makes
 * it the default. Shared text goes to the devices' clipboards instead of becoming a file.
 * Only other apps' `content://` items are accepted ([ShareUris]).
 */
class ShareActivity : ComponentActivity() {
    private sealed class Payload {
        data class Files(val uris: List<Uri>) : Payload()
        data class Text(val text: String) : Payload()
    }

    private val sheet = SendSheet(onMakeDefault = ::storeDefault)
    private val model: SendFlowModel by viewModels()

    /** True once "Send" was tapped or the countdown ran out, so a recreation does not offer it again. */
    private var sent = false

    /** Parses the share intent and runs the flow. Runs once per share, not again after a send. */
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContent { SendSheetContent(sheet) }
        // A send to several devices went on while the activity was recreated: show its card.
        model.runToken?.let { token ->
            sent = true
            val run = ManySends.find(token) ?: return finish()
            lifecycleScope.launch { holdSendingCard(sheet, run) }
            return
        }
        // A one-device send already went, or (after the process was restarted) any send did.
        if (model.handedOver || savedInstanceState?.getBoolean(KEY_SENT) == true) {
            sent = true
            finish()
            return
        }
        val payload = when (val parsed = parse(intent)) {
            is Parsed.Ok -> parsed.payload
            is Parsed.Refused -> return finishWith(getString(parsed.message))
        }
        sheet.show(SheetState.Preparing)
        lifecycleScope.launch { run(payload) }
    }

    /** Remembers that the send already went. */
    override fun onSaveInstanceState(outState: Bundle) {
        super.onSaveInstanceState(outState)
        outState.putBoolean(KEY_SENT, sent)
    }

    private suspend fun run(payload: Payload) {
        if (!CoreHost.awaitRunning(this)) return finishWith(getString(R.string.toast_not_running))
        if (payload is Payload.Files) {
            // Open every item now, while the share grant is certainly valid. The model owns the
            // result until a send takes it, so a rotation here loses nothing.
            try {
                model.prepare(applicationContext, payload.uris).await()
            } catch (e: CancellationException) {
                throw e
            } catch (e: Exception) {
                HostLog.wNoDetail("share", "preparing shared items failed", e)
                return finishWith(ShareSender.failedText(this, e))
            }
        }
        val peers = CoreHost.peers.value
        val defaultId = Settings.read(this).defaultPeer?.takeIf { id -> peers.any { it.deviceId == id } }
        val kind = if (payload is Payload.Files) SendKind.FILES else SendKind.SHARED_TEXT
        when (val launch = Launch.forShare(peers.map(Candidate::of), defaultId, kind)) {
            Launch.NoDevices, is Launch.NoneEligible -> finishWith(getString(R.string.toast_no_paired_devices))
            is Launch.Direct -> {
                // SR-29: the card must be on screen for the full 2 s before anything leaves
                // the phone. Wait until the activity is resumed (not behind the lock
                // screen); leaving the resumed state during the countdown cancels.
                withResumed { }
                if (!sheet.countdown(countdownTitle(payload, CoreHost.labelOf(launch.id)))) {
                    return finishWith(getString(R.string.toast_cancelled))
                }
                sent = true
                when (payload) {
                    is Payload.Files -> model.take()?.let { ShareSender.sendFiles(this, launch.id, it) }
                    is Payload.Text -> ShareSender.sendText(this, launch.id, payload.text)
                }
                model.handedOver = true
                finish()
            }
            is Launch.Choose -> {
                val items = (payload as? Payload.Files)?.uris?.size ?: 1
                val title: ((List<String>) -> String)? = if (payload is Payload.Files) {
                    { chosen -> sendingTitle(items, chosen) }
                } else {
                    null
                }
                val chosen = sheet.choose(launch.selection, kind, items, title) ?: return finishWith(getString(R.string.toast_cancelled))
                sent = true
                DefaultRule.after(defaultId, chosen)?.let(::storeDefault)
                when (payload) {
                    is Payload.Files -> {
                        val prepared = model.take() ?: return finish()
                        val run = startFileSends(this, prepared, chosen, sendingTitle(items, chosen))
                        model.runToken = run.token
                        holdSendingCard(sheet, run)
                    }
                    is Payload.Text -> {
                        model.handedOver = true
                        sendTextToMany(chosen, payload.text)
                    }
                }
            }
        }
    }

    /** Text to several clipboards: a clip cannot be called back, so it goes at once. */
    private fun sendTextToMany(chosen: List<String>, text: String) {
        val app = applicationContext
        CoreHostImpl.scope.launch {
            val results = ShareSender.deliverClip(chosen, ShareSender.Clip.Text(text), sensitive = false, dialFirst = true)
            ShareSender.toast(app, SendReport.sentence(results, SharePhrases.sharedText(app)))
        }
        finish()
    }

    private fun sendingTitle(items: Int, chosen: List<String>): String =
        resources.getQuantityString(R.plurals.share_sending, items, items, joinNames(chosen.map(CoreHost::labelOf)))

    private fun countdownTitle(payload: Payload, label: String): String = when (payload) {
        is Payload.Text -> getString(R.string.share_sending_text, label)
        is Payload.Files -> resources.getQuantityString(R.plurals.share_sending, payload.uris.size, payload.uris.size, label)
    }

    /** Leaving the screen during the countdown cancels the send (SR-29). */
    override fun onPause() {
        super.onPause()
        if (sheet.state is SheetState.Countdown) sheet.cancel()
    }

    /**
     * A share card that is no longer on screen is abandoned: finish, so a stale instance
     * (for example one started behind the lock screen) never swallows the next share. Before
     * "Send" that cancels; after it the sends go on.
     */
    override fun onStop() {
        super.onStop()
        if (!isChangingConfigurations && !isFinishing) {
            sheet.leave()
            finish()
        }
    }

    private fun storeDefault(id: String) = storeDefaultDevice(id)

    private sealed class Parsed {
        data class Ok(val payload: Payload) : Parsed()
        data class Refused(val message: Int) : Parsed()
    }

    private fun parse(intent: Intent): Parsed {
        val files = when (intent.action) {
            Intent.ACTION_SEND -> {
                val uri = IntentCompat.getParcelableExtra(intent, Intent.EXTRA_STREAM, Uri::class.java)
                if (uri == null) {
                    val text = intent.getCharSequenceExtra(Intent.EXTRA_TEXT)?.toString()
                    return text?.takeIf { it.isNotEmpty() }?.let { Parsed.Ok(Payload.Text(it)) }
                        ?: Parsed.Refused(R.string.toast_nothing_to_send)
                }
                listOf(uri)
            }
            Intent.ACTION_SEND_MULTIPLE ->
                IntentCompat.getParcelableArrayListExtra(intent, Intent.EXTRA_STREAM, Uri::class.java)?.filterNotNull().orEmpty()
            else -> emptyList()
        }
        if (files.isEmpty()) return Parsed.Refused(R.string.toast_nothing_to_send)
        return when (ShareUris.refusal(files, ShareUris.ownAuthorities(this))) {
            null -> Parsed.Ok(Payload.Files(files))
            ShareUris.Refusal.TOO_MANY -> Parsed.Refused(R.string.toast_too_many_items)
            ShareUris.Refusal.NOT_ALLOWED -> Parsed.Refused(R.string.toast_share_not_allowed)
        }
    }

    private fun finishWith(message: String) {
        toast(message)
        finish()
    }

    private fun toast(text: String) = Toast.makeText(applicationContext, text, Toast.LENGTH_LONG).show()

    private companion object {
        const val KEY_SENT = "sent"
    }
}

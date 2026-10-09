package org.playdrop.android.share

import android.content.Intent
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.widget.Toast
import androidx.activity.ComponentActivity
import androidx.activity.compose.BackHandler
import androidx.activity.compose.setContent
import androidx.compose.animation.core.LinearEasing
import androidx.compose.animation.core.animateFloatAsState
import androidx.compose.animation.core.tween
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.Card
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.dynamicDarkColorScheme
import androidx.compose.material3.dynamicLightColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import androidx.core.content.IntentCompat
import androidx.lifecycle.lifecycleScope
import androidx.lifecycle.withResumed
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeoutOrNull
import org.playdrop.android.R
import org.playdrop.android.core.CoreHost
import org.playdrop.android.core.HostLog
import org.playdrop.android.core.UriSource
import org.playdrop.android.data.Settings
import org.playdrop.core.PeerSummary

/**
 * The share-sheet target and the only exported component besides the launcher (SR-29).
 *
 * One tap: with a default device it shows "Sending <n> items to <label>" with Cancel for
 * 2 s, then hands the send to the service and finishes. Without a default it lists the paired
 * devices (name and Short ID, SR-8); the pick becomes the default and is sent at once (the
 * pick itself is the confirmation). Shared text goes to the device's clipboard instead of
 * becoming a file.
 */
class ShareActivity : ComponentActivity() {
    private sealed class Payload {
        data class Files(val uris: List<Uri>) : Payload()
        data class Text(val text: String) : Payload()
    }

    private sealed class Ui {
        data object Preparing : Ui()
        data class Pick(val peers: List<PeerSummary>) : Ui()
        data class Countdown(val label: String, val count: Int, val text: Boolean) : Ui()
    }

    private var ui by mutableStateOf<Ui>(Ui.Preparing)

    /** False once the activity left the resumed state during the countdown. */
    private var visibleThroughout = true
    private val picked = CompletableDeferred<String?>()
    private val cancelled = CompletableDeferred<Unit>()

    /** Parses the share intent and runs the flow. Runs once per share, not on rotation. */
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        val payload = parse(intent)
        if (payload == null) {
            toast(getString(R.string.toast_nothing_to_send))
            finish()
            return
        }
        setContent { ShareTheme { ShareScreen() } }
        lifecycleScope.launch { run(payload) }
    }

    private suspend fun run(payload: Payload) {
        var prepared: UriSource.PreparedSend? = null
        var handedOver = false
        try {
            if (!CoreHost.awaitRunning(this)) return finishWith(getString(R.string.toast_not_running))
            if (payload is Payload.Files) {
                prepared = try {
                    // Open every item now, while the share grant is certainly valid.
                    withContext(Dispatchers.IO) { UriSource.prepare(this@ShareActivity, payload.uris) }
                } catch (e: Exception) {
                    HostLog.w("share", "preparing shared items failed", e)
                    return finishWith(getString(R.string.toast_send_failed, ShareSender.reason(e)))
                }
            }
            val peers = CoreHost.peers.value
            if (peers.isEmpty()) return finishWith(getString(R.string.toast_no_paired_devices))
            val default = Settings.read(this).defaultPeer?.takeIf { id -> peers.any { it.deviceId == id } }
            val peer: String
            if (default != null) {
                peer = default
                // SR-29: the card must be on screen for the full 2 s before anything leaves the
                // phone. Wait until the activity is resumed (not behind the lock screen), and
                // treat leaving the resumed state during the countdown as Cancel.
                withResumed { }
                ui = Ui.Countdown(CoreHost.labelOf(peer), (payload as? Payload.Files)?.uris?.size ?: 1, payload is Payload.Text)
                if (withTimeoutOrNull(CANCEL_WINDOW_MS) { cancelled.await() } != null || !visibleThroughout) {
                    return finishWith(getString(R.string.toast_cancelled))
                }
            } else {
                ui = Ui.Pick(peers.sortedByDescending { it.online })
                peer = picked.await() ?: return finishWith(getString(R.string.toast_cancelled))
                Settings.update(this) { it.copy(defaultPeer = peer) }
            }
            when (payload) {
                is Payload.Files -> ShareSender.sendFiles(this, peer, prepared!!)
                is Payload.Text -> ShareSender.sendText(this, peer, payload.text)
            }
            handedOver = true
            finish()
        } finally {
            if (!handedOver) prepared?.release()
        }
    }

    /** Leaving the screen during the countdown cancels the send (SR-29). */
    override fun onPause() {
        super.onPause()
        if (ui is Ui.Countdown) {
            visibleThroughout = false
            cancelled.complete(Unit)
        }
    }

    /**
     * A share card that is no longer on screen is abandoned: finish, so a stale instance
     * (for example one started behind the lock screen) never swallows the next share.
     */
    override fun onStop() {
        super.onStop()
        if (!isChangingConfigurations && !isFinishing) {
            cancel()
            finish()
        }
    }

    private fun parse(intent: Intent): Payload? {
        return when (intent.action) {
            Intent.ACTION_SEND -> {
                val uri = IntentCompat.getParcelableExtra(intent, Intent.EXTRA_STREAM, Uri::class.java)
                if (uri != null) return Payload.Files(listOf(uri))
                val text = intent.getCharSequenceExtra(Intent.EXTRA_TEXT)?.toString()
                text?.takeIf { it.isNotEmpty() }?.let { Payload.Text(it) }
            }
            Intent.ACTION_SEND_MULTIPLE -> {
                val uris = IntentCompat.getParcelableArrayListExtra(intent, Intent.EXTRA_STREAM, Uri::class.java)
                uris?.filterNotNull()?.takeIf { it.isNotEmpty() }?.let { Payload.Files(it) }
            }
            else -> null
        }
    }

    private fun finishWith(message: String) {
        toast(message)
        finish()
    }

    private fun toast(text: String) = Toast.makeText(applicationContext, text, Toast.LENGTH_LONG).show()

    private fun cancel() {
        cancelled.complete(Unit)
        picked.complete(null)
    }

    @Composable
    private fun ShareScreen() {
        BackHandler { cancel() }
        Box(
            Modifier
                .fillMaxSize()
                .background(Color.Black.copy(alpha = 0.32f))
                .clickable(onClick = ::cancel),
            contentAlignment = Alignment.BottomCenter,
        ) {
            Card(
                Modifier
                    .fillMaxWidth()
                    .navigationBarsPadding()
                    // A Material3 card consumes its own taps, so only the scrim cancels.
                    .padding(16.dp),
            ) {
                when (val state = ui) {
                    Ui.Preparing -> Row(Modifier.padding(24.dp), verticalAlignment = Alignment.CenterVertically) {
                        CircularProgressIndicator(Modifier.size(24.dp))
                        Spacer(Modifier.padding(8.dp))
                        Text(stringResource(R.string.share_preparing))
                    }
                    is Ui.Countdown -> CountdownCard(state)
                    is Ui.Pick -> PickCard(state.peers)
                }
            }
        }
    }

    @Composable
    private fun CountdownCard(state: Ui.Countdown) {
        var started by remember { mutableStateOf(false) }
        val progress by animateFloatAsState(
            targetValue = if (started) 1f else 0f,
            animationSpec = tween(CANCEL_WINDOW_MS.toInt(), easing = LinearEasing),
            label = "countdown",
        )
        LaunchedEffect(Unit) { started = true }
        Column(Modifier.padding(20.dp)) {
            Text(
                if (state.text) {
                    stringResource(R.string.share_sending_text, state.label)
                } else {
                    pluralStringResource(R.plurals.share_sending, state.count, state.count, state.label)
                },
                style = MaterialTheme.typography.titleMedium,
            )
            Spacer(Modifier.height(16.dp))
            LinearProgressIndicator(progress = { progress }, modifier = Modifier.fillMaxWidth())
            Spacer(Modifier.height(8.dp))
            Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.End) {
                TextButton(onClick = ::cancel) { Text(stringResource(R.string.cancel)) }
            }
        }
    }

    @Composable
    private fun PickCard(peers: List<PeerSummary>) {
        Column(Modifier.padding(vertical = 16.dp)) {
            Text(
                stringResource(R.string.share_pick_device),
                style = MaterialTheme.typography.titleMedium,
                modifier = Modifier.padding(horizontal = 20.dp),
            )
            Text(
                stringResource(R.string.share_pick_hint),
                style = MaterialTheme.typography.bodySmall,
                modifier = Modifier.padding(horizontal = 20.dp, vertical = 4.dp),
            )
            LazyColumn(Modifier.heightIn(max = 360.dp)) {
                items(peers, key = { it.deviceId }) { p ->
                    Column(
                        Modifier
                            .fillMaxWidth()
                            .clickable { picked.complete(p.deviceId) }
                            .padding(horizontal = 20.dp, vertical = 12.dp),
                    ) {
                        Text(p.label.ifBlank { p.shortId }, style = MaterialTheme.typography.bodyLarge)
                        val offline = if (p.online) "" else " - " + stringResource(R.string.share_offline)
                        Text(p.shortId + offline, style = MaterialTheme.typography.bodySmall)
                    }
                }
            }
            Row(Modifier.fillMaxWidth().padding(horizontal = 12.dp), horizontalArrangement = Arrangement.End) {
                TextButton(onClick = ::cancel) { Text(stringResource(R.string.cancel)) }
            }
        }
    }

    @Composable
    private fun ShareTheme(content: @Composable () -> Unit) {
        val dark = isSystemInDarkTheme()
        val ctx = LocalContext.current
        val scheme = when {
            Build.VERSION.SDK_INT >= Build.VERSION_CODES.S -> if (dark) dynamicDarkColorScheme(ctx) else dynamicLightColorScheme(ctx)
            dark -> darkColorScheme()
            else -> lightColorScheme()
        }
        MaterialTheme(colorScheme = scheme, content = content)
    }

    private companion object {
        const val CANCEL_WINDOW_MS = 2_000L
    }
}

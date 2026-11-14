package org.playdrop.android.ui.device

import android.os.SystemClock
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.VolumeDown
import androidx.compose.material.icons.automirrored.filled.VolumeOff
import androidx.compose.material.icons.automirrored.filled.VolumeUp
import androidx.compose.material3.Card
import androidx.compose.material3.Icon
import androidx.compose.material3.IconToggleButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Slider
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.delay
import org.playdrop.android.R
import org.playdrop.core.MediaState
import kotlin.math.roundToInt

/**
 * The computer's master volume (the reserved `@system` entry, see
 * [org.playdrop.android.core.MediaPlayers.SYSTEM_VOLUME_ID]). [onSet] sends a volume 0 to 100.
 * Dragging follows [VolumeSlider]: throttled sends, the finger's value while held, and the
 * released value until the computer echoes it. When the desktop offers mute ([MuteToggle.canMute])
 * a button next to the slider mutes it; [onMute] sends true to mute and false to unmute. The
 * slider keeps showing the real level while muted, and a level above 0 unmutes the computer.
 */
@Composable
fun SystemVolumeCard(state: MediaState, onSet: (Long) -> Unit, onMute: (Boolean) -> Unit = {}) {
    val incoming = (state.volume?.toInt() ?: 0).coerceIn(0, 100)
    var slider by remember { mutableStateOf(VolumeSlider()) }
    val send by rememberUpdatedState(onSet)
    val sendMute by rememberUpdatedState(onMute)
    val canMute = MuteToggle.canMute(state)
    val incomingMuted = MuteToggle.reportedMuted(state)
    var mute by remember { mutableStateOf(MuteToggle()) }
    val sendVolume = { v: Int ->
        send(v.toLong())
        // The computer unmutes itself on a level above 0.
        if (canMute && v > 0) mute = mute.unmutedByVolume(incomingMuted, SystemClock.uptimeMillis())
    }

    LaunchedEffect(incoming) { slider = slider.incoming(incoming) }
    LaunchedEffect(incomingMuted) { mute = mute.incoming(incomingMuted) }
    val muteHoldUntil = mute.holdUntilMs()
    LaunchedEffect(muteHoldUntil) {
        if (muteHoldUntil == null) return@LaunchedEffect
        delay((muteHoldUntil - SystemClock.uptimeMillis()).coerceAtLeast(0))
        mute = mute.expire(SystemClock.uptimeMillis())
    }
    // A value the throttle held back goes out when its interval ends, even if the finger rests.
    val flushAt = slider.flushAtMs()
    LaunchedEffect(flushAt) {
        if (flushAt == null) return@LaunchedEffect
        delay((flushAt - SystemClock.uptimeMillis()).coerceAtLeast(0))
        val step = slider.flush(SystemClock.uptimeMillis())
        slider = step.state
        step.send?.let(sendVolume)
    }
    // Without an echo (refused, or the computer changed nothing), go back to the reported value.
    val holdUntil = slider.holdUntilMs()
    LaunchedEffect(holdUntil) {
        if (holdUntil == null) return@LaunchedEffect
        delay((holdUntil - SystemClock.uptimeMillis()).coerceAtLeast(0))
        slider = slider.expire(SystemClock.uptimeMillis())
    }

    val shown = slider.shown(incoming)
    val muted = canMute && mute.shown(incomingMuted)
    val label = stringResource(R.string.ui_media_system_volume)
    val percent = stringResource(R.string.ui_media_volume_percent, shown)
    val mutedText = stringResource(R.string.ui_media_muted)
    Card(Modifier.fillMaxWidth()) {
        Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Icon(
                    when {
                        muted || shown == 0 -> Icons.AutoMirrored.Filled.VolumeOff
                        shown < HALF -> Icons.AutoMirrored.Filled.VolumeDown
                        else -> Icons.AutoMirrored.Filled.VolumeUp
                    },
                    // The label next to it names the control.
                    contentDescription = null,
                    tint = MaterialTheme.colorScheme.primary,
                )
                Text(
                    label,
                    style = MaterialTheme.typography.labelLarge,
                    color = MaterialTheme.colorScheme.primary,
                    modifier = Modifier.padding(start = 8.dp).weight(1f),
                )
                Text(percent, style = MaterialTheme.typography.labelLarge)
                if (canMute) {
                    val action = stringResource(if (muted) R.string.ui_media_unmute_computer else R.string.ui_media_mute_computer)
                    IconToggleButton(
                        checked = muted,
                        onCheckedChange = {
                            val step = mute.tap(incomingMuted, SystemClock.uptimeMillis())
                            mute = step.state
                            step.send?.let(sendMute)
                        },
                        modifier = Modifier.semantics {
                            contentDescription = action
                            if (muted) stateDescription = mutedText
                        },
                    ) {
                        Icon(
                            if (muted) Icons.AutoMirrored.Filled.VolumeOff else Icons.AutoMirrored.Filled.VolumeUp,
                            contentDescription = null,
                            tint = if (muted) MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                }
            }
            Slider(
                value = shown.toFloat(),
                onValueChange = { v ->
                    val step = slider.drag(v.roundToInt(), SystemClock.uptimeMillis())
                    slider = step.state
                    step.send?.let(sendVolume)
                },
                onValueChangeFinished = {
                    val step = slider.release(SystemClock.uptimeMillis())
                    slider = step.state
                    step.send?.let(sendVolume)
                },
                valueRange = 0f..100f,
                modifier = Modifier.alpha(if (muted) MUTED_ALPHA else 1f).semantics {
                    contentDescription = label
                    stateDescription = if (muted) "$percent, $mutedText" else percent
                },
            )
        }
    }
}

private const val HALF = 50
private const val MUTED_ALPHA = 0.5f

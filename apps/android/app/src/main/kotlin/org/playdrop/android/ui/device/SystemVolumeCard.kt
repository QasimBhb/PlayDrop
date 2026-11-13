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
 * released value until the computer echoes it.
 */
@Composable
fun SystemVolumeCard(state: MediaState, onSet: (Long) -> Unit) {
    val incoming = (state.volume?.toInt() ?: 0).coerceIn(0, 100)
    var slider by remember { mutableStateOf(VolumeSlider()) }
    val send by rememberUpdatedState(onSet)

    LaunchedEffect(incoming) { slider = slider.incoming(incoming) }
    // A value the throttle held back goes out when its interval ends, even if the finger rests.
    val flushAt = slider.flushAtMs()
    LaunchedEffect(flushAt) {
        if (flushAt == null) return@LaunchedEffect
        delay((flushAt - SystemClock.uptimeMillis()).coerceAtLeast(0))
        val step = slider.flush(SystemClock.uptimeMillis())
        slider = step.state
        step.send?.let { send(it.toLong()) }
    }
    // Without an echo (refused, or the computer changed nothing), go back to the reported value.
    val holdUntil = slider.holdUntilMs()
    LaunchedEffect(holdUntil) {
        if (holdUntil == null) return@LaunchedEffect
        delay((holdUntil - SystemClock.uptimeMillis()).coerceAtLeast(0))
        slider = slider.expire(SystemClock.uptimeMillis())
    }

    val shown = slider.shown(incoming)
    val label = stringResource(R.string.ui_media_system_volume)
    val percent = stringResource(R.string.ui_media_volume_percent, shown)
    Card(Modifier.fillMaxWidth()) {
        Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Icon(
                    when {
                        shown == 0 -> Icons.AutoMirrored.Filled.VolumeOff
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
            }
            Slider(
                value = shown.toFloat(),
                onValueChange = { v ->
                    val step = slider.drag(v.roundToInt(), SystemClock.uptimeMillis())
                    slider = step.state
                    step.send?.let { send(it.toLong()) }
                },
                onValueChangeFinished = {
                    val step = slider.release(SystemClock.uptimeMillis())
                    slider = step.state
                    step.send?.let { send(it.toLong()) }
                },
                valueRange = 0f..100f,
                modifier = Modifier.semantics {
                    contentDescription = label
                    stateDescription = percent
                },
            )
        }
    }
}

private const val HALF = 50

package org.playdrop.android.ui.device

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.VolumeUp
import androidx.compose.material.icons.filled.Pause
import androidx.compose.material.icons.filled.PlayArrow
import androidx.compose.material.icons.filled.SkipNext
import androidx.compose.material.icons.filled.SkipPrevious
import androidx.compose.material3.Card
import androidx.compose.material3.FilledIconButton
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Slider
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import kotlinx.coroutines.flow.flowOf
import org.playdrop.android.ui.common.nowMsTicker
import org.playdrop.android.R
import org.playdrop.core.MediaAction
import org.playdrop.core.MediaState

/** One "Now playing" card for a player on the peer. [onCommand] takes an action and its value. */
@Composable
fun NowPlayingCard(state: MediaState, onCommand: (MediaAction, Long?) -> Unit) {
    // A one-second clock while playing; a paused player needs no ticks.
    val ticks = remember(state.playing, state.positionAtMs) {
        if (state.playing) nowMsTicker(TICK_MS) else flowOf(System.currentTimeMillis())
    }
    val now by ticks.collectAsStateWithLifecycle(System.currentTimeMillis())
    val dash = stringResource(R.string.ui_media_missing)
    Card(Modifier.fillMaxWidth()) {
        Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
            Text(
                state.name?.takeIf { it.isNotBlank() } ?: state.player,
                style = MaterialTheme.typography.labelLarge,
                color = MaterialTheme.colorScheme.primary,
            )
            Text(
                state.title?.takeIf { it.isNotBlank() } ?: dash,
                style = MaterialTheme.typography.titleMedium,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
            Text(
                state.artist?.takeIf { it.isNotBlank() } ?: dash,
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
            Progress(state, now, onCommand)
            Row(
                Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.Center,
                verticalAlignment = Alignment.CenterVertically,
            ) {
                if (MediaAction.PREVIOUS in state.can) {
                    IconButton(onClick = { onCommand(MediaAction.PREVIOUS, null) }) {
                        Icon(Icons.Filled.SkipPrevious, contentDescription = stringResource(R.string.ui_media_previous))
                    }
                }
                NowPlaying.toggleAction(state)?.let { action ->
                    FilledIconButton(onClick = { onCommand(action, null) }) {
                        if (state.playing) {
                            Icon(Icons.Filled.Pause, contentDescription = stringResource(R.string.ui_media_pause))
                        } else {
                            Icon(Icons.Filled.PlayArrow, contentDescription = stringResource(R.string.ui_media_play))
                        }
                    }
                }
                if (MediaAction.NEXT in state.can) {
                    IconButton(onClick = { onCommand(MediaAction.NEXT, null) }) {
                        Icon(Icons.Filled.SkipNext, contentDescription = stringResource(R.string.ui_media_next))
                    }
                }
            }
            if (MediaAction.VOLUME in state.can) Volume(state, onCommand)
        }
    }
}

@Composable
private fun Progress(state: MediaState, now: Long, onCommand: (MediaAction, Long?) -> Unit) {
    val duration = state.durationMs?.toLong()?.takeIf { it > 0 } ?: return
    val position = NowPlaying.positionMs(state, now) ?: 0L
    val fraction = NowPlaying.fraction(state, now) ?: 0f
    // While the thumb is held, show where it is instead of the extrapolated position.
    var dragging by remember(state.player) { mutableStateOf<Float?>(null) }
    if (MediaAction.SEEK in state.can) {
        Slider(
            value = dragging ?: fraction,
            onValueChange = { dragging = it },
            onValueChangeFinished = {
                dragging?.let { onCommand(MediaAction.SEEK, (it * duration).toLong()) }
                dragging = null
            },
        )
    } else {
        LinearProgressIndicator(progress = { fraction }, modifier = Modifier.fillMaxWidth().padding(vertical = 8.dp))
    }
    val shown = dragging?.let { (it * duration).toLong() } ?: position
    Row(Modifier.fillMaxWidth()) {
        Text(NowPlaying.clock(shown), style = MaterialTheme.typography.labelSmall, modifier = Modifier.weight(1f))
        Text(NowPlaying.clock(duration), style = MaterialTheme.typography.labelSmall)
    }
}

@Composable
private fun Volume(state: MediaState, onCommand: (MediaAction, Long?) -> Unit) {
    var dragging by remember(state.player) { mutableStateOf<Float?>(null) }
    val current = (state.volume?.toInt() ?: 0).toFloat()
    Row(verticalAlignment = Alignment.CenterVertically) {
        Icon(
            Icons.AutoMirrored.Filled.VolumeUp,
            contentDescription = stringResource(R.string.ui_media_volume),
            tint = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Slider(
            value = dragging ?: current,
            onValueChange = { dragging = it },
            onValueChangeFinished = {
                dragging?.let { onCommand(MediaAction.VOLUME, it.toLong().coerceIn(0, 100)) }
                dragging = null
            },
            valueRange = 0f..100f,
            modifier = Modifier.padding(start = 8.dp),
        )
    }
}

private const val TICK_MS = 1_000L

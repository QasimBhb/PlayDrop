package org.playdrop.android.ui.common

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material.icons.filled.CloudOff
import androidx.compose.material3.Surface
import androidx.compose.material3.TextButton
import androidx.compose.ui.Alignment
import androidx.compose.foundation.layout.size
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Battery1Bar
import androidx.compose.material.icons.filled.Battery3Bar
import androidx.compose.material.icons.filled.Battery5Bar
import androidx.compose.material.icons.filled.BatteryAlert
import androidx.compose.material.icons.filled.BatteryChargingFull
import androidx.compose.material.icons.filled.BatteryFull
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.flow.Flow
import org.playdrop.android.R
import org.playdrop.android.ui.theme.MonoStyle

/** A Short ID in monospace (SR-8). */
@Composable
fun ShortIdText(shortId: String, modifier: Modifier = Modifier) {
    Text(
        text = shortId,
        style = MonoStyle,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
        modifier = modifier,
    )
}

/** "just now", "5 min ago", "never" from an [Ago]. */
@Composable
fun agoText(ago: Ago): String = when (ago) {
    Ago.JustNow -> stringResource(R.string.ui_ago_just_now)
    is Ago.Minutes -> pluralStringResource(R.plurals.ui_ago_minutes, ago.n, ago.n)
    is Ago.Hours -> pluralStringResource(R.plurals.ui_ago_hours, ago.n, ago.n)
    is Ago.Days -> pluralStringResource(R.plurals.ui_ago_days, ago.n, ago.n)
    Ago.Never -> stringResource(R.string.ui_ago_never)
}

/** A small round status dot. */
@Composable
fun StatusDot(color: Color, modifier: Modifier = Modifier) {
    Box(modifier.size(10.dp).clip(CircleShape).background(color))
}

/** The battery glyph for a level, charging or low. */
fun batteryIcon(level: Int, charging: Boolean): ImageVector = when {
    charging -> Icons.Filled.BatteryChargingFull
    level <= 15 -> Icons.Filled.BatteryAlert
    level <= 35 -> Icons.Filled.Battery1Bar
    level <= 60 -> Icons.Filled.Battery3Bar
    level <= 85 -> Icons.Filled.Battery5Bar
    else -> Icons.Filled.BatteryFull
}

/** Battery icon plus percent. */
@Composable
fun BatteryLabel(level: Int, charging: Boolean) {
    Icon(
        batteryIcon(level, charging),
        contentDescription = null,
        modifier = Modifier.size(16.dp),
        tint = MaterialTheme.colorScheme.onSurfaceVariant,
    )
    Text(
        text = stringResource(R.string.ui_percent, level),
        style = MaterialTheme.typography.labelMedium,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
    )
}

/** Shows every [UiMessage] from [messages] through [show] as long as the caller is composed. */
@Composable
fun CollectMessages(messages: Flow<UiMessage>, show: (String) -> Unit) {
    val context = LocalContext.current
    LaunchedEffect(messages) {
        messages.collect { show(it.resolve(context)) }
    }
}

/** A section title inside a scrolling screen. */
@Composable
fun SectionTitle(text: String, modifier: Modifier = Modifier) {
    Text(
        text = text,
        style = MaterialTheme.typography.titleSmall,
        color = MaterialTheme.colorScheme.primary,
        modifier = modifier,
    )
}

/** "Service not running" with a Start button. */
@Composable
fun ServiceBanner(onStart: () -> Unit) {
    Surface(color = MaterialTheme.colorScheme.errorContainer, modifier = Modifier.fillMaxWidth()) {
        Row(Modifier.padding(start = 16.dp, end = 8.dp, top = 4.dp, bottom = 4.dp), verticalAlignment = Alignment.CenterVertically) {
            Icon(Icons.Filled.CloudOff, contentDescription = null, tint = MaterialTheme.colorScheme.onErrorContainer)
            Text(
                stringResource(R.string.ui_service_not_running),
                color = MaterialTheme.colorScheme.onErrorContainer,
                style = MaterialTheme.typography.bodyMedium,
                modifier = Modifier.padding(start = 12.dp).weight(1f),
            )
            TextButton(onClick = onStart) { Text(stringResource(R.string.ui_start)) }
        }
    }
}

package org.playdrop.android.ui.help

import android.content.ActivityNotFoundException
import android.content.Context
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilledTonalButton
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.tooling.preview.Preview
import androidx.compose.ui.unit.dp
import org.playdrop.android.R
import org.playdrop.android.ui.theme.PlayDropTheme

/**
 * "Can't find your computer?": four situations in plain words, most common first. [topic]
 * marks the card that matches a diagnostics finding. [showMessage] reports a settings screen
 * that could not be opened (anti-pattern 18).
 */
@Composable
fun CantFindRoute(
    topic: HelpTopic?,
    onBack: () -> Unit,
    onPairByQrOrAddress: () -> Unit,
    showMessage: (String) -> Unit,
) {
    val context = LocalContext.current
    CantFindScreen(
        topic = topic,
        onBack = onBack,
        onPairByQrOrAddress = onPairByQrOrAddress,
        onOpenTetherSettings = {
            if (!openTetherSettings(context)) showMessage(context.getString(R.string.ui_help_settings_missing))
        },
    )
}

/** Starts the first tether settings screen the phone has; false when none opens. */
internal fun openTetherSettings(context: Context): Boolean {
    for (intent in TetherSettings.intents()) {
        try {
            context.startActivity(intent)
            return true
        } catch (_: ActivityNotFoundException) {
            // Try the next, more general screen.
        } catch (_: SecurityException) {
            // A vendor screen that is not exported; same fallback.
        }
    }
    return false
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun CantFindScreen(
    topic: HelpTopic?,
    onBack: () -> Unit,
    onPairByQrOrAddress: () -> Unit,
    onOpenTetherSettings: () -> Unit,
) {
    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text(stringResource(R.string.ui_help_title)) },
                navigationIcon = {
                    IconButton(onClick = onBack) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = stringResource(R.string.ui_back))
                    }
                },
            )
        },
    ) { padding ->
        Column(
            Modifier.padding(padding).fillMaxSize().verticalScroll(rememberScrollState()).padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            Text(stringResource(R.string.ui_help_intro), style = MaterialTheme.typography.bodyMedium)
            HelpCard(
                R.string.ui_help_same_title,
                R.string.ui_help_same_body,
                highlighted = topic == HelpTopic.SAME_NETWORK,
            ) {
                Button(onClick = onPairByQrOrAddress) { Text(stringResource(R.string.ui_help_same_action)) }
            }
            HelpCard(
                R.string.ui_help_hotspot_title,
                R.string.ui_help_hotspot_body,
                highlighted = topic == HelpTopic.OTHER_NETWORK,
            ) {
                FilledTonalButton(onClick = onOpenTetherSettings) { Text(stringResource(R.string.ui_help_hotspot_action)) }
            }
            HelpCard(R.string.ui_help_usb_title, R.string.ui_help_usb_body, highlighted = false) {
                FilledTonalButton(onClick = onOpenTetherSettings) { Text(stringResource(R.string.ui_help_usb_action)) }
            }
            HelpCard(R.string.ui_help_vpn_title, R.string.ui_help_vpn_body, highlighted = false) {}
        }
    }
}

@Composable
private fun HelpCard(title: Int, body: Int, highlighted: Boolean, action: @Composable () -> Unit) {
    Card(
        modifier = Modifier.fillMaxWidth(),
        colors = CardDefaults.cardColors(
            containerColor = if (highlighted) MaterialTheme.colorScheme.secondaryContainer else MaterialTheme.colorScheme.surfaceContainer,
        ),
    ) {
        Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            if (highlighted) {
                Text(
                    stringResource(R.string.ui_help_start_here),
                    style = MaterialTheme.typography.labelLarge,
                    color = MaterialTheme.colorScheme.primary,
                )
            }
            Text(stringResource(title), style = MaterialTheme.typography.titleMedium)
            Text(stringResource(body), style = MaterialTheme.typography.bodyMedium)
            action()
        }
    }
}

@Preview(showBackground = true, heightDp = 1000)
@Composable
private fun CantFindPreview() {
    PlayDropTheme { CantFindScreen(HelpTopic.OTHER_NETWORK, {}, {}, {}) }
}

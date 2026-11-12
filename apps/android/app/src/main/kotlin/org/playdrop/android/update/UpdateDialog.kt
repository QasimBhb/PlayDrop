package org.playdrop.android.update

import android.content.ActivityNotFoundException
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.provider.Settings as AndroidSettings
import android.widget.Toast
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.DialogProperties
import org.playdrop.android.R
import org.playdrop.android.ui.common.Format
import org.playdrop.core.UpdateOffer

/** Callbacks of [UpdateDialog]. */
data class UpdateActions(
    val onUpdate: () -> Unit = {},
    val onLater: () -> Unit = {},
    val onDismiss: () -> Unit = {},
)

/**
 * The update offer and the steps after "Update", over every screen. Nothing happens without
 * a tap on Update (SR-47), and no button is a default destructive action: tapping outside
 * does nothing, Back is "Later" on the offer and "Cancel" while downloading.
 */
@Composable
fun UpdateDialog(state: UpdateState, actions: UpdateActions) {
    val context = LocalContext.current
    val fixed = DialogProperties(dismissOnClickOutside = false)
    when (state) {
        is UpdateState.Available -> AlertDialog(
            onDismissRequest = actions.onLater,
            properties = fixed,
            title = { Text(stringResource(R.string.update_available_title, state.offer.version)) },
            text = {
                Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                    Text(stringResource(if (state.offer.asset != null) R.string.update_available_body else R.string.update_available_no_file))
                    TextButton(onClick = { openPage(context, state.offer) }) { Text(stringResource(R.string.update_whats_new)) }
                }
            },
            confirmButton = {
                if (state.offer.asset != null) TextButton(onClick = actions.onUpdate) { Text(stringResource(R.string.update_update)) }
            },
            dismissButton = { TextButton(onClick = actions.onLater) { Text(stringResource(R.string.update_later)) } },
        )
        is UpdateState.NeedsPermission -> AlertDialog(
            onDismissRequest = actions.onDismiss,
            properties = fixed,
            title = { Text(stringResource(R.string.update_permission_title)) },
            text = { Text(stringResource(R.string.update_permission_body)) },
            confirmButton = {
                TextButton(onClick = { openInstallSetting(context) }) { Text(stringResource(R.string.update_permission_open)) }
            },
            dismissButton = { TextButton(onClick = actions.onDismiss) { Text(stringResource(R.string.update_cancel)) } },
        )
        is UpdateState.Downloading -> AlertDialog(
            onDismissRequest = actions.onDismiss,
            properties = fixed,
            title = { Text(stringResource(R.string.update_downloading_title, state.offer.version)) },
            text = {
                Column(Modifier.fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                    if (state.total > 0) {
                        LinearProgressIndicator(progress = { Format.fraction(state.done, state.total) }, modifier = Modifier.fillMaxWidth())
                        Text(
                            stringResource(R.string.update_downloading_progress, Format.bytes(state.done), Format.bytes(state.total)),
                            style = MaterialTheme.typography.bodySmall,
                        )
                    } else {
                        LinearProgressIndicator(modifier = Modifier.fillMaxWidth())
                        Text(stringResource(R.string.update_downloading_bytes, Format.bytes(state.done)), style = MaterialTheme.typography.bodySmall)
                    }
                }
            },
            confirmButton = { TextButton(onClick = actions.onDismiss) { Text(stringResource(R.string.update_cancel)) } },
        )
        is UpdateState.Verifying -> AlertDialog(
            onDismissRequest = {},
            properties = DialogProperties(dismissOnClickOutside = false, dismissOnBackPress = false),
            title = { Text(stringResource(R.string.update_downloading_title, state.offer.version)) },
            text = {
                Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                    LinearProgressIndicator(modifier = Modifier.fillMaxWidth())
                    Text(stringResource(R.string.update_verifying))
                }
            },
            confirmButton = {},
        )
        is UpdateState.HandingOver -> AlertDialog(
            onDismissRequest = actions.onDismiss,
            properties = fixed,
            title = { Text(stringResource(R.string.update_available_title, state.offer.version)) },
            text = { Text(stringResource(R.string.update_handing_over)) },
            confirmButton = { TextButton(onClick = actions.onDismiss) { Text(stringResource(R.string.update_ok)) } },
        )
        is UpdateState.Failed -> AlertDialog(
            onDismissRequest = actions.onDismiss,
            title = {
                Text(stringResource(if (state.offer == null) R.string.update_check_failed_title else R.string.update_failed_title))
            },
            text = {
                Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                    Text(stringResource(state.failure.sentence))
                    state.offer?.let { offer ->
                        TextButton(onClick = { openPage(context, offer) }) { Text(stringResource(R.string.update_whats_new)) }
                    }
                }
            },
            confirmButton = { TextButton(onClick = actions.onDismiss) { Text(stringResource(R.string.update_ok)) } },
        )
        UpdateState.Idle, UpdateState.UpToDate, is UpdateState.Checking -> Unit
    }
}

/** Opens the release page in the browser. */
private fun openPage(context: Context, offer: UpdateOffer) {
    try {
        context.startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(offer.pageUrl)))
    } catch (_: ActivityNotFoundException) {
        Toast.makeText(context, offer.pageUrl, Toast.LENGTH_LONG).show()
    }
}

/** Opens "Install unknown apps" for PlayDrop; the app goes on when the person comes back. */
private fun openInstallSetting(context: Context) {
    val intent = Intent(AndroidSettings.ACTION_MANAGE_UNKNOWN_APP_SOURCES, Uri.parse("package:" + context.packageName))
    try {
        context.startActivity(intent)
    } catch (_: ActivityNotFoundException) {
        Toast.makeText(context, R.string.update_permission_unavailable, Toast.LENGTH_LONG).show()
    }
}

package org.playdrop.android.ui.device

import android.content.ClipDescription
import android.content.ClipboardManager
import android.content.Context
import android.net.Uri
import android.os.Build
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.PickVisualMediaRequest
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.ContentPaste
import androidx.compose.material.icons.filled.Description
import androidx.compose.material.icons.filled.Image
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.FilledTonalButton
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import org.playdrop.android.R
import org.playdrop.android.service.ClipboardApplier

/** Callbacks of [SendActions]. */
data class SendCallbacks(
    /** Files or media were picked; send them to this device. */
    val onPicked: (List<Uri>) -> Unit = {},
    /** The clipboard was read on a tap. */
    val onClipboard: (ClipRead) -> Unit = {},
    /** The user agreed to send a clip marked sensitive. */
    val onConfirmSensitive: () -> Unit = {},
    /** The user backed out of sending a sensitive clip. */
    val onCancelSensitive: () -> Unit = {},
    /** Show the Transfers tab. */
    val onSeeProgress: () -> Unit = {},
    /** Hide the confirmation card. */
    val onDismissNotice: () -> Unit = {},
)

/**
 * The three send buttons at the top of the device page, the confirmation after a pick and
 * the sensitive-clip question. The pickers are the system ones; nothing is sent until the
 * user has chosen items.
 */
@Composable
fun SendActions(state: DeviceDetailUiState, callbacks: SendCallbacks) {
    val context = LocalContext.current
    val documents = rememberLauncherForActivityResult(ActivityResultContracts.OpenMultipleDocuments()) {
        if (it.isNotEmpty()) callbacks.onPicked(it)
    }
    val media = rememberLauncherForActivityResult(ActivityResultContracts.PickMultipleVisualMedia()) {
        if (it.isNotEmpty()) callbacks.onPicked(it)
    }
    val filesEnabled = DeviceSend.canSendFiles(state.row)
    Column(Modifier.fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(8.dp)) {
        state.notice?.let { SendNoticeCard(it, callbacks) }
        SendButton(R.string.ui_device_send_files, Icons.Filled.Description, filesEnabled) { documents.launch(arrayOf("*/*")) }
        SendButton(R.string.ui_device_send_media, Icons.Filled.Image, filesEnabled) {
            media.launch(PickVisualMediaRequest(ActivityResultContracts.PickVisualMedia.ImageAndVideo))
        }
        SendButton(
            R.string.ui_device_send_clipboard,
            Icons.Filled.ContentPaste,
            DeviceSend.canSendClipboard(state.row, state.sendingClipboard),
        ) {
            // With the device's clipboard switch off nothing is read: the plan says why.
            callbacks.onClipboard(if (state.grants.clipboardOut) readClipboard(context) else ClipRead.Empty)
        }
        if (!filesEnabled) {
            Text(
                stringResource(R.string.ui_send_removed),
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }
    if (state.askSensitive) {
        AlertDialog(
            onDismissRequest = callbacks.onCancelSensitive,
            title = { Text(stringResource(R.string.sensitive_title)) },
            text = { Text(stringResource(R.string.sensitive_text)) },
            confirmButton = { TextButton(onClick = callbacks.onConfirmSensitive) { Text(stringResource(R.string.sensitive_send)) } },
            dismissButton = { TextButton(onClick = callbacks.onCancelSensitive) { Text(stringResource(R.string.cancel)) } },
        )
    }
}

@Composable
private fun SendButton(label: Int, icon: androidx.compose.ui.graphics.vector.ImageVector, enabled: Boolean, onClick: () -> Unit) {
    FilledTonalButton(
        onClick = onClick,
        enabled = enabled,
        modifier = Modifier.fillMaxWidth().heightIn(min = 48.dp),
    ) {
        Icon(icon, contentDescription = null, modifier = Modifier.padding(end = 8.dp).size(20.dp))
        Text(stringResource(label))
    }
}

/** "Sending 3 files to calliope" with a way to the Transfers tab. */
@Composable
private fun SendNoticeCard(notice: SendNotice, callbacks: SendCallbacks) {
    Card(
        colors = CardDefaults.cardColors(containerColor = MaterialTheme.colorScheme.secondaryContainer),
        modifier = Modifier.fillMaxWidth().semantics { liveRegion = LiveRegionMode.Polite },
    ) {
        Column(Modifier.padding(start = 16.dp, top = 12.dp, end = 8.dp, bottom = 4.dp)) {
            Text(
                pluralStringResource(R.plurals.ui_send_started, notice.count, notice.count, notice.label),
                color = MaterialTheme.colorScheme.onSecondaryContainer,
                style = MaterialTheme.typography.bodyLarge,
            )
            if (notice.offline) {
                Text(
                    stringResource(R.string.ui_send_offline, notice.label),
                    color = MaterialTheme.colorScheme.onSecondaryContainer,
                    style = MaterialTheme.typography.bodyMedium,
                )
            }
            Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
                TextButton(onClick = callbacks.onSeeProgress, modifier = Modifier.heightIn(min = 48.dp)) {
                    Text(stringResource(R.string.ui_send_progress))
                }
                TextButton(onClick = callbacks.onDismissNotice, modifier = Modifier.heightIn(min = 48.dp)) {
                    Text(stringResource(R.string.ui_send_dismiss))
                }
            }
        }
    }
}

/**
 * Reads the clipboard. Only the focused app may (Android 10+), which holds here because the
 * user just tapped a button. Mirrors the notification action: first item, an image kept as
 * [ClipRead.Image] (its bytes are read only when it is sent, off the main thread, capped by
 * `ClipImage`), the sensitive marker honoured. Only the item's own text counts: a clip that
 * is a link to some other content is never opened here (that would read an unbounded stream
 * on the main thread), so it reads as empty.
 */
fun readClipboard(context: Context): ClipRead {
    val cm = context.getSystemService(ClipboardManager::class.java)
    val clip = try {
        cm.primaryClip
    } catch (e: SecurityException) {
        null
    }
    if (clip == null || clip.itemCount == 0) return ClipRead.Empty
    val desc = clip.description
    val item = clip.getItemAt(0)
    val sensitive = desc.extras?.let { extras ->
        extras.getBoolean(ClipboardApplier.EXTRA_IS_SENSITIVE, false) ||
            (Build.VERSION.SDK_INT >= 33 && extras.getBoolean(ClipDescription.EXTRA_IS_SENSITIVE, false))
    } ?: false
    val imageMime = desc.takeIf { it.mimeTypeCount > 0 }?.getMimeType(0)?.takeIf { it.startsWith("image/") }
    if (item.uri != null && imageMime != null) return ClipRead.Image(item.uri.toString(), imageMime, sensitive)
    val text = item.text?.toString()?.takeIf { it.isNotEmpty() } ?: return ClipRead.Empty
    return ClipRead.Text(text, sensitive)
}

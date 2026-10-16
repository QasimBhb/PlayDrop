package org.playdrop.android.ui.transfers

import android.app.DownloadManager
import android.content.ActivityNotFoundException
import android.content.Context
import android.content.Intent
import android.net.Uri
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Download
import androidx.compose.material.icons.filled.SwapVert
import androidx.compose.material.icons.filled.Upload
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilledTonalButton
import androidx.compose.material3.Icon
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.tooling.preview.Preview
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import org.playdrop.android.R
import org.playdrop.android.core.FileNames
import org.playdrop.android.ui.common.CollectMessages
import org.playdrop.android.ui.common.Format
import org.playdrop.android.ui.common.Samples
import org.playdrop.android.ui.theme.PlayDropTheme
import org.playdrop.core.TransferStatus

/** The Transfers tab wired to the activity-scoped view model and the system viewers. */
@Composable
fun TransfersRoute(vm: TransfersViewModel, showMessage: (String) -> Unit) {
    val rows by vm.rows.collectAsStateWithLifecycle()
    val context = LocalContext.current
    CollectMessages(vm.messages, showMessage)
    TransfersScreen(
        rows = rows,
        onCancel = vm::cancel,
        onRespond = vm::respond,
        onOpen = { row ->
            val uri = row.openUri ?: return@TransfersScreen
            val opened = if (isRisky(context, uri, row.openName)) {
                showMessage(context.getString(R.string.ui_transfers_risky))
                openDownloads(context)
            } else {
                openFile(context, uri)
            }
            if (!opened) showMessage(context.getString(R.string.ui_transfers_no_viewer))
        },
        onOpenFolder = {
            if (!openDownloads(context)) showMessage(context.getString(R.string.ui_transfers_no_viewer))
        },
    )
}

/** Live and recent transfers with Cancel, Accept and Decline, and Open for received files. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun TransfersScreen(
    rows: List<TransferRow>,
    onCancel: (String) -> Unit,
    onRespond: (String, Boolean) -> Unit,
    onOpen: (TransferRow) -> Unit,
    onOpenFolder: () -> Unit,
) {
    Scaffold(topBar = { TopAppBar(title = { Text(stringResource(R.string.ui_transfers_title)) }) }) { padding ->
        if (rows.isEmpty()) {
            Column(
                Modifier.padding(padding).fillMaxSize().padding(32.dp),
                verticalArrangement = Arrangement.Center,
                horizontalAlignment = Alignment.CenterHorizontally,
            ) {
                Icon(Icons.Filled.SwapVert, contentDescription = null, tint = MaterialTheme.colorScheme.primary)
                Text(
                    stringResource(R.string.ui_transfers_empty),
                    textAlign = TextAlign.Center,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.padding(top = 12.dp),
                )
            }
        } else {
            LazyColumn(
                Modifier.padding(padding),
                contentPadding = PaddingValues(16.dp),
                verticalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                items(rows, key = { it.id }) { row ->
                    TransferCard(row, onCancel, onRespond, onOpen, onOpenFolder)
                }
            }
        }
    }
}

@Composable
private fun TransferCard(
    row: TransferRow,
    onCancel: (String) -> Unit,
    onRespond: (String, Boolean) -> Unit,
    onOpen: (TransferRow) -> Unit,
    onOpenFolder: () -> Unit,
) {
    Card(Modifier.fillMaxWidth()) {
        Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Icon(
                    if (row.receive) Icons.Filled.Download else Icons.Filled.Upload,
                    contentDescription = stringResource(if (row.receive) R.string.ui_transfers_incoming else R.string.ui_transfers_outgoing),
                    tint = MaterialTheme.colorScheme.primary,
                )
                Text(
                    stringResource(if (row.receive) R.string.ui_transfers_from else R.string.ui_transfers_to, row.peerLabel),
                    style = MaterialTheme.typography.titleMedium,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                    modifier = Modifier.padding(start = 8.dp).weight(1f),
                )
                Text(stateText(row), style = MaterialTheme.typography.labelLarge, color = stateColor(row.state))
            }
            val files = if (row.fileCount > 0) {
                pluralStringResource(R.plurals.ui_files, row.fileCount.toInt(), Format.count(row.fileCount)) + ", " + Format.bytes(row.total)
            } else {
                Format.bytes(row.total)
            }
            Text(files, style = MaterialTheme.typography.bodyMedium)
            if (row.offeredNames.isNotEmpty()) {
                val more = if (row.moreNames > 0) " " + stringResource(R.string.ui_transfers_more, row.moreNames.toInt()) else ""
                Text(row.offeredNames.joinToString(", ") + more, style = MaterialTheme.typography.bodySmall, maxLines = 3, overflow = TextOverflow.Ellipsis)
            }
            if (row.state == TransferStatus.ACTIVE) {
                LinearProgressIndicator(progress = { row.fraction }, modifier = Modifier.fillMaxWidth())
                val detail = buildString {
                    append(stringResource(R.string.ui_transfers_of, Format.bytes(row.bytes), Format.bytes(row.total)))
                    if (row.showFilesDone) {
                        append(" · ").append(
                            stringResource(
                                R.string.ui_transfers_files_done,
                                Format.count(row.filesDone),
                                Format.count(row.fileCount),
                            ),
                        )
                    }
                    if (row.bytesPerSec > 0) append(" · ").append(Format.rate(row.bytesPerSec))
                    row.etaSecs?.let { append(" · ").append(stringResource(R.string.ui_transfers_left, Format.duration(it))) }
                }
                Text(detail, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
            }
            row.note?.takeIf { it.isNotBlank() }?.let {
                Text(it, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
            }
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                if (row.canRespond) {
                    Button(onClick = { onRespond(row.id, true) }) { Text(stringResource(R.string.ui_transfers_accept)) }
                    OutlinedButton(onClick = { onRespond(row.id, false) }) { Text(stringResource(R.string.ui_transfers_decline)) }
                }
                if (row.canCancel) {
                    TextButton(onClick = { onCancel(row.id) }) { Text(stringResource(R.string.ui_cancel)) }
                }
                if (row.openUri != null) {
                    FilledTonalButton(onClick = { onOpen(row) }) { Text(stringResource(R.string.ui_transfers_open)) }
                }
                if (row.showFolder) {
                    TextButton(onClick = onOpenFolder) { Text(stringResource(R.string.ui_transfers_open_folder)) }
                }
            }
        }
    }
}

@Composable
private fun stateText(row: TransferRow): String = when (row.state) {
    TransferStatus.OFFERED -> stringResource(if (row.receive) R.string.ui_state_waiting_you else R.string.ui_state_waiting_peer)
    TransferStatus.ACTIVE -> stringResource(R.string.ui_percent, (row.fraction * 100).toInt())
    TransferStatus.COMPLETE -> stringResource(R.string.ui_state_done)
    TransferStatus.FAILED -> stringResource(R.string.ui_state_failed)
    TransferStatus.CANCELLED -> stringResource(R.string.ui_state_cancelled)
}

@Composable
private fun stateColor(state: TransferStatus) = when (state) {
    TransferStatus.FAILED -> MaterialTheme.colorScheme.error
    TransferStatus.OFFERED -> MaterialTheme.colorScheme.tertiary
    else -> MaterialTheme.colorScheme.onSurfaceVariant
}

/** True when the delivered file could install or run code if opened (SR-23). */
private fun isRisky(context: Context, location: String, name: String?): Boolean {
    val mime = runCatching { context.contentResolver.getType(Uri.parse(location)) }.getOrNull()
    return FileNames.isRiskyToOpen(name?.substringAfterLast('/') ?: "", mime)
}

/** Opens a delivered content URI in the app the user picks; false when none can. */
fun openFile(context: Context, location: String): Boolean {
    val uri = Uri.parse(location)
    val intent = Intent(Intent.ACTION_VIEW).apply {
        setDataAndType(uri, context.contentResolver.getType(uri) ?: "*/*")
        addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_ACTIVITY_NEW_TASK)
    }
    return try {
        context.startActivity(intent)
        true
    } catch (_: ActivityNotFoundException) {
        false
    } catch (_: SecurityException) {
        false
    }
}

/** Opens the system Downloads app, where received files land. */
fun openDownloads(context: Context): Boolean = try {
    context.startActivity(Intent(DownloadManager.ACTION_VIEW_DOWNLOADS).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
    true
} catch (_: ActivityNotFoundException) {
    false
}

@Preview(showBackground = true)
@Composable
private fun TransfersPreview() {
    PlayDropTheme { TransfersScreen(Samples.transferRows, {}, { _, _ -> }, {}, {}) }
}

@Preview(showBackground = true)
@Composable
private fun TransfersEmptyPreview() {
    PlayDropTheme { TransfersScreen(emptyList(), {}, { _, _ -> }, {}, {}) }
}

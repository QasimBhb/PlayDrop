package org.playdrop.android.ui.device

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.Edit
import androidx.compose.material.icons.filled.ExpandLess
import androidx.compose.material.icons.filled.ExpandMore
import androidx.compose.material.icons.filled.Mouse
import androidx.compose.material.icons.filled.Warning
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilledTonalButton
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.ListItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.tooling.preview.Preview
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import org.playdrop.android.R
import org.playdrop.android.ui.common.BatteryLabel
import org.playdrop.android.ui.common.CollectMessages
import org.playdrop.android.ui.common.Samples
import org.playdrop.android.ui.common.SectionTitle
import org.playdrop.android.ui.common.ShortIdText
import org.playdrop.android.ui.common.agoText
import org.playdrop.android.ui.theme.MonoStyle
import org.playdrop.android.ui.theme.PlayDropTheme
import org.playdrop.core.MediaAction
import java.text.DateFormat
import java.util.Date

/** The Device detail screen wired to its view model. */
@Composable
fun DeviceDetailRoute(
    onBack: () -> Unit,
    onPairAgain: () -> Unit,
    onOpenInput: () -> Unit,
    showMessage: (String) -> Unit,
    vm: DeviceDetailViewModel = viewModel(),
) {
    val state by vm.state.collectAsStateWithLifecycle()
    CollectMessages(vm.messages, showMessage)
    LaunchedEffect(vm) { vm.closed.collect { onBack() } }
    DeviceDetailScreen(
        state = state,
        actions = DeviceDetailActions(
            onBack = onBack,
            onRename = vm::rename,
            onSetDefault = vm::setDefault,
            onPing = vm::ping,
            onToggleFind = vm::toggleFind,
            onConnect = vm::connect,
            onGrant = vm::setGrant,
            onUnpair = vm::unpair,
            onPairAgain = onPairAgain,
            onOpenInput = onOpenInput,
            onMedia = vm::mediaCommand,
        ),
    )
}

/** Callbacks of [DeviceDetailScreen]. */
data class DeviceDetailActions(
    /** Up navigation. */
    val onBack: () -> Unit = {},
    /** New local label. */
    val onRename: (String) -> Unit = {},
    /** Make this the default device. */
    val onSetDefault: () -> Unit = {},
    /** Ping with an optional message. */
    val onPing: (String?) -> Unit = {},
    /** Start or stop ringing. */
    val onToggleFind: () -> Unit = {},
    /** Dial now. */
    val onConnect: () -> Unit = {},
    /** Change a grant. */
    val onGrant: (GrantKind, Boolean) -> Unit = { _, _ -> },
    /** Unpair after confirmation. */
    val onUnpair: () -> Unit = {},
    /** Open "Add device" to pair again. */
    val onPairAgain: () -> Unit = {},
    /** Open the remote input screen. */
    val onOpenInput: () -> Unit = {},
    /** Media command: player ID, action, seek ms or volume. */
    val onMedia: (String, MediaAction, Long?) -> Unit = { _, _, _ -> },
)

/** One paired device: actions, grants, addresses and unpair. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun DeviceDetailScreen(state: DeviceDetailUiState, actions: DeviceDetailActions) {
    var renaming by rememberSaveable { mutableStateOf(false) }
    var pinging by rememberSaveable { mutableStateOf(false) }
    var confirmUnpair by rememberSaveable { mutableStateOf(false) }
    var showAddresses by rememberSaveable { mutableStateOf(false) }
    val row = state.row
    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text(row?.label ?: stringResource(R.string.ui_detail_title)) },
                navigationIcon = {
                    IconButton(onClick = actions.onBack) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = stringResource(R.string.ui_back))
                    }
                },
                actions = {
                    if (row != null) {
                        IconButton(onClick = { renaming = true }) {
                            Icon(Icons.Filled.Edit, contentDescription = stringResource(R.string.ui_detail_rename))
                        }
                    }
                },
            )
        },
    ) { padding ->
        if (row == null) {
            Text(
                stringResource(R.string.ui_detail_gone),
                modifier = Modifier.padding(padding).padding(24.dp),
            )
            return@Scaffold
        }
        Column(
            Modifier.padding(padding).fillMaxSize().verticalScroll(rememberScrollState()).padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            if (row.removedByPeer) {
                Card(colors = CardDefaults.cardColors(containerColor = MaterialTheme.colorScheme.errorContainer)) {
                    Row(Modifier.padding(16.dp), verticalAlignment = Alignment.CenterVertically) {
                        Icon(Icons.Filled.Warning, contentDescription = null, tint = MaterialTheme.colorScheme.onErrorContainer)
                        Text(
                            stringResource(R.string.ui_detail_removed),
                            color = MaterialTheme.colorScheme.onErrorContainer,
                            modifier = Modifier.padding(start = 12.dp).weight(1f),
                        )
                        TextButton(onClick = actions.onPairAgain) { Text(stringResource(R.string.ui_detail_pair_again)) }
                    }
                }
            }
            Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                ShortIdText(row.shortId)
                Spacer(Modifier.weight(1f))
                val level = row.batteryLevel
                if (level != null && row.online) BatteryLabel(level, row.charging)
            }
            Text(
                when {
                    row.asleep -> stringResource(R.string.ui_device_asleep)
                    row.online -> stringResource(R.string.ui_device_online)
                    else -> stringResource(R.string.ui_device_last_seen, agoText(row.lastSeen))
                } + "  ·  " + stringResource(
                    R.string.ui_detail_paired_on,
                    DateFormat.getDateInstance(DateFormat.MEDIUM).format(Date(state.pairedAt * 1000)),
                ),
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )

            FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                if (!row.isDefault) {
                    FilledTonalButton(onClick = actions.onSetDefault) { Text(stringResource(R.string.ui_detail_set_default)) }
                }
                FilledTonalButton(onClick = { pinging = true }) { Text(stringResource(R.string.ui_detail_ping)) }
                FilledTonalButton(onClick = actions.onToggleFind) {
                    Text(stringResource(if (state.ringing) R.string.ui_detail_find_stop else R.string.ui_detail_find))
                }
                if (!row.online) {
                    OutlinedButton(onClick = actions.onConnect) { Text(stringResource(R.string.ui_detail_connect)) }
                }
                if (state.canControl) {
                    FilledTonalButton(onClick = actions.onOpenInput) {
                        Icon(Icons.Filled.Mouse, contentDescription = null, modifier = Modifier.padding(end = 8.dp))
                        Text(stringResource(R.string.ui_detail_input))
                    }
                }
            }
            if (row.online && !state.canControl) {
                // Say why the feature is missing instead of hiding it silently (anti-pattern 18).
                ListItem(
                    headlineContent = { Text(stringResource(R.string.ui_detail_input)) },
                    supportingContent = { Text(stringResource(R.string.ui_detail_input_unsupported)) },
                    leadingContent = { Icon(Icons.Filled.Mouse, contentDescription = null) },
                    modifier = Modifier.alpha(DISABLED_ALPHA),
                )
            }
            if (state.ringing) {
                Text(
                    stringResource(R.string.ui_detail_ringing),
                    color = MaterialTheme.colorScheme.primary,
                    style = MaterialTheme.typography.bodyMedium,
                )
            }
            if (row.isDefault) {
                Text(
                    stringResource(R.string.ui_detail_is_default),
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }

            if (state.media.isNotEmpty()) {
                HorizontalDivider()
                SectionTitle(stringResource(R.string.ui_media_title))
                state.media.forEach { player ->
                    NowPlayingCard(player) { action, value -> actions.onMedia(player.player, action, value) }
                }
            }

            HorizontalDivider()
            SectionTitle(stringResource(R.string.ui_grants_title))
            GRANT_ROWS.forEach { (kind, title, body) ->
                ListItem(
                    headlineContent = { Text(stringResource(title)) },
                    supportingContent = { Text(stringResource(body)) },
                    trailingContent = {
                        Switch(
                            checked = GrantEdit.get(state.grants, kind),
                            onCheckedChange = { actions.onGrant(kind, it) },
                        )
                    },
                )
            }
            // GrantKind.INPUT would let the device control this phone, which a phone never
            // accepts, so it gets a note instead of a toggle that does nothing.
            Text(
                stringResource(R.string.ui_grant_input_phone),
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(horizontal = 16.dp),
            )

            HorizontalDivider()
            Row(
                Modifier.fillMaxWidth().clickable { showAddresses = !showAddresses }.padding(vertical = 8.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                SectionTitle(stringResource(R.string.ui_detail_addresses), Modifier.weight(1f))
                Icon(if (showAddresses) Icons.Filled.ExpandLess else Icons.Filled.ExpandMore, contentDescription = null)
            }
            if (showAddresses) {
                if (state.lastKnown.isEmpty()) {
                    Text(stringResource(R.string.ui_detail_no_addresses), style = MaterialTheme.typography.bodyMedium)
                } else {
                    state.lastKnown.forEach { Text(it, style = MonoStyle) }
                }
            }

            HorizontalDivider()
            OutlinedButton(onClick = { confirmUnpair = true }) {
                Text(stringResource(R.string.ui_detail_unpair), color = MaterialTheme.colorScheme.error)
            }
            Spacer(Modifier.size(24.dp))
        }
    }

    if (renaming && row != null) {
        TextInputDialog(
            title = stringResource(R.string.ui_detail_rename_title),
            body = stringResource(R.string.ui_detail_rename_body),
            initial = row.label,
            confirm = stringResource(R.string.ui_detail_rename),
            onDismiss = { renaming = false },
            onConfirm = {
                renaming = false
                actions.onRename(it)
            },
            allowEmpty = false,
        )
    }
    if (pinging) {
        TextInputDialog(
            title = stringResource(R.string.ui_detail_ping),
            body = stringResource(R.string.ui_detail_ping_body),
            initial = "",
            confirm = stringResource(R.string.ui_detail_ping_send),
            onDismiss = { pinging = false },
            onConfirm = {
                pinging = false
                actions.onPing(it)
            },
            allowEmpty = true,
        )
    }
    if (confirmUnpair && row != null) {
        AlertDialog(
            onDismissRequest = { confirmUnpair = false },
            title = { Text(stringResource(R.string.ui_detail_unpair_title, row.label)) },
            text = {
                Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                    Text(stringResource(R.string.ui_detail_unpair_body))
                    ShortIdText(row.shortId)
                }
            },
            confirmButton = {
                TextButton(onClick = {
                    confirmUnpair = false
                    actions.onUnpair()
                }) { Text(stringResource(R.string.ui_detail_unpair), color = MaterialTheme.colorScheme.error) }
            },
            dismissButton = { TextButton(onClick = { confirmUnpair = false }) { Text(stringResource(R.string.ui_cancel)) } },
        )
    }
}

/** A dialog with one text field. */
@Composable
fun TextInputDialog(
    title: String,
    body: String,
    initial: String,
    confirm: String,
    onDismiss: () -> Unit,
    onConfirm: (String) -> Unit,
    allowEmpty: Boolean,
) {
    var text by rememberSaveable { mutableStateOf(initial) }
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(title) },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                Text(body, style = MaterialTheme.typography.bodyMedium)
                OutlinedTextField(value = text, onValueChange = { text = it.take(MAX_INPUT) }, singleLine = true)
            }
        },
        confirmButton = {
            TextButton(onClick = { onConfirm(text) }, enabled = allowEmpty || text.isNotBlank()) { Text(confirm) }
        },
        dismissButton = { TextButton(onClick = onDismiss) { Text(stringResource(R.string.ui_cancel)) } },
    )
}

private const val MAX_INPUT = 64

private data class GrantRow(val kind: GrantKind, val title: Int, val body: Int)

private val GRANT_ROWS = listOf(
    GrantRow(GrantKind.FILES, R.string.ui_grant_files, R.string.ui_grant_files_body),
    GrantRow(GrantKind.CLIPBOARD_IN, R.string.ui_grant_clip_in, R.string.ui_grant_clip_in_body),
    GrantRow(GrantKind.CLIPBOARD_OUT, R.string.ui_grant_clip_out, R.string.ui_grant_clip_out_body),
    GrantRow(GrantKind.MEDIA, R.string.ui_grant_media, R.string.ui_grant_media_body),
    GrantRow(GrantKind.FIND, R.string.ui_grant_find, R.string.ui_grant_find_body),
)

private const val DISABLED_ALPHA = 0.6f

@Preview(showBackground = true, heightDp = 1100)
@Composable
private fun DeviceDetailPreview() {
    PlayDropTheme {
        DeviceDetailScreen(Samples.detailState, DeviceDetailActions())
    }
}

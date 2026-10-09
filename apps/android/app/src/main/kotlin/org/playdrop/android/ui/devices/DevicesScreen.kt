package org.playdrop.android.ui.devices

import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.PickVisualMediaRequest
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.Devices
import androidx.compose.material.icons.filled.Star
import androidx.compose.material.icons.automirrored.filled.Send
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.Checkbox
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.ExtendedFloatingActionButton
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.ListItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.tooling.preview.Preview
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import org.playdrop.android.R
import org.playdrop.android.ui.common.BatteryLabel
import org.playdrop.android.ui.common.CollectMessages
import org.playdrop.android.ui.common.ServiceBanner
import org.playdrop.android.ui.common.ShortIdText
import org.playdrop.android.ui.common.StatusDot
import org.playdrop.android.ui.common.agoText
import org.playdrop.android.ui.common.Samples
import org.playdrop.android.ui.theme.OnlineGreen
import org.playdrop.android.ui.theme.PlayDropTheme

/** The Devices tab wired to its view model and the photo picker. */
@Composable
fun DevicesRoute(
    onOpenDevice: (String) -> Unit,
    onAddDevice: () -> Unit,
    showMessage: (String) -> Unit,
    vm: DevicesViewModel = viewModel(),
) {
    val state by vm.state.collectAsStateWithLifecycle()
    CollectMessages(vm.messages, showMessage)
    var pendingTarget by rememberSaveable { mutableStateOf<String?>(null) }
    val picker = rememberLauncherForActivityResult(ActivityResultContracts.PickMultipleVisualMedia()) { uris ->
        val target = pendingTarget
        pendingTarget = null
        if (target != null) vm.send(target, uris)
    }
    DevicesScreen(
        state = state,
        onOpenDevice = onOpenDevice,
        onAddDevice = onAddDevice,
        onStartService = vm::startService,
        onSetDefault = vm::setDefault,
        onSend = { peer ->
            pendingTarget = peer
            picker.launch(PickVisualMediaRequest(ActivityResultContracts.PickVisualMedia.ImageAndVideo))
        },
    )
}

/** The Devices tab: one card per paired device, the service banner and the one-tap send button. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun DevicesScreen(
    state: DevicesUiState,
    onOpenDevice: (String) -> Unit,
    onAddDevice: () -> Unit,
    onStartService: () -> Unit,
    onSetDefault: (String) -> Unit,
    onSend: (String) -> Unit,
) {
    var choosing by rememberSaveable { mutableStateOf(false) }
    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text(stringResource(R.string.ui_devices_title)) },
                actions = {
                    IconButton(onClick = onAddDevice) {
                        Icon(Icons.Filled.Add, contentDescription = stringResource(R.string.ui_add_device))
                    }
                },
            )
        },
        floatingActionButton = {
            if (state.rows.isNotEmpty()) {
                ExtendedFloatingActionButton(
                    onClick = {
                        val target = state.sendTarget
                        if (target != null) onSend(target) else choosing = true
                    },
                    icon = { Icon(Icons.AutoMirrored.Filled.Send, contentDescription = null) },
                    text = { Text(stringResource(R.string.ui_send_files)) },
                )
            }
        },
    ) { padding ->
        Column(Modifier.padding(padding).fillMaxSize()) {
            if (!state.running) ServiceBanner(onStart = onStartService)
            if (state.rows.isEmpty()) {
                EmptyDevices(onAddDevice)
            } else {
                LazyColumn(
                    contentPadding = PaddingValues(start = 16.dp, end = 16.dp, top = 8.dp, bottom = 96.dp),
                    verticalArrangement = Arrangement.spacedBy(8.dp),
                ) {
                    items(state.rows, key = { it.deviceId }) { row ->
                        DeviceCard(row, onClick = { onOpenDevice(row.deviceId) })
                    }
                }
            }
        }
    }
    if (choosing) {
        ChooseTargetDialog(
            rows = state.rows,
            onDismiss = { choosing = false },
            onChosen = { id, remember ->
                choosing = false
                if (remember) onSetDefault(id)
                onSend(id)
            },
        )
    }
}

/** One paired device. */
@Composable
fun DeviceCard(row: DeviceRow, onClick: () -> Unit) {
    Card(
        modifier = Modifier.fillMaxWidth().clickable(onClick = onClick),
        colors = CardDefaults.cardColors(),
    ) {
        Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                StatusDot(
                    if (row.online && !row.asleep) OnlineGreen else MaterialTheme.colorScheme.outline,
                )
                Spacer(Modifier.width(8.dp))
                Text(
                    row.label,
                    style = MaterialTheme.typography.titleMedium,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                    modifier = Modifier.weight(1f, fill = false),
                )
                Spacer(Modifier.width(8.dp))
                ShortIdText(row.shortId)
                Spacer(Modifier.weight(1f))
                if (row.isDefault) {
                    Icon(
                        Icons.Filled.Star,
                        contentDescription = stringResource(R.string.ui_default_device),
                        tint = MaterialTheme.colorScheme.primary,
                        modifier = Modifier.size(20.dp),
                    )
                }
            }
            Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                val status = when {
                    row.removedByPeer -> stringResource(R.string.ui_device_removed_short)
                    row.asleep -> stringResource(R.string.ui_device_asleep)
                    row.online -> stringResource(R.string.ui_device_online)
                    else -> stringResource(R.string.ui_device_last_seen, agoText(row.lastSeen))
                }
                Text(status, style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
                val level = row.batteryLevel
                if (level != null && row.online) {
                    Spacer(Modifier.width(8.dp))
                    BatteryLabel(level, row.charging)
                }
            }
        }
    }
}

@Composable
private fun EmptyDevices(onAddDevice: () -> Unit) {
    Column(
        Modifier.fillMaxSize().padding(32.dp),
        verticalArrangement = Arrangement.Center,
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        Icon(Icons.Filled.Devices, contentDescription = null, modifier = Modifier.size(56.dp), tint = MaterialTheme.colorScheme.primary)
        Spacer(Modifier.size(16.dp))
        Text(stringResource(R.string.ui_devices_empty_title), style = MaterialTheme.typography.titleLarge, textAlign = TextAlign.Center)
        Spacer(Modifier.size(8.dp))
        Text(
            stringResource(R.string.ui_devices_empty_body),
            style = MaterialTheme.typography.bodyMedium,
            textAlign = TextAlign.Center,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Spacer(Modifier.size(24.dp))
        Button(onClick = onAddDevice) { Text(stringResource(R.string.ui_add_device)) }
    }
}

@Composable
private fun ChooseTargetDialog(rows: List<DeviceRow>, onDismiss: () -> Unit, onChosen: (String, Boolean) -> Unit) {
    var remember by rememberSaveable { mutableStateOf(true) }
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(stringResource(R.string.ui_send_to_title)) },
        text = {
            Column {
                rows.forEach { row ->
                    ListItem(
                        headlineContent = { Text(row.label) },
                        supportingContent = { ShortIdText(row.shortId) },
                        modifier = Modifier.clickable { onChosen(row.deviceId, remember) },
                    )
                }
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Checkbox(checked = remember, onCheckedChange = { remember = it })
                    Text(stringResource(R.string.ui_send_to_remember))
                }
            }
        },
        confirmButton = {},
        dismissButton = { TextButton(onClick = onDismiss) { Text(stringResource(R.string.ui_cancel)) } },
    )
}

@Preview(showBackground = true)
@Composable
private fun DevicesPreview() {
    PlayDropTheme {
        DevicesScreen(Samples.devicesState, {}, {}, {}, {}, {})
    }
}

@Preview(showBackground = true)
@Composable
private fun DevicesEmptyPreview() {
    PlayDropTheme {
        DevicesScreen(DevicesUiState(running = false), {}, {}, {}, {}, {})
    }
}

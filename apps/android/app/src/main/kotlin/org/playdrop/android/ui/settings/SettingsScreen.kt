package org.playdrop.android.ui.settings

import android.Manifest
import android.annotation.SuppressLint
import android.content.ActivityNotFoundException
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Build
import android.os.PowerManager
import android.provider.Settings as AndroidSettings
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.KeyboardArrowRight
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Card
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilledTonalButton
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.ListItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.RadioButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Switch
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
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.tooling.preview.Preview
import androidx.compose.ui.unit.dp
import androidx.core.content.ContextCompat
import androidx.lifecycle.compose.LifecycleResumeEffect
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import org.playdrop.android.R
import org.playdrop.android.ui.common.CollectMessages
import org.playdrop.android.ui.common.Samples
import org.playdrop.android.ui.common.SectionTitle
import org.playdrop.android.ui.common.ShortIdText
import org.playdrop.android.ui.theme.MonoStyle
import org.playdrop.android.ui.theme.PlayDropTheme

/** What the platform reports, refreshed every time the screen resumes. */
data class PlatformState(
    /** PlayDrop is exempt from battery optimisation. */
    val batteryExempt: Boolean = false,
    /** Notifications may be posted (always true before Android 13). */
    val notificationsAllowed: Boolean = true,
)

/** The Settings tab wired to its view model and the system settings screens. */
@Composable
fun SettingsRoute(onOpenDiagnostics: () -> Unit, showMessage: (String) -> Unit, vm: SettingsViewModel = viewModel()) {
    val state by vm.state.collectAsStateWithLifecycle()
    val context = LocalContext.current
    CollectMessages(vm.messages, showMessage)
    var platform by rememberSaveable(stateSaver = PlatformSaver) { mutableStateOf(readPlatform(context)) }
    LifecycleResumeEffect(Unit) {
        platform = readPlatform(context)
        onPauseOrDispose {}
    }
    val notifications = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) {
        platform = readPlatform(context)
    }
    SettingsScreen(
        state = state,
        platform = platform,
        actions = SettingsActions(
            onSaveName = vm::saveName,
            onSavePort = vm::savePort,
            onSetDefault = vm::setDefault,
            onAutoClipboard = vm::setAutoClipboard,
            onBattery = {
                vm.markBatteryPromptShown()
                if (!requestBatteryExemption(context)) showMessage(context.getString(R.string.ui_settings_battery_unavailable))
            },
            onNotifications = {
                if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
                    notifications.launch(Manifest.permission.POST_NOTIFICATIONS)
                }
            },
            onDiagnostics = onOpenDiagnostics,
        ),
    )
}

/** Callbacks of [SettingsScreen]. */
data class SettingsActions(
    /** Save the device name. */
    val onSaveName: (String) -> Unit = {},
    /** Save the port. */
    val onSavePort: (String) -> Unit = {},
    /** Choose the default device (null clears). */
    val onSetDefault: (String?) -> Unit = {},
    /** Toggle clipboard auto-push. */
    val onAutoClipboard: (Boolean) -> Unit = {},
    /** Ask for the battery exemption (only from this tap, anti-pattern 14). */
    val onBattery: () -> Unit = {},
    /** Ask for the notification permission. */
    val onNotifications: () -> Unit = {},
    /** Open Diagnostics. */
    val onDiagnostics: () -> Unit = {},
)

/** Device name, default device, port, clipboard, battery, notifications, diagnostics and about. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SettingsScreen(state: SettingsUiState, platform: PlatformState, actions: SettingsActions) {
    val values = state.values
    Scaffold(topBar = { TopAppBar(title = { Text(stringResource(R.string.ui_settings_title)) }) }) { padding ->
        if (values == null) return@Scaffold
        var name by rememberSaveable(values.deviceName) { mutableStateOf(values.deviceName) }
        var port by rememberSaveable(values.port) { mutableStateOf(values.port.toString()) }
        var choosingDefault by rememberSaveable { mutableStateOf(false) }
        Column(
            Modifier.padding(padding).fillMaxSize().verticalScroll(rememberScrollState()).padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            SectionTitle(stringResource(R.string.ui_settings_name))
            Row(verticalAlignment = Alignment.CenterVertically) {
                OutlinedTextField(
                    value = name,
                    onValueChange = { name = it.take(64) },
                    singleLine = true,
                    modifier = Modifier.weight(1f),
                )
                TextButton(onClick = { actions.onSaveName(name) }, enabled = name.trim() != values.deviceName && name.isNotBlank()) {
                    Text(stringResource(R.string.ui_save))
                }
            }
            Hint(stringResource(R.string.ui_settings_name_hint))

            HorizontalDivider()
            SectionTitle(stringResource(R.string.ui_settings_default))
            val current = state.peers.firstOrNull { it.deviceId == values.defaultPeer }
            ListItem(
                headlineContent = { Text(current?.label ?: stringResource(R.string.ui_settings_default_none)) },
                supportingContent = { current?.let { ShortIdText(it.shortId) } ?: Text(stringResource(R.string.ui_settings_default_hint)) },
                trailingContent = { Icon(Icons.AutoMirrored.Filled.KeyboardArrowRight, contentDescription = null) },
                modifier = Modifier.clickable(enabled = state.peers.isNotEmpty()) { choosingDefault = true },
            )

            HorizontalDivider()
            SectionTitle(stringResource(R.string.ui_settings_clipboard))
            ListItem(
                headlineContent = { Text(stringResource(R.string.ui_settings_auto_clipboard)) },
                supportingContent = { Text(stringResource(R.string.ui_settings_auto_clipboard_hint)) },
                trailingContent = { Switch(checked = values.autoClipboard, onCheckedChange = actions.onAutoClipboard) },
            )

            HorizontalDivider()
            SectionTitle(stringResource(R.string.ui_settings_background))
            Card {
                Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                    Text(stringResource(R.string.ui_settings_battery_title), style = MaterialTheme.typography.titleSmall)
                    Text(
                        stringResource(if (platform.batteryExempt) R.string.ui_settings_battery_done else R.string.ui_settings_battery_body),
                        style = MaterialTheme.typography.bodyMedium,
                    )
                    if (!platform.batteryExempt) {
                        FilledTonalButton(onClick = actions.onBattery) { Text(stringResource(R.string.ui_settings_battery_button)) }
                    }
                }
            }
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
                ListItem(
                    headlineContent = { Text(stringResource(R.string.ui_settings_notifications)) },
                    supportingContent = {
                        Text(
                            stringResource(
                                if (platform.notificationsAllowed) R.string.ui_settings_notifications_on else R.string.ui_settings_notifications_off,
                            ),
                        )
                    },
                    trailingContent = {
                        if (!platform.notificationsAllowed) {
                            TextButton(onClick = actions.onNotifications) { Text(stringResource(R.string.ui_allow)) }
                        }
                    },
                )
            }

            HorizontalDivider()
            SectionTitle(stringResource(R.string.ui_settings_network))
            Row(verticalAlignment = Alignment.CenterVertically) {
                OutlinedTextField(
                    value = port,
                    onValueChange = { port = it.filter(Char::isDigit).take(5) },
                    singleLine = true,
                    label = { Text(stringResource(R.string.ui_settings_port)) },
                    keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number),
                    modifier = Modifier.weight(1f),
                )
                TextButton(onClick = { actions.onSavePort(port) }, enabled = port != values.port.toString()) {
                    Text(stringResource(R.string.ui_save))
                }
            }
            Hint(stringResource(R.string.ui_settings_port_hint))
            ListItem(
                headlineContent = { Text(stringResource(R.string.ui_diagnostics_title)) },
                supportingContent = { Text(stringResource(R.string.ui_settings_diagnostics_hint)) },
                trailingContent = { Icon(Icons.AutoMirrored.Filled.KeyboardArrowRight, contentDescription = null) },
                modifier = Modifier.clickable(onClick = actions.onDiagnostics),
            )

            HorizontalDivider()
            SectionTitle(stringResource(R.string.ui_settings_about))
            val id = state.identity
            if (id != null) {
                Text(stringResource(R.string.ui_settings_short_id), style = MaterialTheme.typography.labelMedium)
                ShortIdText(id.shortId)
                Text(stringResource(R.string.ui_settings_device_id), style = MaterialTheme.typography.labelMedium)
                Text(id.deviceId, style = MonoStyle)
            } else {
                Hint(stringResource(R.string.ui_settings_identity_unknown))
            }
            if (state.coreVersion.isNotEmpty()) Hint(stringResource(R.string.ui_settings_core_version, state.coreVersion))
            Hint(stringResource(R.string.ui_settings_licence))
            Spacer(Modifier.size(24.dp))
        }
        if (choosingDefault) {
            AlertDialog(
                onDismissRequest = { choosingDefault = false },
                title = { Text(stringResource(R.string.ui_settings_default)) },
                text = {
                    Column {
                        state.peers.forEach { p ->
                            Row(
                                Modifier.fillMaxWidth().clickable {
                                    choosingDefault = false
                                    actions.onSetDefault(p.deviceId)
                                },
                                verticalAlignment = Alignment.CenterVertically,
                            ) {
                                RadioButton(selected = p.deviceId == values.defaultPeer, onClick = null)
                                Column(Modifier.padding(start = 8.dp, top = 8.dp, bottom = 8.dp)) {
                                    Text(p.label)
                                    ShortIdText(p.shortId)
                                }
                            }
                        }
                    }
                },
                confirmButton = {
                    TextButton(onClick = { choosingDefault = false }) { Text(stringResource(R.string.ui_cancel)) }
                },
                dismissButton = {
                    if (values.defaultPeer != null) {
                        TextButton(onClick = {
                            choosingDefault = false
                            actions.onSetDefault(null)
                        }) { Text(stringResource(R.string.ui_settings_default_clear)) }
                    }
                },
            )
        }
    }
}

@Composable
private fun Hint(text: String) {
    Text(text, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
}

/** Reads battery exemption and notification permission. */
fun readPlatform(context: Context): PlatformState {
    val pm = context.getSystemService(PowerManager::class.java)
    val exempt = pm?.isIgnoringBatteryOptimizations(context.packageName) ?: false
    val notifications = Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU ||
        ContextCompat.checkSelfPermission(context, Manifest.permission.POST_NOTIFICATIONS) == PackageManager.PERMISSION_GRANTED
    return PlatformState(exempt, notifications)
}

/**
 * Asks Android to stop optimising PlayDrop's battery use. Called only from the user's tap on
 * the battery card (anti-pattern 14). Uses the direct request when the manifest declares
 * `REQUEST_IGNORE_BATTERY_OPTIMIZATIONS`, otherwise opens the system list. False when
 * neither screen exists.
 */
@SuppressLint("BatteryLife")
fun requestBatteryExemption(context: Context): Boolean {
    val declared = ContextCompat.checkSelfPermission(
        context,
        Manifest.permission.REQUEST_IGNORE_BATTERY_OPTIMIZATIONS,
    ) == PackageManager.PERMISSION_GRANTED
    val direct = Intent(AndroidSettings.ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS, Uri.parse("package:" + context.packageName))
    val list = Intent(AndroidSettings.ACTION_IGNORE_BATTERY_OPTIMIZATION_SETTINGS)
    for (intent in if (declared) listOf(direct, list) else listOf(list)) {
        try {
            context.startActivity(intent)
            return true
        } catch (_: ActivityNotFoundException) {
            // Try the next screen.
        }
    }
    return false
}

private val PlatformSaver = androidx.compose.runtime.saveable.Saver<PlatformState, List<Boolean>>(
    save = { listOf(it.batteryExempt, it.notificationsAllowed) },
    restore = { PlatformState(it[0], it[1]) },
)

@Preview(showBackground = true, heightDp = 1400)
@Composable
private fun SettingsPreview() {
    PlayDropTheme { SettingsScreen(Samples.settingsState, PlatformState(), SettingsActions()) }
}


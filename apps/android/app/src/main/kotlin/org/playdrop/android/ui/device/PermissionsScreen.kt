package org.playdrop.android.ui.device

import android.content.ActivityNotFoundException
import android.content.Context
import android.content.Intent
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.Notifications
import androidx.compose.material.icons.filled.NotificationsOff
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.ListItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.tooling.preview.Preview
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.LifecycleResumeEffect
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import org.playdrop.android.R
import org.playdrop.android.service.NotificationMirror
import org.playdrop.android.ui.common.CollectMessages
import org.playdrop.android.ui.common.Samples
import org.playdrop.android.ui.common.SectionTitle
import org.playdrop.android.ui.theme.PlayDropTheme

/**
 * The Permissions page of one device, wired to the same [DeviceDetailViewModel] calls the
 * device page used before the switches moved here.
 */
@Composable
fun PermissionsRoute(
    onBack: () -> Unit,
    onChooseMirrorApps: () -> Unit,
    showMessage: (String) -> Unit,
    vm: DeviceDetailViewModel = viewModel(),
) {
    val state by vm.state.collectAsStateWithLifecycle()
    val context = LocalContext.current
    CollectMessages(vm.messages, showMessage)
    // Read again on every resume: the user grants or revokes access in system Settings.
    var notificationAccess by remember { mutableStateOf(NotificationMirror.hasAccess(context)) }
    LifecycleResumeEffect(Unit) {
        notificationAccess = NotificationMirror.hasAccess(context)
        onPauseOrDispose {}
    }
    PermissionsScreen(
        state = state,
        notificationAccess = notificationAccess,
        onBack = onBack,
        onGrant = vm::setGrant,
        onOpenNotificationAccess = {
            if (!openNotificationAccess(context)) showMessage(context.getString(R.string.ui_mirror_access_unavailable))
        },
        onChooseMirrorApps = onChooseMirrorApps,
    )
}

/** Opens the system list of notification listeners; false when this phone has no such screen. */
private fun openNotificationAccess(context: Context): Boolean = try {
    context.startActivity(Intent(android.provider.Settings.ACTION_NOTIFICATION_LISTENER_SETTINGS))
    true
} catch (e: ActivityNotFoundException) {
    false
}

/**
 * What one device may do on this phone: the "Allow this device to" switches and notification
 * mirroring. [notificationAccess]: PlayDrop holds "Notification access" in system Settings.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun PermissionsScreen(
    state: DeviceDetailUiState,
    notificationAccess: Boolean,
    onBack: () -> Unit,
    onGrant: (GrantKind, Boolean) -> Unit,
    onOpenNotificationAccess: () -> Unit,
    onChooseMirrorApps: () -> Unit,
) {
    val row = state.row
    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text(stringResource(R.string.ui_permissions_title)) },
                navigationIcon = {
                    IconButton(onClick = onBack) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = stringResource(R.string.ui_back))
                    }
                },
            )
        },
    ) { padding ->
        if (row == null) {
            Text(stringResource(R.string.ui_detail_gone), modifier = Modifier.padding(padding).padding(24.dp))
            return@Scaffold
        }
        Column(
            Modifier.padding(padding).fillMaxSize().verticalScroll(rememberScrollState()).padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            SectionTitle(stringResource(R.string.ui_grants_title))
            GRANT_ROWS.forEach { (kind, title, body) ->
                ListItem(
                    headlineContent = { Text(stringResource(title)) },
                    supportingContent = { Text(stringResource(body)) },
                    trailingContent = {
                        Switch(
                            checked = GrantEdit.get(state.grants, kind),
                            onCheckedChange = { onGrant(kind, it) },
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
            MirrorSection(
                deviceLabel = row.label,
                on = state.grants.notificationsOut,
                access = notificationAccess,
                apps = state.mirrorApps,
                onToggle = { onGrant(GrantKind.NOTIFICATIONS_OUT, it) },
                onOpenAccess = onOpenNotificationAccess,
                onChooseApps = onChooseMirrorApps,
            )
        }
    }
}

/**
 * Notification mirroring for one device: the `notificationsOut` toggle with what it shares,
 * a row saying why mirroring is off when PlayDrop has no notification access (anti-pattern
 * 18), and the way to the app picker.
 */
@Composable
private fun MirrorSection(
    deviceLabel: String,
    on: Boolean,
    access: Boolean,
    apps: Int,
    onToggle: (Boolean) -> Unit,
    onOpenAccess: () -> Unit,
    onChooseApps: () -> Unit,
) {
    SectionTitle(stringResource(R.string.ui_mirror_title))
    ListItem(
        headlineContent = { Text(stringResource(R.string.ui_mirror_toggle)) },
        supportingContent = { Text(stringResource(R.string.ui_mirror_body, deviceLabel)) },
        leadingContent = { Icon(Icons.Filled.Notifications, contentDescription = null) },
        trailingContent = { Switch(checked = on, onCheckedChange = onToggle) },
    )
    if (!access) {
        Card(colors = CardDefaults.cardColors(containerColor = MaterialTheme.colorScheme.secondaryContainer)) {
            Row(Modifier.padding(16.dp), verticalAlignment = Alignment.CenterVertically) {
                Icon(
                    Icons.Filled.NotificationsOff,
                    contentDescription = null,
                    tint = MaterialTheme.colorScheme.onSecondaryContainer,
                )
                Text(
                    stringResource(R.string.ui_mirror_no_access),
                    color = MaterialTheme.colorScheme.onSecondaryContainer,
                    style = MaterialTheme.typography.bodyMedium,
                    modifier = Modifier.padding(start = 12.dp).weight(1f),
                )
                TextButton(onClick = onOpenAccess) { Text(stringResource(R.string.ui_mirror_open_access)) }
            }
        }
    }
    Row(verticalAlignment = Alignment.CenterVertically, modifier = Modifier.fillMaxWidth()) {
        Text(
            if (apps == 0) {
                stringResource(R.string.ui_mirror_no_apps)
            } else {
                pluralStringResource(R.plurals.ui_mirror_apps_picked, apps, apps)
            },
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.padding(horizontal = 16.dp).weight(1f),
        )
        OutlinedButton(onClick = onChooseApps, modifier = Modifier.heightIn(min = 48.dp)) { Text(stringResource(R.string.ui_mirror_choose_apps)) }
    }
}

private data class GrantRow(val kind: GrantKind, val title: Int, val body: Int)

private val GRANT_ROWS = listOf(
    GrantRow(GrantKind.FILES, R.string.ui_grant_files, R.string.ui_grant_files_body),
    GrantRow(GrantKind.CLIPBOARD_IN, R.string.ui_grant_clip_in, R.string.ui_grant_clip_in_body),
    GrantRow(GrantKind.CLIPBOARD_OUT, R.string.ui_grant_clip_out, R.string.ui_grant_clip_out_body),
    GrantRow(GrantKind.MEDIA, R.string.ui_grant_media, R.string.ui_grant_media_body),
    GrantRow(GrantKind.FIND, R.string.ui_grant_find, R.string.ui_grant_find_body),
)


@Preview(showBackground = true, heightDp = 900)
@Composable
private fun PermissionsPreview() {
    PlayDropTheme {
        PermissionsScreen(Samples.detailState, notificationAccess = false, onBack = {}, onGrant = { _, _ -> }, onOpenNotificationAccess = {}, onChooseMirrorApps = {})
    }
}
